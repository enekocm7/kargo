//! `kargo install` and `kargo uninstall`: put a Kotlin command-line tool on the `PATH`.
//!
//! A tool comes from a project directory (`--path`), a git repository (`--git`) or a published
//! application on Maven (`group:artifact[:version]`). Its launchers go to `~/.kargo/bin`, the same
//! directory `kargo-<name>` plugins live in, and its files to `~/.kargo/install/<name>/<version>/`:
//!
//! - **JVM**: the release build's self-contained jar (or, with `--format jlink`, a runtime image)
//!   and a launcher that runs it with the JDK the build used, pinned by path so a later `JAVA_HOME`
//!   does not break it.
//! - **Native**: the release executable of the host, copied to `~/.kargo/bin` as it is.
//! - **Maven**: the application's jar and its runtime dependencies, run by their main class.
//!
//! `~/.kargo/install/.installed.toml` records what was installed, for `--list`, reinstalls and
//! `kargo uninstall`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::build::{self, BuildOptions};
use super::context::BuildContext;
use super::packaging::{App, app_jvm_args, bat_quote, jar_names, sh_quote};
use super::{PackageFormat, PackageOptions};
use crate::workspace::{ProfileKind, Project, Selection};
use kargo_manifest::{JdkRequest, Manifest, NativeKind, TargetKind};
use kargo_maven::pom::{MAIN_CLASS_PROPERTY, Pom};
use kargo_maven::{DependencyRequest, MavenClient, ModuleId, Platform, Repository, Resolver};
use kargo_toolchain::native::Host;
use kargo_util::config::Config;
use kargo_util::fs::{copy_dir, dunce_canonicalize, join_paths, remove_dir_all_if_exists, write_string_atomic};
use kargo_util::{hash, process};

/// The file in `~/.kargo/install` that records the installed programs.
pub const REGISTRY: &str = ".installed.toml";

/// Where installed programs go: launchers and executables in `bin`, program files and the registry in `install`.
#[derive(Debug, Clone)]
pub struct Layout {
    pub bin: PathBuf,
    pub install: PathBuf,
}

impl Layout {
    /// `bin/` and `install/` in `root` (`--root`, `KARGO_INSTALL_ROOT`), or in the Kargo home by default.
    pub fn new(config: &Config, root: Option<&Path>) -> Self {
        match root {
            Some(root) => Layout { bin: root.join("bin"), install: root.join("install") },
            None => Layout { bin: config.bin_dir(), install: config.install_dir() },
        }
    }
}

/// Where a program is installed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    /// A project directory (`--path`).
    Path { path: PathBuf },
    /// A git repository (`--git`), at a tag, a branch or a revision; the default branch otherwise.
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
    },
    /// A published application: `group:artifact`, at a version (the newest by default), looked
    /// up in `repositories` and then Maven Central.
    Maven {
        module: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        repositories: Vec<String>,
    },
}

impl Source {
    /// How `--list` shows the source, e.g. `/work/tool` or `https://github.com/user/tool#1a2b3c4`.
    fn describe(&self, commit: Option<&str>) -> String {
        match self {
            Source::Path { path } => path.display().to_string(),
            Source::Git { url, .. } => match commit {
                Some(commit) => format!("{url}#{}", &commit[..commit.len().min(7)]),
                None => url.clone(),
            },
            Source::Maven { module, .. } => module.clone(),
        }
    }
}

/// How a JVM program is shipped (`--format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// The self-contained jar, run by the JDK of the build.
    #[default]
    Jar,
    /// A runtime image trimmed by `jlink`: runs without a Java installation.
    Jlink,
}

impl Format {
    fn package_format(self) -> PackageFormat {
        match self {
            Format::Jar => PackageFormat::Jar,
            Format::Jlink => PackageFormat::Jlink,
        }
    }
}

/// What `kargo install` was asked to do.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub source: Source,
    /// The member to install, when the source is a workspace (`-p`).
    pub package: Option<String>,
    /// Install only these programs (`--bin`); every entry point by default.
    pub bins: Vec<String>,
    /// `jvm` or `native`; the JVM when the project has both.
    pub target: Option<TargetKind>,
    pub format: Format,
    /// Replace a program that is already installed.
    pub force: bool,
    /// `--locked`: fail when `kargo.lock` is out of date.
    pub locked: bool,
    /// `--root`: install below this directory instead of the Kargo home.
    pub root: Option<PathBuf>,
}

/// One installed program, as `.installed.toml` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub source: Source,
    /// The member of a workspace that was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// `jvm` or `native`.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// The commit a git source was installed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// SHA-256 of the `kargo.lock` the program was built with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// The programs `--bin` selected; empty when every entry point was installed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<String>,
    /// The launchers and executables in `~/.kargo/bin`.
    pub binaries: Vec<String>,
}

