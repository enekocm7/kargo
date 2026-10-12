//! `kargo outdated` and `kargo upgrade`: compare the versions declared in `kargo.toml` with the
//! versions published in the repositories, and bump them in place.

use anyhow::{Context, Result, bail};

use crate::workspace::Workspace;
use kargo_manifest::edit::{DependencySection, ManifestEditor};
use kargo_manifest::{Dependency, Manifest, TargetKind};
use kargo_maven::{MavenClient, Version, VersionReq, select_latest};
use kargo_util::config::Config;

/// A declared dependency and the newer versions available for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outdated {
    pub name: String,
    pub module: String,
    pub section: DependencySection,
    pub current: String,
    /// The newest compatible version (same major, or same minor for `0.x`), if newer than `current`.
    pub compatible: Option<String>,
    /// The newest version, if newer than `current`.
    pub latest: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UpgradeOptions {
    /// Only these dependencies (`group:artifact` or manifest key); all when empty.
    pub names: Vec<String>,
    /// Also consider versions that are not compatible with the current one.
    pub incompatible: bool,
    /// Consider pre-release versions (always done for dependencies already on a pre-release).
    pub prerelease: bool,
    /// Report the changes without writing `kargo.toml`.
    pub dry_run: bool,
}

/// Every dependency declared in the manifest, with the table it is declared in.
fn declared(manifest: &Manifest) -> Vec<(DependencySection, &Dependency)> {
    let mut out: Vec<(DependencySection, &Dependency)> = Vec::new();
    out.extend(manifest.dependencies.iter().map(|d| (DependencySection::Main, d)));
    out.extend(manifest.dev_dependencies.iter().map(|d| (DependencySection::Dev, d)));
    for kind in TargetKind::ALL {
        out.extend(manifest.targets.dependencies(kind).iter().map(|d| (DependencySection::Target(kind), d)));
    }
    out
}

/// The declared dependencies selected by `names`, failing on a name that matches nothing.
fn selected<'m>(manifest: &'m Manifest, names: &[String]) -> Result<Vec<(DependencySection, &'m Dependency)>> {
    let all = declared(manifest);
    for name in names {
        if !all.iter().any(|(_, d)| d.name == *name || d.module.to_string() == *name) {
            bail!("dependency `{name}` is not declared in kargo.toml");
        }
    }
    Ok(all.into_iter().filter(|(_, d)| names.is_empty() || names.iter().any(|n| d.name == *n || d.module.to_string() == *n)).collect())
}

/// Checks one dependency against the versions its repositories publish. Dependencies without a
/// plain version (ranges, or versions managed by a BOM) are skipped.
fn check(client: &MavenClient, section: DependencySection, dep: &Dependency, prerelease: bool) -> Result<Option<Outdated>> {
    let Some(raw) = &dep.version else { return Ok(None) };
    let Ok(VersionReq::Soft(current)) = raw.parse::<VersionReq>() else { return Ok(None) };
    let versions = client.versions(&dep.module).with_context(|| format!("failed to list the versions of `{}`", dep.module))?;
    let prerelease = prerelease || current.is_prerelease();
    // Stay on the same variant: a `-jre` dependency is not upgraded to `-android`.
    let variant = current.variant();
    let newer = |v: Option<&Version>| v.filter(|v| **v > current).map(|v| v.to_string());
    let compatible: Vec<Version> = versions.iter().filter(|v| current.is_compatible_with(v)).cloned().collect();
    Ok(Some(Outdated {
        name: dep.name.clone(),
        module: dep.module.to_string(),
        section,
        current: raw.clone(),
        compatible: newer(select_latest(&compatible, &variant, prerelease)),
        latest: newer(select_latest(&versions, &variant, prerelease)),
    }))
}

/// `kargo outdated`: the declared dependencies that have newer versions.
pub fn outdated(config: &Config, ws: &Workspace, names: &[String], prerelease: bool) -> Result<Vec<Outdated>> {
    let client = MavenClient::new(config, ws.manifest().repositories.clone());
    let mut out = Vec::new();
    for (section, dep) in selected(ws.manifest(), names)? {
        if let Some(o) = check(&client, section, dep, prerelease)?
            && (o.compatible.is_some() || o.latest.is_some())
        {
            out.push(o);
        }
    }
    Ok(out)
}

