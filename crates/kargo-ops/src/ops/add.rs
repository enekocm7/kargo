//! `kargo add` and `kargo remove`.

use anyhow::{Context, Result, bail};

use crate::workspace::Workspace;
use kargo_manifest::edit::{DependencySection, ManifestEditor, NewDependency};
use kargo_manifest::{Manifest, Scope};
use kargo_maven::{Artifact, CoordSpec, MavenClient, VersionReq};
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
    if opts.specs.is_empty() {
        bail!("nothing to add; pass coordinates like `org.jetbrains.kotlinx:kotlinx-datetime`");
    }
    let client = MavenClient::new(config, ws.manifest().repositories.clone());
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
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
                let latest = client
                    .latest_version(&spec.module, opts.prerelease)
                    .with_context(|| format!("could not determine the latest version of `{}`", spec.module))?;
                latest.to_string()
            }
        };
        if !opts.no_verify && !version.starts_with(['[', '(']) {
            let pom = Artifact::new(spec.module.clone(), &version).with_extension("pom");
            let module = Artifact::new(spec.module.clone(), &version).with_extension("module");
            let exists = client.fetch(&pom.repo_path())?.is_some() || client.fetch(&module.repo_path())?.is_some();
            if !exists {
                bail!("`{}:{version}` was not found in any repository", spec.module);
            }
        }
        let dep = NewDependency { version: Some(version.clone()), scope: opts.scope, classifier: spec.classifier.clone(), bom: opts.bom };
        let replaced = editor.add_dependency(opts.section, &spec.module, &dep)?;
        let verb = if replaced { "Updating" } else { "Adding" };
        config.shell().status(verb, format!("{}:{version} to [{}]", spec.module, opts.section.display()));
        added.push(format!("{}:{version}", spec.module));
    }
    // Never write a manifest that no longer parses.
    Manifest::parse(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    Ok(added)
}

/// Enables built-in frameworks in `[frameworks]`.
pub fn add_frameworks(config: &Config, ws: &Workspace, names: &[String]) -> Result<()> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    for name in names {
        if !crate::builtins::FRAMEWORK_NAMES.contains(&name.as_str()) {
            bail!("unknown framework `{name}` (available: {})", crate::builtins::FRAMEWORK_NAMES.join(", "));
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
    Manifest::parse(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()
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
    Manifest::parse(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
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