impl Installed {
    /// The options that install this program again from where it came from (`kargo install --force <name>`).
    pub fn options(&self, locked: bool, root: Option<PathBuf>) -> InstallOptions {
        InstallOptions {
            source: self.source.clone(),
            package: self.package.clone(),
            bins: self.selected.clone(),
            target: match self.target.as_str() {
                "native" => Some(TargetKind::Native),
                _ => Some(TargetKind::Jvm),
            },
            format: self.format.unwrap_or_default(),
            force: true,
            locked,
            root,
        }
    }

    /// `tool v1.0.0 (/work/tool)`, the heading of a program in `kargo install --list`.
    pub fn describe(&self) -> String {
        format!("{} v{} ({})", self.name, self.version, self.source.describe(self.commit.as_deref()))
    }
}

/// The contents of `.installed.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default, rename = "tool")]
    tools: Vec<Installed>,
}

impl Registry {
    fn path(layout: &Layout) -> PathBuf {
        layout.install.join(REGISTRY)
    }

    fn load(layout: &Layout) -> Result<Self> {
        let path = Self::path(layout);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("failed to parse {}; delete it to forget the installed programs", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    fn save(&mut self, layout: &Layout) -> Result<()> {
        self.tools.sort_by(|a, b| a.name.cmp(&b.name));
        let text = format!("# Generated by `kargo install`; `kargo install --list` shows it.\n{}", toml::to_string(self)?);
        write_string_atomic(&Self::path(layout), &text)
    }

    fn find(&self, name: &str) -> Option<&Installed> {
        self.tools.iter().find(|t| t.name == name)
    }
}

/// The programs installed so far, by name.
pub fn installed(layout: &Layout) -> Result<Vec<Installed>> {
    let mut tools = Registry::load(layout)?.tools;
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tools)
}

/// The installed program called `name`.
pub fn recorded(layout: &Layout, name: &str) -> Result<Installed> {
    Registry::load(layout)?.find(name).cloned().with_context(|| {
        format!("`{name}` is not installed; install a project with `--path` or `--git`, or an application with `group:artifact`")
    })
}

/// What a built program is, before it is copied into place.
enum Built {
    /// A native executable.
    Native(PathBuf),
    /// A fat jar, run by `java` with these JVM options.
    Jar { jar: PathBuf, java: PathBuf, args: Vec<String> },
    /// The jars of a published application and its main class.
    Classpath { jars: Vec<PathBuf>, main: String, java: PathBuf },
    /// A `jlink` image directory.
    Image(PathBuf),
}

struct Program {
    /// The name of the launcher or executable.
    name: String,
    built: Built,
}

/// A built project or a resolved application, ready to be put on the `PATH`.
struct Prepared {
    name: String,
    version: String,
    target: TargetKind,
    format: Option<Format>,
    commit: Option<String>,
    lock: Option<String>,
    programs: Vec<Program>,
}

/// A temporary clone that is removed when it goes out of scope.
struct Checkout {
    dir: PathBuf,
    commit: String,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = remove_dir_all_if_exists(&self.dir);
    }
}

impl Checkout {
    fn clone_repo(config: &Config, url: &str, tag: Option<&str>, branch: Option<&str>, rev: Option<&str>) -> Result<Self> {
        if config.offline() {
            bail!("cannot clone {url} with --offline");
        }
        let git = process::find_on_path("git").context("`kargo install --git` clones with git, which is not on the PATH")?;
        let parent = config.home().join("cache").join("git");
        std::fs::create_dir_all(&parent).with_context(|| format!("failed to create {}", parent.display()))?;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut checkout = Checkout { dir: parent.join(format!("install-{}-{nanos}", std::process::id())), commit: String::new() };
        let shell = config.shell();
        shell.status("Cloning", url);
        let mut cmd = Command::new(&git);
        cmd.args(["clone", "--quiet"]);
        if rev.is_none() {
            cmd.args(["--depth", "1"]);
            if let Some(reference) = tag.or(branch) {
                cmd.args(["--branch", reference]);
            }
        }
        cmd.arg("--").arg(url).arg(&checkout.dir);
        process::run(shell, &mut cmd, "git clone")?;
        if let Some(rev) = rev {
            let mut cmd = Command::new(&git);
            cmd.arg("-C").arg(&checkout.dir).args(["checkout", "--quiet", rev]);
            process::run(shell, &mut cmd, "git checkout")?;
        }
        let output = Command::new(&git).arg("-C").arg(&checkout.dir).args(["rev-parse", "HEAD"]).output().context("failed to start git")?;
        checkout.commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(checkout)
    }
}

