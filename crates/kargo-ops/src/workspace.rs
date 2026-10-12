//! Loaded packages and their on-disk layout.
//!
//! A [`Workspace`] is one package:
//!
//! ```text
//! kargo.toml
//! kargo.lock
//! src/                 common sources (Kotlin, and Java for JVM)
//! src-<target>/        target-specific sources (src-jvm, src-wasm, src-native, src-android)
//! tests/               common tests
//! tests-<target>/      target-specific tests
//! resources/           JVM resources
//! test-resources/      JVM test resources
//! target/              build output
//! ```
//!
//! A [`Project`] is everything one command works on: a single package, or a
//! `[workspace]` of member packages, plus every package they reach through
//! `path` dependencies. The packages of a project share one `kargo.lock`, one
//! `target/` directory, one Kotlin version and one JDK.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use kargo_manifest::{JdkRequest, LOCKFILE_NAME, MANIFEST_NAME, Manifest, PathDependency, Profile, TargetKind, WorkspaceManifest};
use kargo_maven::Repository;
use kargo_util::flock::FileLock;
pub use kargo_util::fs::dunce_canonicalize;
use kargo_util::shell::Shell;

/// Environment variable overriding the build output directory.
pub const TARGET_DIR_ENV: &str = "KARGO_TARGET_DIR";

/// The lock file inside the target directory that serializes Kargo processes.
pub const BUILD_LOCK_NAME: &str = ".kargo-lock";

/// One package: its manifest, directory and where its build output goes.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    manifest: Manifest,
    target_dir: PathBuf,
    lockfile: PathBuf,
    /// Subdirectory of the build directories for this package's output, when it is built
    /// together with other packages (`app` for a workspace member, `deps/lib` for a path
    /// dependency of a single package).
    out_prefix: Option<PathBuf>,
    /// Prefix of this package's `kargo.lock` entries, shared with the other packages.
    lock_prefix: Option<String>,
    /// The workspace whose `[workspace.dependencies]` this package can inherit.
    workspace: Option<Arc<WorkspaceManifest>>,
    /// Path dependencies with the index of the package they point to in the [`Project`].
    path_deps: Vec<(PathDependency, usize)>,
}

/// Build profile selected on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileKind {
    #[default]
    Dev,
    Release,
}

impl ProfileKind {
    pub fn dir_name(self) -> &'static str {
        match self {
            ProfileKind::Dev => "debug",
            ProfileKind::Release => "release",
        }
    }
}

/// The build directory: `KARGO_TARGET_DIR`, or `target/` next to the manifest.
fn target_dir_for(root: &Path) -> PathBuf {
    match std::env::var_os(TARGET_DIR_ENV) {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() { dir } else { root.join(dir) }
        }
        _ => root.join("target"),
    }
}

