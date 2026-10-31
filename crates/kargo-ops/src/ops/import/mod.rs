//! `kargo import`: turns a Maven, Gradle or Kotlin Toolchain project into a Kargo project.
//!
//! Importing is planned first and applied second. [`plan`] reads the build files, builds the
//! `kargo.toml` files, checks that each of them is valid and works out every file move, without
//! touching the project. [`apply`] writes the manifests and moves the sources. `--dry-run` stops
//! after the plan.

mod frameworks;
mod gradle;
mod kotlin_toolchain;
mod layout;
mod maven;
mod model;

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_manifest::{MANIFEST_NAME, Manifest, WorkspaceManifest};
use kargo_util::config::Config;
use kargo_util::shell::Shell;

pub use self::layout::FileMove;
use self::model::{Package, Section, Value, render};

/// Where the documentation lives; the report links to it.
const DOCS: &str = "https://enekocm7.github.io/kargo";

/// The build tool a project comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Maven,
    Gradle,
    KotlinToolchain,
}

impl Build {
    pub fn name(self) -> &'static str {
        match self {
            Build::Maven => "Maven",
            Build::Gradle => "Gradle",
            Build::KotlinToolchain => "Kotlin Toolchain",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// The build tool to import from, when the directory has the files of both.
    pub from: Option<Build>,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Translated, or in need of manual work.
    pub translated: bool,
    pub message: String,
    /// The documentation page that helps with it, relative to the site (`guides/dependencies/`).
    pub docs: Option<&'static str>,
}

/// What a reader (Maven or Gradle) found.
#[derive(Debug, Default)]
struct Imported {
    packages: Vec<Package>,
    /// The root is a workspace of `packages`.
    workspace: bool,
    notes: Vec<Note>,
}

impl Imported {
    fn translated(&mut self, message: impl Into<String>) {
        self.notes.push(Note { translated: true, message: message.into(), docs: None });
    }

    fn manual(&mut self, message: impl Into<String>, docs: &'static str) {
        self.notes.push(Note { translated: false, message: message.into(), docs: Some(docs) });
    }
}

/// Everything `kargo import` will do.
#[derive(Debug)]
pub struct Plan {
    pub build: Build,
    /// Each `kargo.toml` to write: its path relative to the project root and its text.
    pub manifests: Vec<(PathBuf, String)>,
    /// The packages (their names), for the summary.
    pub packages: Vec<String>,
    pub moves: Vec<FileMove>,
    pub notes: Vec<Note>,
}

impl Plan {
    /// The plan as text for `--dry-run`: every manifest, then every move.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for (path, text) in &self.manifests {
            let _ = writeln!(out, "# {}", display(path));
            out.push_str(text);
            out.push('\n');
        }
        if self.moves.is_empty() {
            out.push_str("# no files move\n");
        } else {
            let _ = writeln!(out, "# {} files move:", self.moves.len());
            for m in &self.moves {
                let _ = writeln!(out, "{} -> {}", display(&m.from), display(&m.to));
            }
        }
        out
    }
}

/// A path with forward slashes, in messages and plans.
fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Works out what importing the project in `root` does. Writes and moves nothing.
pub fn plan(config: &Config, root: &Path, opts: &ImportOptions) -> Result<Plan> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    if root.join(MANIFEST_NAME).exists() {
        bail!("{} already has a {MANIFEST_NAME}; there is nothing to import", root.display());
    }
    let build = detect(root, opts.from)?;
    let mut imported = match build {
        Build::Maven => maven::read(config, root)?,
        Build::Gradle => gradle::read(root)?,
        Build::KotlinToolchain => kotlin_toolchain::read(root)?,
    };
    export_for_dependents(&mut imported.packages);
    if imported.workspace {
        unify_toolchains(&mut imported);
    }

    let mut manifests = Vec::new();
    let mut workspace_manifest = None;
    let workspace_kotlin = shared_kotlin_version(&imported.packages).filter(|_| imported.workspace);
    if imported.workspace {
        let members: Vec<String> = imported.packages.iter().map(|p| display(&p.dir)).collect();
        let mut workspace = Section::new("workspace");
        workspace.row("members", Value::strings(members));
        let mut kotlin = Section::new("workspace.kotlin");
        kotlin.row_if("version", workspace_kotlin.clone());
        let text = render(&[workspace, kotlin]);
        workspace_manifest = Some(WorkspaceManifest::parse(&text, root).context("the generated workspace manifest is not valid")?);
        manifests.push((PathBuf::from(MANIFEST_NAME), text));
    }
    for package in &imported.packages {
        let text = package.manifest(workspace_kotlin.as_deref());
        // A member is checked against its workspace, which has rules of its own.
        Manifest::parse_in(&text, workspace_manifest.as_ref())
            .with_context(|| format!("the generated manifest of `{}` is not valid:\n{text}", package.name))?;
        manifests.push((package.dir.join(MANIFEST_NAME), text));
    }
    for (path, _) in &manifests {
        if root.join(path).exists() {
            bail!("{} already exists", display(path));
        }
    }

    let dir_moves: Vec<_> = imported.packages.iter().flat_map(|p| p.moves.iter().cloned()).collect();
    let moves = layout::plan(root, &dir_moves)?;
    Ok(Plan { build, manifests, packages: imported.packages.iter().map(|p| p.name.clone()).collect(), moves, notes: imported.notes })
}