/// `kargo install`: builds or downloads the program and puts it on the `PATH`.
pub fn install(config: &Config, opts: &InstallOptions) -> Result<Installed> {
    let layout = Layout::new(config, opts.root.as_deref());
    let registry = Registry::load(&layout)?;
    let mut opts = opts.clone();
    let mut checkout: Option<Checkout> = None;
    let prepared = match opts.source.clone() {
        Source::Path { path } => {
            let manifest = manifest_in(&path)?;
            let dir = manifest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            opts.source = Source::Path { path: dunce_canonicalize(dir) };
            let project = Project::load(&manifest)?;
            from_project(config, &opts, &project, None, &registry)?
        }
        Source::Git { url, tag, branch, rev } => {
            let checkout = checkout.insert(Checkout::clone_repo(config, &url, tag.as_deref(), branch.as_deref(), rev.as_deref())?);
            let manifest = checkout.dir.join("kargo.toml");
            if !manifest.is_file() {
                bail!("{url} has no kargo.toml at its root; it is not a Kargo project");
            }
            let project = Project::load(&manifest)?;
            from_project(config, &opts, &project, Some(checkout.commit.clone()), &registry)?
        }
        Source::Maven { module, version, repositories } => {
            from_maven(config, &opts, module.parse().map_err(|e| anyhow::anyhow!("{e}"))?, version, &repositories, &registry)?
        }
    };
    let installed = commit(config, &layout, &opts, prepared, registry)?;
    drop(checkout);
    Ok(installed)
}

fn manifest_in(path: &Path) -> Result<PathBuf> {
    let manifest = if path.is_file() { path.to_path_buf() } else { path.join("kargo.toml") };
    if !manifest.is_file() {
        bail!("{} has no kargo.toml; give `--path` the directory of the project to install", path.display());
    }
    Ok(manifest)
}

/// Fails when `name` is installed already and `force` does not allow replacing it.
fn ensure_free(registry: &Registry, name: &str, force: bool) -> Result<()> {
    match registry.find(name) {
        Some(tool) if !force => bail!(
            "`{name}` v{} is already installed; pass `--force` to reinstall it, or `kargo install --force {name}` to upgrade it from its source",
            tool.version
        ),
        _ => Ok(()),
    }
}

/// The target that gets installed: `requested`, else the JVM, else Native.
fn pick_target(manifest: &Manifest, requested: Option<TargetKind>) -> Result<TargetKind> {
    let targets = &manifest.targets;
    match requested {
        Some(t @ (TargetKind::Jvm | TargetKind::Native)) if targets.is_enabled(t) => Ok(t),
        Some(t @ (TargetKind::Jvm | TargetKind::Native)) => bail!("target `{t}` is not enabled in kargo.toml"),
        Some(t) => bail!("`kargo install` installs programs for the jvm and native targets, not {t}"),
        None if targets.is_enabled(TargetKind::Jvm) => Ok(TargetKind::Jvm),
        None if targets.is_enabled(TargetKind::Native) => Ok(TargetKind::Native),
        None => bail!("`kargo install` installs programs for the jvm and native targets, and this project enables neither"),
    }
}

fn from_project(
    config: &Config,
    opts: &InstallOptions,
    project: &Project,
    commit: Option<String>,
    registry: &Registry,
) -> Result<Prepared> {
    let selection = Selection { packages: opts.package.iter().cloned().collect(), ..Default::default() };
    let index = project.select_one(&selection, "`kargo install`")?;
    let manifest = project.package(index).manifest();
    let (name, version) = (manifest.package.name.clone(), manifest.package.version.clone());
    ensure_free(registry, &name, opts.force)?;
    let target = pick_target(manifest, opts.target)?;
    if target == TargetKind::Native && opts.format != Format::Jar {
        bail!("`--format {}` packages a JVM program and does not apply to the native target", format_name(opts.format));
    }
    let mut ctx = BuildContext::new(config, project, index, ProfileKind::Release)?;
    ctx.locked = opts.locked;
    let programs = match target {
        TargetKind::Native => native_programs(&ctx, opts)?,
        _ => jvm_programs(&ctx, opts)?,
    };
    ctx.save_lockfile()?;
    let lock = std::fs::read(project.lockfile_path()).ok().map(|bytes| hash::sha256_bytes(&bytes));
    let format = (target == TargetKind::Jvm).then_some(opts.format);
    Ok(Prepared { name, version, target, format, commit, lock, programs })
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Jar => "jar",
        Format::Jlink => "jlink",
    }
}

/// The JVM entry points to install, as `(program name, [[bin]] name)`: the `--bin` ones, else every
/// `[[bin]]` with a JVM main class and the package's own main class.
fn jvm_entry_points(ctx: &BuildContext, opts: &InstallOptions, jvm: &build::JvmBuild) -> Result<Vec<(String, Option<String>)>> {
    let manifest = ctx.ws.manifest();
    let own = manifest.package.name.as_str();
    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    if opts.bins.is_empty() {
        for bin in &manifest.bins {
            if build::bin_main_class(ctx.ws.root(), bin).is_ok() {
                entries.push((bin.name.clone(), Some(bin.name.clone())));
            }
        }
        if jvm.main_class.is_some() && !entries.iter().any(|(name, _)| name == own) {
            entries.push((own.to_owned(), None));
        }
    } else {
        for name in &opts.bins {
            if manifest.bin(name).is_some() {
                entries.push((name.clone(), Some(name.clone())));
            } else if name == own && jvm.main_class.is_some() {
                entries.push((own.to_owned(), None));
            } else if name == own {
                bail!("`{own}` has no main class to install: {}", jvm.main_class_error.as_deref().unwrap_or("set [target.jvm] main-class"));
            } else {
                build::find_bin(manifest, name)?;
            }
        }
    }
    if entries.is_empty() {
        bail!(
            "`{own}` has no program to install: {}; declare a [[bin]] or a `main` function",
            jvm.main_class_error.as_deref().unwrap_or("it declares no [[bin]] with a main class")
        );
    }
    Ok(entries)
}

