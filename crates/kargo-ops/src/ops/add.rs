//! `kargo add` and `kargo remove`.

use anyhow::{Context, Result, bail};

use std::path::{Component, Path, PathBuf};

use crate::workspace::{Project, Workspace, dunce_canonicalize};
use kargo_manifest::edit::{DependencySection, ManifestEditor, NewDependency};
use kargo_manifest::{MANIFEST_NAME, Manifest, Scope, TargetKind};
use kargo_maven::{Artifact, CoordSpec, MavenClient, Repository, VersionReq, select_latest};
use kargo_util::config::Config;

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub specs: Vec<String>,
    pub section: DependencySection,
    pub scope: Option<Scope>,
    pub bom: bool,
    /// Allow pre-release versions when picking the latest version.
    pub prerelease: bool,
    /// Skip checking that the artifact exists remotely.
    pub no_verify: bool,
}

/// Adds dependencies to the manifest, picking the latest release when no
/// version is given. Returns the coordinates written.
pub fn add(config: &Config, ws: &Workspace, opts: &AddOptions) -> Result<Vec<String>> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let added = add_to(config, ws.manifest().repositories.clone(), &mut editor, opts)?;
    // Never write a manifest that no longer parses.
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    for (coords, replaced) in &added {
        let verb = if *replaced { "Updating" } else { "Adding" };
        config.shell().status(verb, format!("{coords} to [{}]", opts.section.display()));
    }
    Ok(added.into_iter().map(|(coords, _)| coords).collect())
}

/// The edit of `kargo add`, made to `editor` without saving it: picks each dependency's version
/// from `repositories` and writes its entry. Returns the `group:artifact:version` of each, and
/// whether it replaced an entry.
pub fn add_to(
    config: &Config,
    repositories: Vec<Repository>,
    editor: &mut ManifestEditor,
    opts: &AddOptions,
) -> Result<Vec<(String, bool)>> {
    if opts.specs.is_empty() {
        bail!("nothing to add; pass coordinates like `org.jetbrains.kotlinx:kotlinx-datetime`");
    }
    let client = MavenClient::new(config, repositories);
    let mut added = Vec::new();
    for raw in &opts.specs {
        let spec: CoordSpec = raw.parse()?;
        if spec.extension.is_some() {
            bail!("`kargo add` does not support `@extension`; edit kargo.toml to set `extension`");
        }
        let version = match &spec.version {
            Some(v) => {
                v.parse::<VersionReq>().with_context(|| format!("invalid version in `{raw}`"))?;
                v.clone()
            }
            None => {
                let versions = client
                    .versions(&spec.module)
                    .with_context(|| format!("could not determine the latest version of `{}`", spec.module))?;
                if versions.is_empty() {
                    return Err(client.module_not_found(&spec.module));
                }
                let latest = select_latest(&versions, "", opts.prerelease)
                    .with_context(|| format!("could not determine the latest version of `{}`: no published releases", spec.module))?;
                // Variants such as `0.8.0-0.6.x-compat` can sort above the plain release; say which one was skipped.
                let newest = versions.iter().filter(|v| opts.prerelease || !v.is_prerelease()).max();
                if let Some(skipped) = newest.filter(|n| n.as_str() != latest.as_str() && *n >= latest) {
                    config.shell().note(format!(
                        "picked {latest} for `{}` over the variant {skipped}; pass `{}:{skipped}` to use the variant",
                        spec.module, spec.module
                    ));
                }
                latest.to_string()
            }
        };
        if !opts.no_verify && !version.starts_with(['[', '(']) {
            let pom = Artifact::new(spec.module.clone(), &version).with_extension("pom");
            let module = Artifact::new(spec.module.clone(), &version).with_extension("module");
            let exists = client.fetch(&client.artifact_path(&pom)?)?.is_some() || client.fetch(&client.artifact_path(&module)?)?.is_some();
            if !exists {
                return Err(client.version_not_found(&spec.module, &version));
            }
        }
        let dep = NewDependency { version: Some(version.clone()), scope: opts.scope, classifier: spec.classifier.clone(), bom: opts.bom };
        let replaced = editor.add_dependency(opts.section, &spec.module, &dep)?;
        added.push((format!("{}:{version}", spec.module), replaced));
    }
    Ok(added)
}