impl Workspace {
    /// Finds `kargo.toml` in `start` or any parent directory.
    pub fn find_manifest(start: &Path) -> Result<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            let candidate = d.join(MANIFEST_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
            dir = d.parent();
        }
        bail!("could not find `{MANIFEST_NAME}` in `{}` or any parent directory", start.display())
    }

    /// Loads one package on its own, without its path dependencies.
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let root = dunce_canonicalize(&root);
        Ok(Self {
            target_dir: target_dir_for(&root),
            lockfile: root.join(LOCKFILE_NAME),
            root,
            manifest,
            out_prefix: None,
            lock_prefix: None,
            workspace: None,
            path_deps: Vec::new(),
        })
    }

    /// Loads the package the current directory (or the explicit manifest path) belongs to.
    /// Fails at the root of a workspace with several members, which is not a package.
    pub fn discover(cwd: &Path, manifest_path: Option<&Path>) -> Result<Self> {
        let project = Project::discover(cwd, manifest_path)?;
        let index = project.select_one(&Selection::default(), "this command")?;
        Ok(project.packages.into_iter().nth(index).expect("selected package exists"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_NAME)
    }

    /// The `kargo.lock` this package's dependencies are locked in (the workspace's, in a workspace).
    pub fn lockfile_path(&self) -> PathBuf {
        self.lockfile.clone()
    }

    /// The `kargo.lock` key of the resolution `key` (such as `jvm/runtime`) of this package.
    pub fn lock_key(&self, key: &str) -> String {
        match &self.lock_prefix {
            Some(prefix) => format!("{prefix}/{key}"),
            None => key.to_owned(),
        }
    }

    /// The shared build directory (`target/`).
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Takes the build-directory lock that guards `target/` and `kargo.lock`,
    /// waiting (with a `Blocking` status) while another Kargo process holds it.
    pub fn lock(&self, shell: &Shell) -> Result<FileLock> {
        FileLock::exclusive(&self.target_dir.join(BUILD_LOCK_NAME), shell, "build directory")
    }

    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }

    /// The `path` dependencies of this package, with the index of the package each points to.
    pub fn path_dependencies(&self) -> &[(PathDependency, usize)] {
        &self.path_deps
    }

    /// Validates an edited manifest of this package, resolving `{ workspace = true }` entries.
    pub fn check_manifest(&self, text: &str) -> Result<Manifest> {
        Manifest::parse_in(text, self.workspace.as_deref())
    }

    pub fn profile(&self, kind: ProfileKind) -> &Profile {
        match kind {
            ProfileKind::Dev => &self.manifest.dev_profile,
            ProfileKind::Release => &self.manifest.release_profile,
        }
    }

    /// This package's directory for one profile: `target/debug`, or `target/debug/<member>` in a workspace.
    pub fn profile_dir(&self, profile: ProfileKind) -> PathBuf {
        let dir = self.target_dir.join(profile.dir_name());
        match &self.out_prefix {
            Some(prefix) => dir.join(prefix),
            None => dir,
        }
    }

    /// Output directory for one target and profile, e.g. `target/debug/jvm`.
    pub fn output_dir(&self, profile: ProfileKind, target: TargetKind) -> PathBuf {
        self.profile_dir(profile).join(target.as_str())
    }

    /// A profile-independent working directory such as `target/lint` (`target/lint/<member>` in a workspace).
    pub fn work_dir(&self, name: &str) -> PathBuf {
        let dir = self.target_dir.join(name);
        match &self.out_prefix {
            Some(prefix) => dir.join(prefix),
            None => dir,
        }
    }

    /// Common source directory.
    pub fn common_sources(&self) -> PathBuf {
        self.root.join("src")
    }

    /// Target-specific source directory.
    pub fn target_sources(&self, target: TargetKind) -> PathBuf {
        self.root.join(target.source_dir_name())
    }

    pub fn common_tests(&self) -> PathBuf {
        self.root.join("tests")
    }

    pub fn target_tests(&self, target: TargetKind) -> PathBuf {
        self.root.join(format!("tests-{}", target.as_str()))
    }

    /// Every existing source and test directory (`src/`, `src-<target>/`, `tests/`,
    /// `tests-<target>/`) for any target, enabled or not, relative to the root.
    /// Used by `kargo fmt` and `kargo lint`, which never look inside `target/`.
    pub fn source_roots(&self) -> Vec<PathBuf> {
        let mut dirs = vec![PathBuf::from("src")];
        dirs.extend(TargetKind::ALL.iter().map(|t| PathBuf::from(t.source_dir_name())));
        dirs.push(PathBuf::from("tests"));
        dirs.extend(TargetKind::ALL.iter().map(|t| PathBuf::from(format!("tests-{}", t.as_str()))));
        dirs.into_iter().filter(|d| self.root.join(d).is_dir()).collect()
    }

    pub fn resources(&self) -> PathBuf {
        self.root.join("resources")
    }

    pub fn test_resources(&self) -> PathBuf {
        self.root.join("test-resources")
    }
}

/// Which packages a command applies to (`-p <name>`, `--workspace`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Package names given with `-p`.
    pub packages: Vec<String>,
    /// `--workspace`: every member.
    pub workspace: bool,
}