fn jvm_programs(ctx: &BuildContext, opts: &InstallOptions) -> Result<Vec<Program>> {
    let jvm = build::build_jvm(ctx)?;
    let entries = jvm_entry_points(ctx, opts, &jvm)?;
    let java = java_executable(&ctx.jdk()?.home);
    let mut programs = Vec::new();
    for (name, bin) in entries {
        let package =
            PackageOptions { target: Some(TargetKind::Jvm), bin, format: Some(opts.format.package_format()), ..Default::default() };
        let file = super::package(ctx, &package)?.into_iter().next().context("`kargo package` made no file")?;
        let built = match opts.format {
            Format::Jar => {
                let app = App { name: &name, main_class: String::new(), jvm: &jvm, dir: PathBuf::new() };
                Built::Jar { jar: file, java: java.clone(), args: app_jvm_args(ctx, &app, None)? }
            }
            Format::Jlink => Built::Image(file),
        };
        programs.push(Program { name, built });
    }
    Ok(programs)
}

fn native_programs(ctx: &BuildContext, opts: &InstallOptions) -> Result<Vec<Program>> {
    let build_opts = BuildOptions { targets: vec![TargetKind::Native], profile: ProfileKind::Release, ..Default::default() };
    let results = build::build(ctx, &build_opts)?;
    let host = Host::current()?.konan_target;
    let builds: Vec<_> = results.native.iter().filter(|b| b.konan_target == host).collect();
    if builds.is_empty() {
        bail!("the native target has no build for this host ({host}); add \"host\" to `platforms`");
    }
    if builds.iter().any(|b| b.kind != NativeKind::Executable) {
        bail!("the native target builds a library, not an executable");
    }
    let own = &ctx.ws.manifest().package.name;
    let all: Vec<Program> = builds
        .iter()
        .map(|b| Program { name: b.bin.clone().unwrap_or_else(|| own.clone()), built: Built::Native(b.artifact.clone()) })
        .collect();
    let available: Vec<String> = all.iter().map(|p| p.name.clone()).collect();
    for name in &opts.bins {
        if !available.contains(name) {
            bail!("no program named `{name}` to install; the native target builds: {}", available.join(", "));
        }
    }
    let programs: Vec<Program> = all.into_iter().filter(|p| opts.bins.is_empty() || opts.bins.contains(&p.name)).collect();
    if programs.is_empty() {
        bail!("`{own}` builds no executable to install; add a `main` function or a [[bin]]");
    }
    Ok(programs)
}

fn from_maven(
    config: &Config,
    opts: &InstallOptions,
    module: ModuleId,
    version: Option<String>,
    repositories: &[String],
    registry: &Registry,
) -> Result<Prepared> {
    if opts.format != Format::Jar {
        bail!("`--format {}` needs a project to build; use it with `--path` or `--git`", format_name(opts.format));
    }
    if !matches!(opts.target, None | Some(TargetKind::Jvm)) {
        bail!("an application from Maven runs on the JVM; `--target` applies to projects installed with `--path` or `--git`");
    }
    if opts.package.is_some() {
        bail!("`-p` picks a member of a workspace, which `--path` and `--git` install; a Maven application is one module");
    }
    let name = module.artifact.clone();
    ensure_free(registry, &name, opts.force)?;
    let mut repos: Vec<Repository> = repositories.iter().map(|url| Repository::new("install", url.clone())).collect();
    repos.push(Repository::maven_central());
    let client = MavenClient::new(config, repos);
    let version = match version {
        Some(version) => version,
        None => client.latest_version(&module, false)?.to_string(),
    };
    config.shell().status("Resolving", format!("{module}:{version}"));
    let request = DependencyRequest::new(module.clone(), Some(version.clone()));
    let resolution =
        Resolver::new(&client, Platform::Jvm).resolve(&[request]).with_context(|| format!("failed to resolve {module}:{version}"))?;
    let is_jar = |p: &PathBuf| p.extension().is_some_and(|e| e == "jar");
    let jars: Vec<PathBuf> = resolution.files().into_iter().filter(is_jar).collect();
    let root = resolution
        .node(&module)
        .and_then(|node| node.files.iter().find(|f| is_jar(f)))
        .cloned()
        .with_context(|| format!("{module}:{version} has no jar to run"))?;
    let main = main_class_of(&client, &module, &version, &root)?;
    let jdk = kargo_toolchain::jdks::resolve(config, &JdkRequest::System)?;
    let programs = vec![Program { name, built: Built::Classpath { jars, main, java: java_executable(&jdk.home) } }];
    Ok(Prepared { name: module.artifact.clone(), version, target: TargetKind::Jvm, format: None, commit: None, lock: None, programs })
}

