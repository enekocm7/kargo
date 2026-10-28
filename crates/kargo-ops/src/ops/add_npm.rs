//! `kargo add --npm` and `kargo remove --npm`: npm packages in `[target.wasm.npm-dependencies]` and
//! `[target.js.npm-dependencies]` (or the `dev-npm-dependencies` next to them, for the tests).
//!
//! The edit is split from the loading like [`super::add::add_to`]: [`add_to`] changes a
//! [`ManifestEditor`] without saving it, so `kargo lsp` code actions run the same code as the command.

use anyhow::{Context, Result, bail};

use crate::dts;
use crate::npm_registry::{Package, Registry};
use crate::npm_semver::{Range, Semver};
use crate::workspace::Workspace;
use kargo_manifest::TargetKind;
use kargo_manifest::edit::{ManifestEditor, npm_table_key};
use kargo_util::config::Config;

#[derive(Debug, Clone, Default)]
pub struct NpmAddOptions {
    /// `name`, `name@1.2.3`, `name@^1.2` or `name@tag`; scoped names are `@scope/name[@version]`.
    pub specs: Vec<String>,
    /// Which target's table to write to; the project's only npm-capable target when `None`.
    pub target: Option<TargetKind>,
    /// Write to `dev-npm-dependencies`, the packages only the tests install.
    pub dev: bool,
    /// Also add `@types/<package>` when the package ships no declarations.
    pub types: bool,
    /// Allow pre-releases when picking the latest version.
    pub prerelease: bool,
    /// Do not ask the registry whether a given version exists.
    pub no_verify: bool,
}

/// An entry written to the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmChange {
    pub target: TargetKind,
    pub dev: bool,
    pub name: String,
    pub version: String,
    /// The version the entry had before.
    pub previous: Option<String>,
}

impl NpmChange {
    /// The table written to, such as `target.wasm.npm-dependencies`.
    pub fn table(&self) -> String {
        format!("target.{}.{}", self.target, npm_table_key(self.dev))
    }
}

/// `kargo add --npm`: adds the packages to `kargo.toml`, picking the latest release for the ones without a
/// version. Returns what was written.
pub fn add(config: &Config, ws: &Workspace, opts: &NpmAddOptions) -> Result<Vec<NpmChange>> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let changes = add_to(config, ws.root(), &mut editor, opts)?;
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    for change in &changes {
        match &change.previous {
            Some(previous) => {
                config.shell().status("Updating", format!("{} {previous} -> {} in [{}]", change.name, change.version, change.table()))
            }
            None => config.shell().status("Adding", format!("{} {} to [{}]", change.name, change.version, change.table())),
        }
    }
    Ok(changes)
}

/// The edit of `kargo add --npm`, made to `editor` without saving it. `project_dir` is where the project's
/// `.npmrc` is. Packages are written one by one, so a failure leaves the earlier ones in `editor` only.
pub fn add_to(config: &Config, project_dir: &std::path::Path, editor: &mut ManifestEditor, opts: &NpmAddOptions) -> Result<Vec<NpmChange>> {
    if opts.specs.is_empty() {
        bail!("nothing to add; pass npm package names like `left-pad` or `@js-joda/core@^3.2.0`");
    }
    let target = pick_target(&editor.targets(), opts.target)?;
    let registry = Registry::new(config, project_dir);
    let mut changes = Vec::new();
    for raw in &opts.specs {
        let (name, requested) = split_spec(raw)?;
        let chosen = resolve(config, &registry, &name, requested.as_deref(), opts)?;
        let previous = editor.add_npm_dependency(target, opts.dev, &name, &chosen.range)?;
        changes.push(NpmChange { target, dev: opts.dev, name: name.clone(), version: chosen.range.clone(), previous });
        if opts.types
            && let Some(types) = types_for(config, &registry, &name, &chosen)?
        {
            let previous = editor.add_npm_dependency(target, opts.dev, &types.0, &types.1)?;
            changes.push(NpmChange { target, dev: opts.dev, name: types.0, version: types.1, previous });
        }
    }
    Ok(changes)
}

/// `kargo remove --npm`: removes the packages (and the `@types` package `--types` added for each) from the
/// manifest of `ws`. Returns what was removed.
pub fn remove(config: &Config, ws: &Workspace, names: &[String], target: Option<TargetKind>, dev: bool) -> Result<Vec<NpmChange>> {
    if names.is_empty() {
        bail!("nothing to remove");
    }
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let mut removed = Vec::new();
    for name in names {
        let kind = target_declaring(&mut editor, target, dev, name)?;
        let version = editor.remove_npm_dependency(kind, dev, name)?;
        removed.push(NpmChange { target: kind, dev, name: name.clone(), version, previous: None });
        // The declarations `--types` added belong to the package.
        let types = format!("@types/{}", dts::types_name(name));
        if !name.starts_with("@types/") && editor.npm_dependency(kind, dev, &types).is_some() {
            let version = editor.remove_npm_dependency(kind, dev, &types)?;
            removed.push(NpmChange { target: kind, dev, name: types, version, previous: None });
        }
    }
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    for change in &removed {
        config.shell().status("Removing", format!("{} from [{}]", change.name, change.table()));
    }
    Ok(removed)
}