/// The packages one command works on: a single package or a workspace, plus their path dependencies.
#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    workspace: Option<Arc<WorkspaceManifest>>,
    target_dir: PathBuf,
    /// Members first (in `members` order; the single package otherwise), then path dependencies.
    packages: Vec<Workspace>,
    members: Vec<usize>,
    /// The member whose directory the command runs in.
    current: Option<usize>,
}

impl Project {
    /// Loads the project containing `cwd` (or the explicit manifest path). A package listed in
    /// the `members` of a workspace in a parent directory is loaded as part of that workspace.
    pub fn discover(cwd: &Path, manifest_path: Option<&Path>) -> Result<Self> {
        let path = match manifest_path {
            Some(p) => {
                if !p.is_file() {
                    bail!("manifest path `{}` does not exist", p.display());
                }
                p.to_path_buf()
            }
            None => Workspace::find_manifest(cwd)?,
        };
        Self::load(&path)
    }

    /// Loads the project of the manifest at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let dir = dunce_canonicalize(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")));
        if kargo_manifest::is_workspace_manifest(path)? {
            return Self::load_workspace(&dir, None);
        }
        match enclosing_workspace(&dir)? {
            Some(root) => Self::load_workspace(&root, Some(&dir)),
            None => {
                let mut loader = Loader::new(&dir, None);
                let index = loader.package(&dir)?;
                loader.finish(vec![index], Some(index))
            }
        }
    }

    fn load_workspace(root: &Path, current_dir: Option<&Path>) -> Result<Self> {
        let manifest = WorkspaceManifest::load(&root.join(MANIFEST_NAME))?;
        let mut loader = Loader::new(root, Some(Arc::new(manifest.clone())));
        let mut members = Vec::new();
        for member in &manifest.members {
            let dir = dunce_canonicalize(&root.join(member));
            let path = dir.join(MANIFEST_NAME);
            if !path.is_file() {
                bail!("workspace member `{}` has no {MANIFEST_NAME} (looked for {})", member.display(), path.display());
            }
            if kargo_manifest::is_workspace_manifest(&path)? {
                bail!("workspace member `{}` is itself a workspace; nested workspaces are not supported", member.display());
            }
            let index = loader.package(&dir).with_context(|| format!("failed to load workspace member `{}`", member.display()))?;
            members.push(index);
        }
        let current = current_dir.and_then(|d| members.iter().copied().find(|&i| loader.packages[i].root == d));
        loader.finish(members, current)
    }

    /// The directory of the root manifest: the workspace root, or the single package's directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether this is a `[workspace]`.
    pub fn is_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    pub fn lockfile_path(&self) -> PathBuf {
        self.root.join(LOCKFILE_NAME)
    }

    /// Every package: the members, then the packages reached through path dependencies.
    pub fn packages(&self) -> &[Workspace] {
        &self.packages
    }

    pub fn package(&self, index: usize) -> &Workspace {
        &self.packages[index]
    }

    /// Indices of the workspace members (the single package outside a workspace).
    pub fn members(&self) -> &[usize] {
        &self.members
    }

    /// The member the command runs in, if any.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Every repository any package declares, in order: a dependency's own repositories may be
    /// needed to resolve the libraries it brings to its consumers.
    pub fn repositories(&self) -> Vec<Repository> {
        let mut out: Vec<Repository> = Vec::new();
        for repo in self.packages.iter().flat_map(|p| &p.manifest.repositories) {
            if !out.iter().any(|r| r.url == repo.url) {
                out.push(repo.clone());
            }
        }
        out
    }

    fn member_names(&self) -> String {
        self.members.iter().map(|&i| self.packages[i].name()).collect::<Vec<_>>().join(", ")
    }

    fn find(&self, name: &str) -> Result<usize> {
        self.packages
            .iter()
            .position(|p| p.name() == name)
            .with_context(|| format!("package `{name}` is not part of this project; its members are: {}", self.member_names()))
    }

    /// The packages a command applies to: the `-p` packages, every member with `--workspace`,
    /// else the member the command runs in, or every member at the workspace root.
    pub fn select(&self, selection: &Selection) -> Result<Vec<usize>> {
        if selection.workspace {
            return Ok(self.members.clone());
        }
        if !selection.packages.is_empty() {
            let mut out = Vec::new();
            for name in &selection.packages {
                let index = self.find(name)?;
                if !out.contains(&index) {
                    out.push(index);
                }
            }
            return Ok(out);
        }
        Ok(match self.current {
            Some(i) => vec![i],
            None => self.members.clone(),
        })
    }

    /// The one package a command such as `kargo run` applies to.
    pub fn select_one(&self, selection: &Selection, command: &str) -> Result<usize> {
        if selection.workspace {
            bail!("{command} works on one package and does not take --workspace; pick a member with `-p <name>`");
        }
        if selection.packages.len() > 1 {
            bail!("{command} works on one package; pass a single `-p <name>`");
        }
        match self.select(selection)?[..] {
            [one] => Ok(one),
            _ => {
                bail!("{command} works on one package, but this workspace has several; pick one with `-p <name>` ({})", self.member_names())
            }
        }
    }
}