/// Which build tool to import from: the one with build files in `root`.
fn detect(root: &Path, from: Option<Build>) -> Result<Build> {
    let maven = root.join("pom.xml").is_file();
    let gradle = ["build.gradle.kts", "build.gradle", "settings.gradle.kts", "settings.gradle"].iter().any(|f| root.join(f).is_file());
    // The wrapper alone is enough to recognize a Kotlin Toolchain project; `read` then asks for the configuration.
    let kotlin = root.join("project.yaml").is_file()
        || root.join("module.yaml").is_file()
        || ["kotlin", "kotlin.bat", "amper", "amper.bat"].iter().any(|f| root.join(f).is_file());
    if let Some(build) = from {
        let present = match build {
            Build::Maven => maven,
            Build::Gradle => gradle,
            Build::KotlinToolchain => kotlin,
        };
        if present {
            return Ok(build);
        }
        match build {
            Build::Maven => bail!("there is no pom.xml in {}", root.display()),
            Build::Gradle => bail!("there is no build.gradle.kts or build.gradle in {}", root.display()),
            Build::KotlinToolchain => bail!("there is no project.yaml, module.yaml or kotlin wrapper in {}", root.display()),
        }
    }
    match (maven, gradle, kotlin) {
        (false, false, false) => bail!(
            "there is no pom.xml, build.gradle.kts, build.gradle, project.yaml or module.yaml in {}; \
             `kargo import` needs a Maven, Gradle or Kotlin Toolchain project",
            root.display()
        ),
        (true, false, false) => Ok(Build::Maven),
        (false, true, false) => Ok(Build::Gradle),
        (false, false, true) => Ok(Build::KotlinToolchain),
        _ => {
            let mut names = Vec::new();
            if maven {
                names.push("Maven");
            }
            if gradle {
                names.push("Gradle");
            }
            if kotlin {
                names.push("Kotlin Toolchain");
            }
            let options = names.iter().map(|n| format!("`--from {}`", n.to_lowercase().replace(' ', "-"))).collect::<Vec<_>>().join(", ");
            bail!("{} has the files of several build tools ({}); choose with {options}", root.display(), names.join(", "))
        }
    }
}

/// The Kotlin version every package builds with, when they agree.
fn shared_kotlin_version(packages: &[Package]) -> Option<String> {
    let first = packages.first()?.kotlin.version.clone()?;
    packages.iter().all(|p| p.kotlin.version.as_deref() == Some(first.as_str())).then_some(first)
}

/// The numbers in a version, to tell which of two is newer.
fn version_numbers(version: &str) -> Vec<u64> {
    version.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect()
}

/// The packages of a workspace build with one Kotlin compiler and one JDK. Modules that asked for different
/// ones get the newest, and the report says so.
fn unify_toolchains(imported: &mut Imported) {
    let mut kotlin: Vec<String> = imported.packages.iter().filter_map(|p| p.kotlin.version.clone()).collect();
    kotlin.sort();
    kotlin.dedup();
    if kotlin.len() > 1
        && let Some(newest) = kotlin.iter().max_by_key(|v| version_numbers(v)).cloned()
    {
        imported.manual(
            format!(
                "the modules use the Kotlin versions {}; a workspace builds every member with one, so they all use {newest}",
                kotlin.join(", ")
            ),
            "guides/workspaces/",
        );
        for package in &mut imported.packages {
            package.kotlin.version = Some(newest.clone());
        }
    }
    let mut jdks: Vec<String> = imported.packages.iter().filter_map(|p| p.jdk.clone()).collect();
    jdks.sort();
    jdks.dedup();
    if jdks.len() > 1
        && let Some(newest) = jdks.iter().max_by_key(|v| version_numbers(v)).cloned()
    {
        imported.manual(
            format!("the modules ask for the JDKs {}; a workspace builds every member with one, so they all use {newest}", jdks.join(", ")),
            "guides/toolchains/",
        );
        for package in &mut imported.packages {
            package.jdk = Some(newest.clone());
        }
    }
}