/// The target a package is removed from: `wanted`, else the only npm-capable target that declares it.
fn target_declaring(editor: &mut ManifestEditor, wanted: Option<TargetKind>, dev: bool, name: &str) -> Result<TargetKind> {
    let key = npm_table_key(dev);
    let candidates: Vec<TargetKind> = match wanted {
        Some(kind) => {
            check_npm_target(kind)?;
            vec![kind]
        }
        None => editor.targets().into_iter().filter(|t| matches!(t, TargetKind::Wasm | TargetKind::Js)).collect(),
    };
    let declaring: Vec<TargetKind> = candidates.iter().copied().filter(|kind| editor.npm_dependency(*kind, dev, name).is_some()).collect();
    match declaring.as_slice() {
        [kind] => Ok(*kind),
        [] => match wanted {
            Some(kind) => bail!("npm package `{name}` is not declared in [target.{kind}.{key}]"),
            None => bail!("npm package `{name}` is not declared in any [target.*.{key}]"),
        },
        _ => bail!("npm package `{name}` is declared for both the wasm and js targets; choose one with `-t wasm` or `-t js`"),
    }
}

fn check_npm_target(kind: TargetKind) -> Result<()> {
    if matches!(kind, TargetKind::Wasm | TargetKind::Js) {
        Ok(())
    } else {
        bail!("npm packages are for the wasm and js targets, not {kind}")
    }
}

/// The target `kargo add --npm` writes to: `wanted`, else the only one of wasm and js the project has.
pub fn pick_target(declared: &[TargetKind], wanted: Option<TargetKind>) -> Result<TargetKind> {
    if let Some(kind) = wanted {
        check_npm_target(kind)?;
        if !declared.contains(&kind) {
            bail!("[target.{kind}] is not declared in kargo.toml; add the target first with `kargo target add {kind}`");
        }
        return Ok(kind);
    }
    let capable: Vec<TargetKind> = declared.iter().copied().filter(|t| matches!(t, TargetKind::Wasm | TargetKind::Js)).collect();
    match capable.as_slice() {
        [kind] => Ok(*kind),
        [] => bail!(
            "npm packages are installed for the wasm and js targets, and this project has neither; add one with `kargo target add wasm`"
        ),
        _ => bail!("this project has both the wasm and js targets; choose where the package goes with `-t wasm` or `-t js`"),
    }
}

/// Splits `name[@version]`, where the name may be scoped (`@scope/name`).
pub fn split_spec(spec: &str) -> Result<(String, Option<String>)> {
    let spec = spec.trim();
    let at = spec.char_indices().skip(1).find(|(_, c)| *c == '@').map(|(i, _)| i);
    let (name, version) = match at {
        Some(i) => (&spec[..i], Some(spec[i + 1..].trim())),
        None => (spec, None),
    };
    let scoped_ok = match name.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').is_some_and(|(scope, rest)| !scope.is_empty() && !rest.is_empty() && !rest.contains('/')),
        None => !name.contains('/'),
    };
    if name.is_empty() || name.contains(char::is_whitespace) || !scoped_ok {
        bail!("`{spec}` is not an npm package name; use `name`, `@scope/name` or either followed by `@version`");
    }
    if version == Some("") {
        bail!("`{spec}` has an empty version; write `{name}` for the latest release or `{name}@1.2.3`");
    }
    Ok((name.to_owned(), version.map(str::to_owned)))
}

/// The version chosen for a package.
struct Chosen {
    /// What goes in the manifest.
    range: String,
    /// The registry's record of the package, if it was asked.
    package: Option<Package>,
    /// The version that range stands for, if known.
    version: Option<Semver>,
}