/// `kargo add --path <dir>`: depends on the package in `dir` (absolute, or relative to the current
/// directory). The entry's key is that package's name and its path is written relative to `ws`.
/// Returns the package name.
pub fn add_path(config: &Config, ws: &Workspace, dir: &Path, section: DependencySection, scope: Option<Scope>) -> Result<String> {
    let dir = dunce_canonicalize(dir);
    let manifest_path = dir.join(MANIFEST_NAME);
    if !manifest_path.is_file() {
        bail!("`{}` has no {MANIFEST_NAME}; `--path` takes the directory of a Kargo package", dir.display());
    }
    let text = std::fs::read_to_string(&manifest_path).with_context(|| format!("failed to read {}", manifest_path.display()))?;
    if kargo_manifest::is_workspace(&text).with_context(|| format!("failed to parse manifest at {}", manifest_path.display()))? {
        bail!("`{}` is a workspace, not a package; pass the directory of one of its members", dir.display());
    }
    let table: toml::Table = toml::from_str(&text).with_context(|| format!("failed to parse manifest at {}", manifest_path.display()))?;
    let name = table
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .with_context(|| format!("{} has no [package] `name`", manifest_path.display()))?
        .to_owned();
    if dir == ws.root() {
        bail!("`{name}` cannot depend on itself");
    }
    let relative = relative_path(ws.root(), &dir);
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let original = editor.contents();
    let replaced = editor.add_path_dependency(section, &name, &relative.to_string_lossy(), scope)?;
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    // Loading the project follows the new dependency, which catches cycles, name clashes and Kotlin version conflicts.
    if let Err(e) = Project::load(&ws.manifest_path()) {
        kargo_util::fs::write_string_atomic(&ws.manifest_path(), &original)?;
        return Err(e.context(format!("cannot add `{name}`; kargo.toml was not changed")));
    }
    let verb = if replaced { "Updating" } else { "Adding" };
    let shown = relative.to_string_lossy().replace('\\', "/");
    config.shell().status(verb, format!("{name} (path {shown}) to [{}]", section.display()));
    Ok(name)
}

/// `target` relative to the directory `base` (both canonical), or `target` itself when they
/// share no prefix (e.g. different drives on Windows).
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<Component> = base.components().collect();
    let target_parts: Vec<Component> = target.components().collect();
    let common = base.iter().zip(&target_parts).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for part in &target_parts[common..] {
        out.push(part);
    }
    out
}

/// Enables built-in frameworks in `[frameworks]`.
pub fn add_frameworks(config: &Config, ws: &Workspace, names: &[String]) -> Result<()> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    for name in names {
        if !crate::builtins::FRAMEWORK_NAMES.contains(&name.as_str()) {
            return Err(kargo_util::help::unknown(
                format!("unknown framework `{name}` (available: {})", crate::builtins::FRAMEWORK_NAMES.join(", ")),
                name,
                crate::builtins::FRAMEWORK_NAMES,
            ));
        }
        if name == "ktor" {
            bail!("ktor needs an engine; add `ktor = {{ server = \"netty\" }}` (or `client = \"cio\"`) under [frameworks]");
        }
        if editor.enable_framework(name)? {
            config.shell().status("Enabling", format!("framework `{name}`"));
        } else {
            config.shell().note(format!("framework `{name}` is already enabled"));
        }
    }
    let manifest = ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    if names.iter().any(|name| name == "benchmark") {
        scaffold_bench(config, ws, &manifest)?;
    }
    Ok(())
}

/// Writes `benches/SampleBench.kt` for a package that has no benchmarks yet.
fn scaffold_bench(config: &Config, ws: &Workspace, manifest: &Manifest) -> Result<()> {
    if !manifest.targets.is_enabled(TargetKind::Jvm) {
        config.shell().note("benchmarks run on the JVM target; add it with `kargo target add jvm`");
        return Ok(());
    }
    let benches = [ws.common_benches(), ws.target_benches(TargetKind::Jvm)];
    if benches.iter().any(|dir| !kargo_util::fs::collect_files(dir, &["kt", "java"]).is_empty()) {
        return Ok(());
    }
    let package = super::target::new_options(manifest, false).package();
    let path = ws.common_benches().join("SampleBench.kt");
    std::fs::create_dir_all(ws.common_benches()).with_context(|| format!("failed to create {}", ws.common_benches().display()))?;
    std::fs::write(&path, super::new::bench_source(&package)).with_context(|| format!("failed to write {}", path.display()))?;
    config.shell().status("Created", "benches/SampleBench.kt");
    Ok(())
}

/// `kargo add --cinterop <name>`: adds `[target.native.cinterop.<name>]` and, unless it exists,
/// `interop/<name>.def` binding `headers` (default `<name>.h`).
pub fn add_cinterop(config: &Config, ws: &Workspace, name: &str, headers: &[String], pkg_config: Option<&str>) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("invalid cinterop name `{name}` (use letters, digits, `_` and `-`)");
    }
    let package = name.replace('-', "_");
    let package = if package.starts_with(|c: char| c.is_ascii_digit()) { format!("c{package}") } else { package };
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    if !editor.add_cinterop(name, &package, pkg_config)? {
        bail!("[target.native.cinterop.{name}] already exists in kargo.toml");
    }
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    let def = ws.root().join("interop").join(format!("{name}.def"));
    let headers = if headers.is_empty() { vec![format!("{name}.h")] } else { headers.to_vec() };
    if def.exists() {
        config.shell().note(format!("keeping the existing {}", def.display()));
    } else {
        std::fs::create_dir_all(def.parent().expect("the def file has a directory"))?;
        let text = format!(
            "# The C library behind [target.native.cinterop.{name}] in kargo.toml.\n\
             # Keys: https://kotlinlang.org/docs/native-definition-file.html\n\
             headers = {}\n",
            headers.join(" ")
        );
        std::fs::write(&def, text).with_context(|| format!("failed to write {}", def.display()))?;
    }
    editor.save()?;
    config.shell().status("Adding", format!("C library `{name}` (package `{package}`, interop/{name}.def)"));
    Ok(())
}