/// The workspace in a parent directory whose `members` list the package in `dir`.
fn enclosing_workspace(dir: &Path) -> Result<Option<PathBuf>> {
    for ancestor in dir.ancestors().skip(1) {
        let candidate = ancestor.join(MANIFEST_NAME);
        if !candidate.is_file() || !kargo_manifest::is_workspace_manifest(&candidate).unwrap_or(false) {
            continue;
        }
        let manifest = WorkspaceManifest::load(&candidate)?;
        let listed = manifest.members.iter().any(|m| dunce_canonicalize(&ancestor.join(m)) == dir);
        return Ok(listed.then(|| ancestor.to_path_buf()));
    }
    Ok(None)
}

/// Loads packages and follows their path dependencies.
struct Loader {
    root: PathBuf,
    workspace: Option<Arc<WorkspaceManifest>>,
    target_dir: PathBuf,
    packages: Vec<Workspace>,
}

impl Loader {
    fn new(root: &Path, workspace: Option<Arc<WorkspaceManifest>>) -> Self {
        Self { root: root.to_path_buf(), workspace, target_dir: target_dir_for(root), packages: Vec::new() }
    }

    /// Loads the package in `dir` (once), returning its index.
    fn package(&mut self, dir: &Path) -> Result<usize> {
        if let Some(i) = self.packages.iter().position(|p| p.root == dir) {
            return Ok(i);
        }
        let manifest = Manifest::load_in(&dir.join(MANIFEST_NAME), self.workspace.as_deref())?;
        let name = manifest.package.name.clone();
        if let Some(other) = self.packages.iter().find(|p| p.name() == name) {
            bail!(
                "two packages are named `{name}` ({} and {}); packages built together need unique names",
                other.root.display(),
                dir.display()
            );
        }
        // The package of a single-package project keeps the plain `target/<profile>/<target>` layout.
        let primary = self.workspace.is_none() && self.packages.is_empty();
        let (out_prefix, lock_prefix) = match (primary, &self.workspace) {
            (true, _) => (None, None),
            (false, Some(_)) => (Some(PathBuf::from(&name)), Some(name.clone())),
            (false, None) => (Some(Path::new("deps").join(&name)), Some(name.clone())),
        };
        self.packages.push(Workspace {
            root: dir.to_path_buf(),
            manifest,
            target_dir: self.target_dir.clone(),
            lockfile: self.root.join(LOCKFILE_NAME),
            out_prefix,
            lock_prefix,
            workspace: self.workspace.clone(),
            path_deps: Vec::new(),
        });
        Ok(self.packages.len() - 1)
    }