/// In Maven and Gradle the dependencies of a module are visible to the modules that depend on it
/// when they are `compile` / `api`; Kargo needs `exported = true` for that on a path dependency.
fn export_for_dependents(packages: &mut [Package]) {
    let depended_on: Vec<String> = packages
        .iter()
        .flat_map(|p| p.dependencies.iter().chain(&p.dev_dependencies))
        .filter_map(|d| d.path.as_ref().map(|l| l.name.clone()))
        .collect();
    for package in packages.iter_mut().filter(|p| depended_on.contains(&p.name)) {
        for dep in package.dependencies.iter_mut().filter(|d| d.scope == model::Scope::Compile && !d.bom) {
            dep.exported = true;
        }
    }
}

/// The path from directory `from` to directory `to`, both relative to the same root.
fn relative_dir(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    let to: Vec<_> = to.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for component in &to[common..] {
        out.push(component);
    }
    out
}

/// Writes the manifests and moves the sources. Returns whether `git mv` did the moving.
pub fn apply(shell: &Shell, root: &Path, plan: &Plan) -> Result<bool> {
    for (path, text) in &plan.manifests {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&target, text).with_context(|| format!("failed to write {}", target.display()))?;
        shell.status("Created", display(path));
    }
    let git = !plan.moves.is_empty() && layout::in_git_repository(root);
    layout::apply(root, &plan.moves, git)?;
    if !plan.moves.is_empty() {
        shell.status("Moved", format!("{} source files into Kargo's layout{}", plan.moves.len(), if git { " with git mv" } else { "" }));
    }
    ignore_target(shell, root);
    Ok(git)
}

/// Gradle writes `build/` and Maven `target/`; Kargo's `target/` is the one thing a project's
/// `.gitignore` may not cover yet.
fn ignore_target(shell: &Shell, root: &Path) {
    let path = root.join(".gitignore");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    if text.lines().any(|line| matches!(line.trim(), "target" | "target/" | "/target" | "/target/" | "**/target" | "**/target/")) {
        return;
    }
    let separator = if text.ends_with('\n') || text.is_empty() { "" } else { "\n" };
    if std::fs::write(&path, format!("{text}{separator}/target\n")).is_ok() {
        shell.status("Updated", ".gitignore (Kargo builds into target/)");
    }
}

/// Prints the report: what was translated and what needs manual work.
pub fn report(shell: &Shell, plan: &Plan) {
    for note in plan.notes.iter().filter(|n| n.translated) {
        shell.status("Translated", &note.message);
    }
    let manual: Vec<&Note> = plan.notes.iter().filter(|n| !n.translated).collect();
    if manual.is_empty() {
        return;
    }
    shell.warn(format!("{} items need manual work:", manual.len()));
    for note in manual {
        match note.docs {
            Some(docs) => shell.warn(format!("  - {} (see {DOCS}/{docs})", note.message)),
            None => shell.warn(format!("  - {}", note.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_directories() {
        assert_eq!(relative_dir(Path::new("app"), Path::new("core")), PathBuf::from("../core"));
        assert_eq!(relative_dir(Path::new("libs/a"), Path::new("libs/b")), PathBuf::from("../b"));
        assert_eq!(relative_dir(Path::new(""), Path::new("core")), PathBuf::from("core"));
        assert_eq!(relative_dir(Path::new("app/ui"), Path::new("")), PathBuf::from("../.."));
    }

    #[test]
    fn detects_the_build_tool() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect(dir.path(), None).unwrap_err().to_string().contains("no pom.xml"));
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        assert_eq!(detect(dir.path(), None).unwrap(), Build::Maven);
        std::fs::write(dir.path().join("build.gradle.kts"), "").unwrap();
        assert!(detect(dir.path(), None).unwrap_err().to_string().contains("--from"));
        assert_eq!(detect(dir.path(), Some(Build::Gradle)).unwrap(), Build::Gradle);
    }

    #[test]
    fn detects_the_kotlin_toolchain_from_its_configuration_or_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("module.yaml"), "product: jvm/app\n").unwrap();
        assert_eq!(detect(dir.path(), None).unwrap(), Build::KotlinToolchain);
        // A wrapper alone is enough, like `gradlew` next to a missing script.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kotlin"), "#!/bin/sh\n").unwrap();
        assert_eq!(detect(dir.path(), None).unwrap(), Build::KotlinToolchain);
        assert!(detect(dir.path(), Some(Build::Maven)).unwrap_err().to_string().contains("no pom.xml"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        std::fs::write(dir.path().join("kotlin"), "#!/bin/sh\n").unwrap();
        let error = detect(dir.path(), None).unwrap_err().to_string();
        assert!(error.contains("--from maven") && error.contains("--from kotlin"), "{error}");
        assert_eq!(detect(dir.path(), Some(Build::KotlinToolchain)).unwrap(), Build::KotlinToolchain);
    }
}