pub fn remove(config: &Config, ws: &Workspace, names: &[String], section: DependencySection) -> Result<()> {
    if names.is_empty() {
        bail!("nothing to remove");
    }
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    for name in names {
        let key = editor.remove_dependency(section, name)?;
        config.shell().status("Removing", format!("{key} from [{}]", section.display()));
    }
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use kargo_manifest::Manifest;
    use kargo_util::shell::{Shell, Verbosity};

    fn local_repo(dir: &Path) {
        for v in ["1.0", "1.1", "2.0-RC1"] {
            let d = dir.join(format!("com/acme/widget/{v}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("widget-{v}.pom")), "<project><artifactId>widget</artifactId></project>").unwrap();
        }
        std::fs::write(
            dir.join("com/acme/widget/maven-metadata.xml"),
            "<metadata><versioning><versions><version>1.0</version><version>1.1</version><version>2.0-RC1</version></versions></versioning></metadata>",
        )
        .unwrap();
    }

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Config, Workspace) {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        local_repo(&repo);
        let url = format!("file://{}", repo.display()).replace('\\', "/");
        std::fs::write(
            project.path().join("kargo.toml"),
            format!("[package]\nname = \"p\"\nversion = \"1.0\"\n\n[repositories]\nlocal = \"{url}\"\n"),
        )
        .unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ws = Workspace::load(&project.path().join("kargo.toml")).unwrap();
        (project, home, config, ws)
    }

    fn opts(specs: &[&str]) -> AddOptions {
        AddOptions {
            specs: specs.iter().map(|s| s.to_string()).collect(),
            section: DependencySection::Main,
            scope: None,
            bom: false,
            prerelease: false,
            no_verify: false,
        }
    }

    #[test]
    fn adds_latest_release_and_explicit_versions() {
        let (_p, _h, config, ws) = setup();
        let added = add(&config, &ws, &opts(&["com.acme:widget"])).unwrap();
        assert_eq!(added, ["com.acme:widget:1.1"]);
        let m = Manifest::load(&ws.manifest_path()).unwrap();
        assert_eq!(m.dependencies[0].version.as_deref(), Some("1.1"));

        let mut pre = opts(&["com.acme:widget"]);
        pre.prerelease = true;
        assert_eq!(add(&config, &ws, &pre).unwrap(), ["com.acme:widget:2.0-RC1"]);

        assert_eq!(add(&config, &ws, &opts(&["com.acme:widget:1.0"])).unwrap(), ["com.acme:widget:1.0"]);
        let m = Manifest::load(&ws.manifest_path()).unwrap();
        assert_eq!(m.dependencies.len(), 1, "re-adding updates in place");
    }

    #[test]
    fn verifies_existence() {
        let (_p, _h, config, ws) = setup();
        let err = add(&config, &ws, &opts(&["com.acme:widget:9.9"])).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        let mut skip = opts(&["com.acme:widget:9.9"]);
        skip.no_verify = true;
        add(&config, &ws, &skip).unwrap();
        assert!(add(&config, &ws, &opts(&[])).is_err());
        assert!(add(&config, &ws, &opts(&["bad"])).is_err());
    }

    #[test]
    fn removes_dependencies() {
        let (_p, _h, config, ws) = setup();
        add(&config, &ws, &opts(&["com.acme:widget:1.0"])).unwrap();
        remove(&config, &ws, &["com.acme:widget".into()], DependencySection::Main).unwrap();
        assert!(Manifest::load(&ws.manifest_path()).unwrap().dependencies.is_empty());
        assert!(remove(&config, &ws, &["com.acme:widget".into()], DependencySection::Main).is_err());
    }

    #[test]
    fn enables_frameworks() {
        let (_p, _h, config, ws) = setup();
        add_frameworks(&config, &ws, &["serialization".into(), "coroutines".into()]).unwrap();
        let m = Manifest::load(&ws.manifest_path()).unwrap();
        assert!(m.frameworks.serialization.is_some() && m.frameworks.coroutines.is_some());
        assert!(add_frameworks(&config, &ws, &["nope".into()]).is_err());
        assert!(add_frameworks(&config, &ws, &["ktor".into()]).is_err());
    }
}