    /// Loads every path dependency, checks the graph and returns the project.
    fn finish(mut self, members: Vec<usize>, current: Option<usize>) -> Result<Project> {
        let mut i = 0;
        // `packages` grows while path dependencies are discovered.
        while i < self.packages.len() {
            let consumer = self.packages[i].name().to_owned();
            let declared = self.packages[i].manifest.path_dependencies.clone();
            let mut edges = Vec::new();
            for dep in declared {
                let dir = dunce_canonicalize(&self.packages[i].root.join(&dep.path));
                let path = dir.join(MANIFEST_NAME);
                let ctx = || format!("path dependency `{}` of `{consumer}` ({})", dep.name, dep.path.display());
                if !path.is_file() {
                    bail!("{} has no {MANIFEST_NAME} (looked for {})", ctx(), path.display());
                }
                if kargo_manifest::is_workspace_manifest(&path).with_context(ctx)? {
                    bail!("{} is a workspace, not a package; point it at one of the members", ctx());
                }
                let index = self.package(&dir).with_context(ctx)?;
                let name = self.packages[index].name();
                if name != dep.name {
                    bail!("{} is the package `{name}`; rename the key to `{name}`", ctx());
                }
                edges.push((dep, index));
            }
            self.packages[i].path_deps = edges;
            i += 1;
        }
        check_cycles(&self.packages)?;
        unify_toolchains(&mut self.packages, self.workspace.is_some())?;
        Ok(Project { root: self.root, workspace: self.workspace, target_dir: self.target_dir, packages: self.packages, members, current })
    }
}