/// The class to run for a published application: the `Main-Class` of its jar, else the
/// `kargo.main-class` property `kargo publish` writes into its POM.
fn main_class_of(client: &MavenClient, module: &ModuleId, version: &str, jar: &Path) -> Result<String> {
    if let Some(main) = jar_main_class(jar)? {
        return Ok(main);
    }
    // A multiplatform application publishes the property with its JVM module.
    for artifact in [module.artifact.clone(), format!("{}-jvm", module.artifact)] {
        let rel = format!("{}/{version}/{artifact}-{version}.pom", ModuleId::new(&module.group, &artifact).repo_dir());
        // A module that is not there (offline, the `-jvm` of a plain module) is only a POM without the property.
        if let Some(text) = client.fetch_text(&rel).ok().flatten()
            && let Ok(pom) = Pom::parse(&text)
            && let Some(main) = pom.properties.get(MAIN_CLASS_PROPERTY).filter(|m| !m.is_empty())
        {
            return Ok(main.clone());
        }
    }
    bail!(
        "{module}:{version} does not say which class to run: its jar has no `Main-Class` and its POM no `{MAIN_CLASS_PROPERTY}` property; \
         it is probably a library, not a program"
    )
}

/// The `Main-Class` in the manifest of the jar at `path`.
fn jar_main_class(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    let file = std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("{} is not a jar", path.display()))?;
    let Ok(mut entry) = archive.by_name("META-INF/MANIFEST.MF") else { return Ok(None) };
    let mut text = String::new();
    entry.read_to_string(&mut text).with_context(|| format!("failed to read the manifest of {}", path.display()))?;
    Ok(parse_main_class(&text))
}

/// `Main-Class` from the text of a jar manifest, whose long lines continue on lines that start with a space.
fn parse_main_class(manifest: &str) -> Option<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in manifest.lines() {
        match (line.strip_prefix(' '), unfolded.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => unfolded.push(line.to_owned()),
        }
    }
    unfolded.iter().find_map(|line| line.strip_prefix("Main-Class:")).map(|main| main.trim().to_owned()).filter(|main| !main.is_empty())
}

fn java_executable(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
}

/// The files a program called `name` can be in `bin_dir`: a launcher script, a `.cmd` launcher, an executable.
fn binary_files(bin_dir: &Path, name: &str) -> [PathBuf; 3] {
    [bin_dir.join(name), bin_dir.join(format!("{name}.cmd")), bin_dir.join(format!("{name}.exe"))]
}

fn remove_binary(bin_dir: &Path, name: &str) -> Result<()> {
    for file in binary_files(bin_dir, name) {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("failed to remove {}", file.display())),
        }
    }
    Ok(())
}

/// `bin/<name>`: runs `command` with the arguments of the launcher.
fn unix_script(command: &[String]) -> String {
    let command: Vec<String> = command.iter().map(|a| sh_quote(a)).collect();
    format!("#!/bin/sh\n# Generated by `kargo install`.\nexec {} \"$@\"\n", command.join(" "))
}

/// `bin/<name>.cmd`, with CRLF line endings. `batch` is for a command that is itself a batch file,
/// which only continues the script when it is `call`ed.
fn windows_script(command: &[String], batch: bool) -> String {
    let command: Vec<String> = command.iter().map(|a| bat_quote(a)).collect();
    let lines = [
        "@echo off".to_owned(),
        "rem Generated by `kargo install`.".to_owned(),
        format!("{}{} %*", if batch { "call " } else { "" }, command.join(" ")),
        "exit /b %ERRORLEVEL%".to_owned(),
    ];
    lines.iter().map(|l| format!("{l}\r\n")).collect()
}

/// Writes the launcher of `name` that runs `command`.
fn write_launcher(bin_dir: &Path, name: &str, command: &[String], batch: bool) -> Result<PathBuf> {
    if cfg!(windows) {
        let path = bin_dir.join(format!("{name}.cmd"));
        write_string_atomic(&path, &windows_script(command, batch))?;
        Ok(path)
    } else {
        let path = bin_dir.join(name);
        write_string_atomic(&path, &unix_script(command))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(path)
    }
}