/// Renders `kargo outdated` results as a table.
pub fn render(rows: &[Outdated]) -> String {
    let mut table: Vec<[String; 5]> = vec![["name", "current", "compatible", "latest", "kind"].map(str::to_owned)];
    for r in rows {
        let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_owned());
        table.push([r.name.clone(), r.current.clone(), dash(&r.compatible), dash(&r.latest), r.section.display()]);
    }
    let widths: Vec<usize> = (0..5).map(|i| table.iter().map(|row| row[i].len()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for row in &table {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(cell, w)| format!("{cell:w$}")).collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// `kargo upgrade`: rewrites the versions in `kargo.toml` to the newest compatible versions (or
/// the newest overall with `incompatible`), preserving formatting. Returns the changes made.
pub fn upgrade(config: &Config, ws: &Workspace, opts: &UpgradeOptions) -> Result<Vec<Outdated>> {
    let client = MavenClient::new(config, ws.manifest().repositories.clone());
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let mut changed = Vec::new();
    for (section, dep) in selected(ws.manifest(), &opts.names)? {
        let Some(o) = check(&client, section, dep, opts.prerelease)? else {
            if opts.names.iter().any(|n| *n == dep.name || *n == dep.module.to_string()) {
                config.shell().note(format!("`{}` has no single version to upgrade (a range or a BOM-managed version)", dep.name));
            }
            continue;
        };
        let target = if opts.incompatible { o.latest.clone() } else { o.compatible.clone() };
        if !opts.incompatible
            && let Some(latest) = o.latest.as_ref().filter(|l| o.compatible.as_ref() != Some(*l))
        {
            config.shell().note(format!("`{}` {latest} is available but not compatible; use `--incompatible` to upgrade to it", o.name));
        }
        let Some(target) = target else { continue };
        editor.set_dependency_version(section, &dep.module, &target)?;
        config.shell().status("Upgrading", format!("{} v{} -> v{target} in [{}]", o.name, o.current, section.display()));
        changed.push(o);
    }
    if opts.dry_run {
        config.shell().warn("aborting upgrade due to dry run; kargo.toml was not changed");
        return Ok(changed);
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    Manifest::parse(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn publish(dir: &Path, artifact: &str, versions: &[&str]) {
        let base = dir.join(format!("com/acme/{artifact}"));
        std::fs::create_dir_all(&base).unwrap();
        let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
        std::fs::write(
            base.join("maven-metadata.xml"),
            format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"),
        )
        .unwrap();
    }

    fn setup(deps: &str) -> (tempfile::TempDir, tempfile::TempDir, Config, Workspace) {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        publish(&repo, "widget", &["1.0.0", "1.2.0", "1.3.0-RC1", "2.0.0"]);
        publish(&repo, "datetime", &["0.6.0", "0.6.2", "0.7.1", "0.8.0", "0.8.0-0.6.x-compat"]);
        publish(&repo, "fresh", &["3.0"]);
        let url = format!("file://{}", repo.display()).replace('\\', "/");
        std::fs::write(
            project.path().join("kargo.toml"),
            format!("[package]\nname = \"p\"\nversion = \"1.0\"\n\n[repositories]\nlocal = \"{url}\"\n\n{deps}"),
        )
        .unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ws = Workspace::load(&project.path().join("kargo.toml")).unwrap();
        (project, home, config, ws)
    }

    const DEPS: &str = "[dependencies]\n\"com.acme:widget\" = \"1.0.0\" # ui\n\"com.acme:fresh\" = \"3.0\"\nranged = { coords = \"com.acme:datetime\", version = \"[0.6,0.7)\" }\n\n[dev-dependencies]\n\"com.acme:datetime\" = \"0.6.0\"\n";

    #[test]
    fn outdated_reports_compatible_and_latest() {
        let (_p, _h, config, ws) = setup(DEPS);
        let rows = outdated(&config, &ws, &[], false).unwrap();
        let summary: Vec<(&str, Option<&str>, Option<&str>)> =
            rows.iter().map(|r| (r.name.as_str(), r.compatible.as_deref(), r.latest.as_deref())).collect();
        assert_eq!(summary, [("com.acme:widget", Some("1.2.0"), Some("2.0.0")), ("com.acme:datetime", Some("0.6.2"), Some("0.8.0"))]);
        assert_eq!(rows[1].section, DependencySection::Dev);
        let pre = outdated(&config, &ws, &["com.acme:widget".into()], true).unwrap();
        assert_eq!(pre[0].compatible.as_deref(), Some("1.3.0-RC1"));
        assert!(outdated(&config, &ws, &["com.acme:nope".into()], false).is_err());
        let table = render(&rows);
        assert!(table.starts_with("name               current  compatible  latest  kind\n"), "{table}");
        assert!(table.contains("com.acme:datetime  0.6.0    0.6.2       0.8.0   dev-dependencies\n"), "{table}");
    }

    #[test]
    fn upgrade_edits_the_manifest_in_place() {
        let (_p, _h, config, ws) = setup(DEPS);
        let dry = UpgradeOptions { dry_run: true, ..Default::default() };
        assert_eq!(upgrade(&config, &ws, &dry).unwrap().len(), 2);
        let before = std::fs::read_to_string(ws.manifest_path()).unwrap();
        assert!(before.contains("\"com.acme:widget\" = \"1.0.0\" # ui"), "dry run writes nothing");

        upgrade(&config, &ws, &UpgradeOptions::default()).unwrap();
        let text = std::fs::read_to_string(ws.manifest_path()).unwrap();
        assert!(text.contains("\"com.acme:widget\" = \"1.2.0\" # ui"), "{text}");
        assert!(text.contains("\"com.acme:datetime\" = \"0.6.2\""), "{text}");
        assert!(text.contains("version = \"[0.6,0.7)\""), "ranges are left alone: {text}");

        let ws = Workspace::load(&ws.manifest_path()).unwrap();
        let opts = UpgradeOptions { names: vec!["com.acme:datetime".into()], incompatible: true, ..Default::default() };
        upgrade(&config, &ws, &opts).unwrap();
        let text = std::fs::read_to_string(ws.manifest_path()).unwrap();
        assert!(text.contains("\"com.acme:datetime\" = \"0.8.0\""), "plain version, not the compat variant: {text}");
        assert!(text.contains("\"com.acme:widget\" = \"1.2.0\""), "only the named dependency: {text}");
    }
}