fn resolve(config: &Config, registry: &Registry, name: &str, requested: Option<&str>, opts: &NpmAddOptions) -> Result<Chosen> {
    let ask = || registry.package(name);
    let missing = |package: &Package| {
        format!(
            "the registry {} lists no releases of `{name}`; pass a version (`{name}@1.2.3`) or a dist-tag (`{name}@next`)",
            package.registry
        )
    };
    match requested {
        None => {
            let package = ask()?;
            let version = package.default_version(opts.prerelease).with_context(|| missing(&package))?;
            Ok(Chosen { range: format!("^{version}"), version: Some(version), package: Some(package) })
        }
        Some(text) => {
            if let Some(version) = Semver::parse(text) {
                // An exact version is checked against the registry, unless that is turned off or unreachable by choice.
                let verify = !opts.no_verify && !config.offline();
                let package = if verify || opts.types { Some(ask()?) } else { None };
                if verify && let Some(package) = &package {
                    if package.published(&version).is_none() {
                        bail!("`{name}@{text}` was not found in the registry {}", package.registry);
                    }
                    if package.published(&version).is_some_and(|p| p.deprecated) {
                        config.shell().warn(format!("`{name}@{text}` is deprecated"));
                    }
                }
                return Ok(Chosen { range: text.to_owned(), version: Some(version), package });
            }
            if let Some(range) = Range::parse(text) {
                let verify = !opts.no_verify && !config.offline();
                let package = if verify || opts.types { Some(ask()?) } else { None };
                let newest = package.as_ref().and_then(|p| range.newest(&p.semvers()).cloned());
                if verify
                    && newest.is_none()
                    && let Some(package) = &package
                {
                    bail!("no version of `{name}` in the registry {} satisfies `{text}`", package.registry);
                }
                return Ok(Chosen { range: text.to_owned(), version: newest, package });
            }
            // A dist-tag: `latest`, `next`, ...
            let package = ask()?;
            let Some(version) = package.tag(text) else {
                let tags: Vec<&str> = package.tags.keys().map(String::as_str).collect();
                bail!("`{name}` has no version or dist-tag `{text}` in the registry {} (tags: {})", package.registry, tags.join(", "));
            };
            Ok(Chosen { range: format!("^{version}"), version: Some(version), package: Some(package) })
        }
    }
}

/// The `@types/<name>` entry for a package that ships no declarations, as `(name, range)`. Not finding one
/// is a warning: the package still works from Kotlin through hand-written `@JsModule` declarations.
fn types_for(config: &Config, registry: &Registry, name: &str, chosen: &Chosen) -> Result<Option<(String, String)>> {
    if name.starts_with("@types/") {
        return Ok(None);
    }
    let shipped = match (&chosen.package, &chosen.version) {
        (Some(package), Some(version)) => package.published(version).map(|p| p.types),
        _ => None,
    };
    if shipped == Some(true) {
        config.shell().note(format!("`{name}` ships its own TypeScript declarations; no @types package is needed"));
        return Ok(None);
    }
    let types = format!("@types/{}", dts::types_name(name));
    let package = match registry.package(&types) {
        Ok(package) => package,
        Err(e) => {
            config.shell().warn(format!("`{name}` ships no TypeScript declarations and {e:#}"));
            return Ok(None);
        }
    };
    // The declarations are versioned like the package: the same major when the registry has one.
    let versions: Vec<Semver> = package.semvers().into_iter().filter(|v| !v.is_prerelease()).collect();
    let same_major = chosen.version.as_ref().and_then(|v| versions.iter().filter(|t| t.major == v.major).max());
    let Some(version) = same_major.cloned().or_else(|| package.default_version(false)) else {
        config.shell().warn(format!("`{types}` has no releases, so Kotlin gets no declarations for `{name}`"));
        return Ok(None);
    };
    Ok(Some((types, format!("^{version}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_names_and_versions() {
        let split = |s: &str| split_spec(s).unwrap();
        assert_eq!(split("left-pad"), ("left-pad".to_owned(), None));
        assert_eq!(split("left-pad@1.3.0"), ("left-pad".to_owned(), Some("1.3.0".to_owned())));
        assert_eq!(split("left-pad@latest"), ("left-pad".to_owned(), Some("latest".to_owned())));
        assert_eq!(split("@js-joda/core"), ("@js-joda/core".to_owned(), None));
        assert_eq!(split("@js-joda/core@^3.2.0"), ("@js-joda/core".to_owned(), Some("^3.2.0".to_owned())));
        assert_eq!(split("chart.js@>=4 <5").1.as_deref(), Some(">=4 <5"));
        for bad in ["", "@", "@scope", "@scope/", "a/b", "left pad", "left-pad@", "@a/b/c"] {
            assert!(split_spec(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn picks_the_target() {
        use TargetKind::*;
        assert_eq!(pick_target(&[Jvm, Wasm], None).unwrap(), Wasm);
        assert_eq!(pick_target(&[Js], None).unwrap(), Js);
        assert_eq!(pick_target(&[Wasm, Js], Some(Js)).unwrap(), Js);
        assert!(pick_target(&[Jvm], None).unwrap_err().to_string().contains("neither"));
        assert!(pick_target(&[Wasm, Js], None).unwrap_err().to_string().contains("both"));
        assert!(pick_target(&[Wasm], Some(Js)).unwrap_err().to_string().contains("[target.js] is not declared"));
        assert!(pick_target(&[Jvm, Wasm], Some(Jvm)).unwrap_err().to_string().contains("not jvm"));
    }
}