/// Puts the built programs in place, replaces the ones installed before and records the result.
fn commit(config: &Config, layout: &Layout, opts: &InstallOptions, prepared: Prepared, mut registry: Registry) -> Result<Installed> {
    let shell = config.shell();
    let bin_dir = layout.bin.clone();
    let Prepared { name, version, target, format, commit, lock, programs } = prepared;
    let previous = registry.find(&name).cloned();
    ensure_free(&registry, &name, opts.force)?;
    for program in &programs {
        if let Some(owner) = registry.tools.iter().find(|t| t.name != name && t.binaries.contains(&program.name)) {
            bail!("`{}` is already installed as part of `{}`; uninstall that first", program.name, owner.name);
        }
        let ours = previous.as_ref().is_some_and(|p| p.binaries.contains(&program.name));
        let taken = binary_files(&bin_dir, &program.name).into_iter().find(|f| f.exists());
        if let Some(file) = taken.filter(|_| !ours && !opts.force) {
            bail!("{} exists and was not installed by Kargo; remove it or pass `--force` to overwrite it", file.display());
        }
    }

    std::fs::create_dir_all(&bin_dir).with_context(|| format!("failed to create {}", bin_dir.display()))?;
    let root = layout.install.join(&name);
    let dir = root.join(&version);
    remove_dir_all_if_exists(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    // Programs the new install no longer has, and launchers of another kind, go before the new ones are written.
    for old in previous.iter().flat_map(|p| p.binaries.iter()).chain(programs.iter().map(|p| &p.name)) {
        remove_binary(&bin_dir, old)?;
    }

    let mut binaries = Vec::new();
    for program in &programs {
        let placed = match &program.built {
            Built::Native(artifact) => {
                let dest = bin_dir.join(if cfg!(windows) { format!("{}.exe", program.name) } else { program.name.clone() });
                std::fs::copy(artifact, &dest).with_context(|| format!("failed to copy {} to {}", artifact.display(), dest.display()))?;
                dest
            }
            Built::Jar { jar, java, args } => {
                let file = jar.file_name().context("the jar has no file name")?;
                let dest = dir.join(file);
                std::fs::copy(jar, &dest).with_context(|| format!("failed to copy {}", jar.display()))?;
                let mut command = vec![java.to_string_lossy().into_owned()];
                command.extend(args.iter().cloned());
                command.extend(["-jar".to_owned(), dest.to_string_lossy().into_owned()]);
                write_launcher(&bin_dir, &program.name, &command, false)?
            }
            Built::Classpath { jars, main, java } => {
                let lib = dir.join("lib");
                std::fs::create_dir_all(&lib).with_context(|| format!("failed to create {}", lib.display()))?;
                let first = jars.first().and_then(|j| j.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let names = jar_names(&first, jars.get(1..).unwrap_or(&[]));
                let mut classpath = Vec::new();
                for (jar, file) in jars.iter().zip(&names) {
                    let dest = lib.join(file);
                    std::fs::copy(jar, &dest).with_context(|| format!("failed to copy {}", jar.display()))?;
                    classpath.push(dest);
                }
                let command = vec![java.to_string_lossy().into_owned(), "-cp".to_owned(), join_paths(&classpath), main.clone()];
                write_launcher(&bin_dir, &program.name, &command, false)?
            }
            Built::Image(image) => {
                let dest = dir.join(&program.name);
                copy_dir(image, &dest)?;
                let launcher = dest.join("bin").join(if cfg!(windows) { format!("{}.bat", program.name) } else { program.name.clone() });
                write_launcher(&bin_dir, &program.name, &[launcher.to_string_lossy().into_owned()], true)?
            }
        };
        shell.status("Installing", placed.display());
        binaries.push(program.name.clone());
    }

    // Other versions of the same program are no longer used.
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten().filter(|e| e.path() != dir) {
            let _ = remove_dir_all_if_exists(&entry.path());
        }
    }

    let installed = Installed {
        name: name.clone(),
        version,
        source: opts.source.clone(),
        package: opts.package.clone(),
        target: target.as_str().to_owned(),
        format,
        commit,
        lock,
        selected: opts.bins.clone(),
        binaries,
    };
    registry.tools.retain(|t| t.name != name);
    registry.tools.push(installed.clone());
    registry.save(layout)?;
    let list: Vec<String> = installed.binaries.iter().map(|b| format!("`{b}`")).collect();
    shell.status(
        "Installed",
        format!(
            "package `{} v{}` ({} {})",
            installed.name,
            installed.version,
            if list.len() == 1 { "executable" } else { "executables" },
            list.join(", ")
        ),
    );
    if !bin_dir_on_path(&bin_dir) {
        shell.warn(format!("{} is not in your PATH; add it to run the installed programs", bin_dir.display()));
        shell.note(path_hint(&bin_dir));
    }
    Ok(installed)
}

/// Whether `dir` is one of the directories on the `PATH`.
pub fn bin_dir_on_path(dir: &Path) -> bool {
    let wanted = dunce_canonicalize(dir);
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|entry| dunce_canonicalize(&entry) == wanted))
}

/// How to add `dir` to the `PATH` of the shell people most likely use.
fn path_hint(dir: &Path) -> String {
    if cfg!(windows) {
        format!("in PowerShell: [Environment]::SetEnvironmentVariable('Path', \"$env:Path;{}\", 'User')", dir.display())
    } else {
        format!("add `export PATH=\"{}:$PATH\"` to your shell's startup file", dir.display())
    }
}