/// Fails if packages depend on each other in a cycle (dev-dependencies may point back).
fn check_cycles(packages: &[Workspace]) -> Result<()> {
    fn visit(packages: &[Workspace], i: usize, stack: &mut Vec<usize>, done: &mut [bool]) -> Result<()> {
        if done[i] {
            return Ok(());
        }
        if let Some(pos) = stack.iter().position(|&s| s == i) {
            let mut cycle: Vec<&str> = stack[pos..].iter().map(|&s| packages[s].name()).collect();
            cycle.push(packages[i].name());
            bail!("path dependencies form a cycle: {}", cycle.join(" -> "));
        }
        stack.push(i);
        for (dep, child) in &packages[i].path_deps {
            if !dep.dev {
                visit(packages, *child, stack, done)?;
            }
        }
        stack.pop();
        done[i] = true;
        Ok(())
    }
    let mut done = vec![false; packages.len()];
    for i in 0..packages.len() {
        visit(packages, i, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

/// Packages built together share one Kotlin compiler and one JDK: the version set in any of
/// them (or in `[workspace.kotlin]`) applies to all, and two different settings are an error.
fn unify_toolchains(packages: &mut [Workspace], workspace: bool) -> Result<()> {
    let hint = if workspace { "set it once in [workspace.kotlin]" } else { "use the same `[kotlin] version` in each" };
    let kotlin: BTreeMap<String, String> = packages
        .iter()
        .filter(|p| p.manifest.kotlin.version_set)
        .map(|p| (p.manifest.kotlin.version.clone(), p.name().to_owned()))
        .collect();
    if kotlin.len() > 1 {
        let list: Vec<String> = kotlin.iter().map(|(v, name)| format!("`{name}` uses Kotlin {v}")).collect();
        bail!("packages built together must use one Kotlin version, but {}; {hint}", list.join(" and "));
    }
    if let Some(version) = kotlin.into_keys().next() {
        for p in packages.iter_mut() {
            p.manifest.kotlin.version = version.clone();
        }
    }

    let jdks: BTreeMap<String, String> = packages
        .iter()
        .filter(|p| p.manifest.toolchain.jdk != JdkRequest::System)
        .map(|p| (p.manifest.toolchain.jdk.to_string(), p.name().to_owned()))
        .collect();
    if jdks.len() > 1 {
        let list: Vec<String> = jdks.iter().map(|(jdk, name)| format!("`{name}` uses JDK {jdk}")).collect();
        bail!("packages built together must use one JDK, but {}; use the same [toolchain] `jdk` in each", list.join(" and "));
    }
    if let Some(jdk) = packages.iter().map(|p| p.manifest.toolchain.jdk.clone()).find(|j| *j != JdkRequest::System) {
        for p in packages.iter_mut() {
            p.manifest.toolchain.jdk = jdk.clone();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_manifest_in_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_NAME), "[package]\nname = \"w\"\nversion = \"0.1.0\"\n").unwrap();
        let nested = dir.path().join("src/a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let ws = Workspace::discover(&nested, None).unwrap();
        assert_eq!(ws.name(), "w");
        assert_eq!(ws.root(), dunce_canonicalize(dir.path()));
        assert!(ws.output_dir(ProfileKind::Release, TargetKind::Jvm).ends_with("target/release/jvm"));
        assert!(ws.target_sources(TargetKind::Native).ends_with("src-native"));
        assert!(ws.target_tests(TargetKind::Jvm).ends_with("tests-jvm"));
        assert!(!ws.root().to_string_lossy().starts_with(r"\\?\"));
        std::fs::create_dir_all(dir.path().join("tests-jvm")).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        assert_eq!(ws.source_roots(), [PathBuf::from("src"), PathBuf::from("tests-jvm")]);
    }

    #[test]
    fn missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = Workspace::discover(dir.path(), None).unwrap_err();
        assert!(err.to_string().contains("could not find"));
        assert!(Workspace::discover(dir.path(), Some(&dir.path().join("nope.toml"))).is_err());
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn package(name: &str, extra: &str) -> String {
        format!(
            "[package]
name = \"{name}\"
version = \"0.1.0\"
{extra}"
        )
    }

    fn load_err(path: &Path) -> String {
        format!("{:#}", Project::load(path).unwrap_err())
    }

    #[test]
    fn workspace_members_share_lockfile_and_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce_canonicalize(dir.path());
        write(
            &root,
            "kargo.toml",
            "[workspace]
members = [\"app\", \"libs/core\"]
[workspace.kotlin]
version = \"2.3.0\"
",
        );
        write(
            &root,
            "app/kargo.toml",
            &package(
                "app",
                "[dependencies]
core = { path = \"../libs/core\" }
",
            ),
        );
        write(&root, "libs/core/kargo.toml", &package("core", ""));

        let project = Project::discover(&root, None).unwrap();
        assert!(project.is_workspace());
        assert_eq!(project.root(), root);
        assert_eq!(project.members(), [0, 1]);
        assert_eq!(project.current(), None);
        let (app, core) = (project.package(0), project.package(1));
        assert_eq!(app.path_dependencies()[0].1, 1);
        assert_eq!(app.lockfile_path(), root.join("kargo.lock"));
        assert_eq!(core.lockfile_path(), root.join("kargo.lock"));
        assert!(app.output_dir(ProfileKind::Dev, TargetKind::Jvm).ends_with("target/debug/app/jvm"));
        assert!(core.work_dir("lint").ends_with("target/lint/core"));
        assert_eq!(app.lock_key("jvm/runtime"), "app/jvm/runtime");
        assert_eq!(core.manifest().kotlin.version, "2.3.0");

        // Every member at the root, the current one inside a member, any package with -p.
        let all = Selection::default();
        assert_eq!(project.select(&all).unwrap(), [0, 1]);
        assert!(format!("{:#}", project.select_one(&all, "`kargo run`").unwrap_err()).contains("pick one with `-p <name>` (app, core)"));
        let core_only = Selection { packages: vec!["core".into()], workspace: false };
        assert_eq!(project.select_one(&core_only, "`kargo run`").unwrap(), 1);
        let unknown = Selection { packages: vec!["nope".into()], workspace: false };
        assert!(format!("{:#}", project.select(&unknown).unwrap_err()).contains("members are: app, core"));
        let whole = Selection { packages: vec![], workspace: true };
        assert!(project.select_one(&whole, "`kargo run`").is_err());

        let inside = Project::discover(&root.join("libs/core/src"), None).unwrap();
        assert_eq!(inside.current(), Some(1));
        assert_eq!(inside.select(&all).unwrap(), [1]);
        assert_eq!(Workspace::discover(&root.join("app"), None).unwrap().name(), "app");
        assert!(Workspace::discover(&root, None).is_err());
    }

    #[test]
    fn single_package_path_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce_canonicalize(dir.path());
        write(
            &root,
            "calc/kargo.toml",
            &package(
                "calc",
                "[dependencies]
mathlib = { path = \"../mathlib\" }
",
            ),
        );
        write(
            &root,
            "mathlib/kargo.toml",
            &package(
                "mathlib",
                "[kotlin]
version = \"2.3.0\"
[dev-dependencies]
calc = { path = \"../calc\" }
",
            ),
        );

        let project = Project::load(&root.join("calc/kargo.toml")).unwrap();
        assert!(!project.is_workspace());
        assert_eq!(project.packages().len(), 2);
        let (calc, mathlib) = (project.package(0), project.package(1));
        // The consumer keeps the plain layout; its dependency builds under `deps/`.
        assert!(calc.output_dir(ProfileKind::Dev, TargetKind::Jvm).ends_with("calc/target/debug/jvm"));
        assert!(mathlib.output_dir(ProfileKind::Dev, TargetKind::Jvm).ends_with("calc/target/debug/deps/mathlib/jvm"));
        assert_eq!(calc.lock_key("jvm/runtime"), "jvm/runtime");
        assert_eq!(mathlib.lock_key("jvm/runtime"), "mathlib/jvm/runtime");
        assert_eq!(mathlib.lockfile_path(), root.join("calc/kargo.lock"));
        // The one explicit Kotlin version applies to both; a dev-dependency may point back.
        assert_eq!(calc.manifest().kotlin.version, "2.3.0");
    }

    #[test]
    fn invalid_path_dependencies_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce_canonicalize(dir.path());
        let app = |deps: &str| {
            write(
                &root,
                "app/kargo.toml",
                &package(
                    "app",
                    &format!(
                        "[dependencies]
{deps}
"
                    ),
                ),
            )
        };
        let path = root.join("app/kargo.toml");

        app("lib = { path = \"../missing\" }");
        assert!(load_err(&path).contains("has no kargo.toml"));

        write(&root, "lib/kargo.toml", &package("other", ""));
        app("lib = { path = \"../lib\" }");
        assert!(load_err(&path).contains("is the package `other`; rename the key to `other`"));

        write(
            &root,
            "lib/kargo.toml",
            &package(
                "lib",
                "[dependencies]
app = { path = \"../app\" }
",
            ),
        );
        assert!(load_err(&path).contains("cycle: app -> lib -> app"));

        write(
            &root,
            "lib/kargo.toml",
            &package(
                "lib",
                "[kotlin]
version = \"2.3.0\"
",
            ),
        );
        app("lib = { path = \"../lib\" }
[kotlin]
version = \"2.4.0\"");
        assert!(load_err(&path).contains("`lib` uses Kotlin 2.3.0 and `app` uses Kotlin 2.4.0"));

        write(
            &root,
            "lib/kargo.toml",
            &package(
                "lib",
                "[toolchain]
jdk = \"17\"
",
            ),
        );
        app("lib = { path = \"../lib\" }
[toolchain]
jdk = \"21\"");
        assert!(load_err(&path).contains("must use one JDK"));

        write(
            &root,
            "lib/kargo.toml",
            "[workspace]
members = [\"x\"]
",
        );
        app("lib = { path = \"../lib\" }");
        assert!(load_err(&path).contains("is a workspace, not a package"));
    }

    #[test]
    fn invalid_workspaces_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce_canonicalize(dir.path());
        let path = root.join("kargo.toml");
        write(
            &root,
            "kargo.toml",
            "[workspace]
members = [\"a\"]
",
        );
        assert!(load_err(&path).contains("workspace member `a` has no kargo.toml"));
        write(&root, "a/kargo.toml", &package("same", ""));
        write(&root, "b/kargo.toml", &package("same", ""));
        write(
            &root,
            "kargo.toml",
            "[workspace]
members = [\"a\", \"b\"]
",
        );
        assert!(load_err(&path).contains("two packages are named `same`"));
        // A package that the nearest workspace does not list is a project of its own.
        write(&root, "c/kargo.toml", &package("c", ""));
        assert!(!Project::load(&root.join("c/kargo.toml")).unwrap().is_workspace());
    }
}