/// `kargo uninstall`: removes the programs `names`, their launchers and files.
pub fn uninstall(config: &Config, layout: &Layout, names: &[String]) -> Result<Vec<Installed>> {
    let mut registry = Registry::load(layout)?;
    for name in names {
        if registry.find(name).is_none() {
            bail!("`{name}` is not installed; `kargo install --list` shows what is");
        }
    }
    let bin_dir = layout.bin.clone();
    let mut removed = Vec::new();
    for name in names {
        let Some(position) = registry.tools.iter().position(|t| &t.name == name) else { continue };
        let tool = registry.tools.remove(position);
        for binary in &tool.binaries {
            remove_binary(&bin_dir, binary)?;
        }
        remove_dir_all_if_exists(&layout.install.join(&tool.name))?;
        config.shell().status("Removed", format!("package `{} v{}`", tool.name, tool.version));
        removed.push(tool);
    }
    registry.save(layout)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(home: &Path) -> Config {
        Config::new(home.to_path_buf(), Shell::new(Verbosity::Quiet))
    }

    #[test]
    fn reads_the_main_class_of_a_manifest() {
        let manifest = "Manifest-Version: 1.0\r\nMain-Class: com.example.app\r\n .MainKt\r\nImplementation-Title: app\r\n";
        assert_eq!(parse_main_class(manifest).as_deref(), Some("com.example.app.MainKt"));
        assert_eq!(parse_main_class("Manifest-Version: 1.0\n"), None);
        assert_eq!(parse_main_class("Main-Class:\n"), None);
    }

    #[test]
    fn launchers_quote_their_arguments() {
        let command = ["/jdk/bin/java".to_owned(), "-Dname=a b".to_owned(), "-jar".to_owned(), "/x/tool-1.0-all.jar".to_owned()];
        assert_eq!(
            unix_script(&command),
            "#!/bin/sh\n# Generated by `kargo install`.\nexec /jdk/bin/java '-Dname=a b' -jar /x/tool-1.0-all.jar \"$@\"\n"
        );
        let script = windows_script(&command, false);
        assert!(script.contains("/jdk/bin/java \"-Dname=a b\" -jar /x/tool-1.0-all.jar %*\r\n"), "{script}");
        assert!(script.starts_with("@echo off\r\n") && script.ends_with("exit /b %ERRORLEVEL%\r\n"), "{script}");
        assert!(windows_script(&["C:\\img\\bin\\tool.bat".to_owned()], true).contains("call C:\\img\\bin\\tool.bat %*"));
    }

    #[test]
    fn the_layout_defaults_to_the_kargo_home() {
        let home = tempfile::tempdir().unwrap();
        let config = config(home.path());
        let layout = Layout::new(&config, None);
        assert_eq!((layout.bin.as_path(), layout.install.as_path()), (config.bin_dir().as_path(), config.install_dir().as_path()));
        let root = home.path().join("root");
        let layout = Layout::new(&config, Some(&root));
        assert_eq!((layout.bin, layout.install), (root.join("bin"), root.join("install")));
    }

    #[test]
    fn the_registry_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::new(&config(home.path()), None);
        assert!(installed(&layout).unwrap().is_empty());
        let tool = Installed {
            name: "tool".to_owned(),
            version: "1.0.0".to_owned(),
            source: Source::Git { url: "https://example.com/tool".to_owned(), tag: Some("v1".to_owned()), branch: None, rev: None },
            package: None,
            target: "jvm".to_owned(),
            format: Some(Format::Jar),
            commit: Some("1234567890".to_owned()),
            lock: Some("abc".to_owned()),
            selected: Vec::new(),
            binaries: vec!["tool".to_owned(), "tool-admin".to_owned()],
        };
        let maven = Installed {
            name: "app".to_owned(),
            source: Source::Maven { module: "com.example:app".to_owned(), version: None, repositories: vec!["file:///repo".to_owned()] },
            commit: None,
            lock: None,
            format: None,
            binaries: vec!["app".to_owned()],
            ..tool.clone()
        };
        let mut registry = Registry { tools: vec![tool.clone(), maven.clone()] };
        registry.save(&layout).unwrap();
        assert_eq!(installed(&layout).unwrap(), vec![maven, tool.clone()]);
        assert_eq!(recorded(&layout, "tool").unwrap(), tool);
        assert!(recorded(&layout, "missing").unwrap_err().to_string().contains("not installed"));
        assert_eq!(tool.describe(), "tool v1.0.0 (https://example.com/tool#1234567)");
        let options = tool.options(true, Some(PathBuf::from("root")));
        assert!(options.force && options.locked && options.target == Some(TargetKind::Jvm));
        assert_eq!(options.root, Some(PathBuf::from("root")));
    }

    fn prepared(name: &str, version: &str, programs: Vec<Program>) -> Prepared {
        Prepared {
            name: name.to_owned(),
            version: version.to_owned(),
            target: TargetKind::Native,
            format: None,
            commit: None,
            lock: None,
            programs,
        }
    }

    fn options(path: &Path, force: bool) -> InstallOptions {
        InstallOptions {
            source: Source::Path { path: path.to_path_buf() },
            package: None,
            bins: Vec::new(),
            target: None,
            format: Format::Jar,
            force,
            locked: false,
            root: None,
        }
    }

    /// The file `name` is installed as in `layout`: an executable is `name.exe` on Windows.
    fn installed_file(layout: &Layout, name: &str) -> Option<PathBuf> {
        binary_files(&layout.bin, name).into_iter().find(|f| f.exists())
    }

    #[test]
    fn installs_replaces_and_removes_programs() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = config(home.path());
        let layout = Layout::new(&config, None);
        let exe = work.path().join("tool-exe");
        std::fs::write(&exe, "v1").unwrap();
        let native = |exe: &Path| vec![Program { name: "tool".to_owned(), built: Built::Native(exe.to_path_buf()) }];

        let opts = options(work.path(), false);
        commit(&config, &layout, &opts, prepared("tool", "1.0.0", native(&exe)), Registry::load(&layout).unwrap()).unwrap();
        let file = installed_file(&layout, "tool").expect("the executable is in the bin directory");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
        assert_eq!(installed(&layout).unwrap().len(), 1);

        // A second install needs --force.
        let err = commit(&config, &layout, &opts, prepared("tool", "1.1.0", native(&exe)), Registry::load(&layout).unwrap()).unwrap_err();
        assert!(err.to_string().contains("already installed"), "{err:#}");
        std::fs::write(&exe, "v2").unwrap();
        let forced = options(work.path(), true);
        commit(&config, &layout, &forced, prepared("tool", "1.1.0", native(&exe)), Registry::load(&layout).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2");
        assert_eq!(installed(&layout).unwrap()[0].version, "1.1.0");

        // Another program may not take over its executable.
        let err = commit(&config, &layout, &opts, prepared("other", "1.0.0", native(&exe)), Registry::load(&layout).unwrap()).unwrap_err();
        assert!(err.to_string().contains("installed as part of `tool`"), "{err:#}");

        let removed = uninstall(&config, &layout, &["tool".to_owned()]).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!file.exists() && installed(&layout).unwrap().is_empty());
        assert!(!layout.install.join("tool").exists());
        assert!(uninstall(&config, &layout, &["tool".to_owned()]).unwrap_err().to_string().contains("not installed"));
    }

    #[test]
    fn leaves_files_it_did_not_install_alone() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = config(home.path());
        let layout = Layout::new(&config, None);
        std::fs::create_dir_all(&layout.bin).unwrap();
        let existing = binary_files(&layout.bin, "tool")[0].clone();
        std::fs::write(&existing, "somebody else's").unwrap();
        let exe = work.path().join("tool-exe");
        std::fs::write(&exe, "mine").unwrap();
        let programs = || vec![Program { name: "tool".to_owned(), built: Built::Native(exe.clone()) }];
        let err =
            commit(&config, &layout, &options(work.path(), false), prepared("tool", "1.0.0", programs()), Registry::default()).unwrap_err();
        assert!(err.to_string().contains("was not installed by Kargo"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "somebody else's");
        commit(&config, &layout, &options(work.path(), true), prepared("tool", "1.0.0", programs()), Registry::default()).unwrap();
        assert_eq!(std::fs::read_to_string(installed_file(&layout, "tool").unwrap()).unwrap(), "mine");
    }

    #[test]
    fn writes_launchers_for_jars_and_classpaths() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = config(home.path());
        let layout = Layout::new(&config, Some(&work.path().join("root")));
        let jar = work.path().join("tool-1.0.0-all.jar");
        std::fs::write(&jar, "jar").unwrap();
        let java = PathBuf::from("/jdk/bin/java");
        let programs = vec![
            Program {
                name: "tool".to_owned(),
                built: Built::Jar { jar: jar.clone(), java: java.clone(), args: vec!["-Xmx1g".to_owned()] },
            },
            Program {
                name: "app".to_owned(),
                built: Built::Classpath { jars: vec![jar.clone(), jar.clone()], main: "com.example.Main".to_owned(), java },
            },
        ];
        commit(&config, &layout, &options(work.path(), false), prepared("tool", "1.0.0", programs), Registry::default()).unwrap();
        let launcher = |name: &str| std::fs::read_to_string(installed_file(&layout, name).unwrap()).unwrap();
        let tool = launcher("tool");
        assert!(tool.contains("java") && tool.contains("-Xmx1g") && tool.contains("tool-1.0.0-all.jar"), "{tool}");
        let app = launcher("app");
        assert!(app.contains("-cp") && app.contains("com.example.Main") && app.contains("tool-1.0.0-all-2.jar"), "{app}");
        assert!(layout.install.join("tool").join("1.0.0").join("tool-1.0.0-all.jar").is_file());
        assert_eq!(installed(&layout).unwrap()[0].binaries, ["tool", "app"]);
        assert!(!config.bin_dir().exists(), "--root keeps the Kargo home untouched");
    }
}
