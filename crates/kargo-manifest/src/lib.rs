//! The `kargo.toml` manifest.
//!
//! ```toml
//! [package]
//! name = "hello"
//! version = "0.1.0"
//! group = "com.example"
//!
//! [kotlin]
//! version = "2.4.21"
//!
//! [target.jvm]
//! jvm-target = "21"
//!
//! [dependencies]
//! "org.jetbrains.kotlinx:kotlinx-coroutines-core" = "1.11.0"
//!
//! [frameworks]
//! serialization = true
//! ```

pub mod edit;
mod raw;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use kargo_maven::repo::Credentials;
use kargo_maven::{CoordSpec, ModuleId, Repository, VersionReq};

pub use raw::{AndroidConfig, ComposeConfig, KtorConfig, LombokConfig, SerializationConfig, SpringBootConfig};

pub const MANIFEST_NAME: &str = "kargo.toml";
pub const LOCKFILE_NAME: &str = "kargo.lock";

/// Default Kotlin version for new projects.
pub const DEFAULT_KOTLIN_VERSION: &str = "2.4.21";
/// Default JVM bytecode target. When `jvm-target` is not set, it is lowered to
/// the version of the JDK that builds the project if that JDK is older.
pub const DEFAULT_JVM_TARGET: &str = "25";

/// The oldest bytecode target Kotlin accepts: Java 8, which it spells `1.8`.
const MIN_JVM_TARGET: u32 = 8;

/// Checks a `jvm-target` and returns it as the Kotlin compiler spells it: `8` and `1.8` both
/// become `1.8`, and Java 9 and newer stay plain numbers (`17`). `javac --release` takes `8`.
pub fn normalize_jvm_target(value: &str) -> Result<String> {
    let value = value.trim();
    let major = match value {
        "1.8" => Some(MIN_JVM_TARGET),
        _ => value.parse::<u32>().ok().filter(|m| *m >= MIN_JVM_TARGET && m.to_string() == value),
    };
    match major {
        Some(major) => Ok(jvm_target_spelling(major)),
        None => {
            bail!("invalid [target.jvm] jvm-target `{value}` (expected a Java version: `8` or `1.8`, or `9` and up, like `17` or `21`)")
        }
    }
}

/// Java `major` as Kotlin's `-jvm-target` spells it.
fn jvm_target_spelling(major: u32) -> String {
    if major == MIN_JVM_TARGET { "1.8".to_owned() } else { major.to_string() }
}

/// The Java version of a normalized `jvm-target`.
fn jvm_target_major(target: &str) -> u32 {
    target.strip_prefix("1.").unwrap_or(target).parse().unwrap_or(0)
}

/// A compilation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TargetKind {
    Jvm,
    Wasm,
    Native,
    Android,
}

impl TargetKind {
    pub const ALL: [TargetKind; 4] = [TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native, TargetKind::Android];

    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Jvm => "jvm",
            TargetKind::Wasm => "wasm",
            TargetKind::Native => "native",
            TargetKind::Android => "android",
        }
    }

    /// Directory holding sources specific to this target, next to `src/`.
    pub fn source_dir_name(self) -> String {
        format!("src-{}", self.as_str())
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TargetKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "jvm" => Ok(TargetKind::Jvm),
            "wasm" | "wasm-js" | "wasmjs" => Ok(TargetKind::Wasm),
            "native" => Ok(TargetKind::Native),
            "android" => Ok(TargetKind::Android),
            other => bail!("unknown target `{other}` (expected one of: jvm, wasm, native, android)"),
        }
    }
}

/// Which classpaths a dependency belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// Needed to compile and at runtime (the default).
    #[default]
    Compile,
    /// Only needed at runtime (e.g. JDBC drivers, logging backends).
    Runtime,
    /// Only needed to compile; not packaged (e.g. Lombok, annotations).
    Provided,
    /// A Java annotation processor: goes on javac's `-processorpath` only,
    /// never on the compile or runtime classpath (e.g. AutoValue, MapStruct).
    Processor,
    /// A Kotlin Symbol Processing (KSP) processor: runs on Kotlin and Java sources
    /// before compilation, never on the compile or runtime classpath (e.g. Moshi, Room).
    Ksp,
}

impl Scope {
    /// The name written in `kargo.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Compile => "compile",
            Scope::Runtime => "runtime",
            Scope::Provided => "provided",
            Scope::Processor => "processor",
            Scope::Ksp => "ksp",
        }
    }
}

impl FromStr for Scope {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "compile" | "implementation" | "api" => Ok(Scope::Compile),
            "runtime" | "runtime-only" => Ok(Scope::Runtime),
            "provided" | "compile-only" => Ok(Scope::Provided),
            "processor" | "annotation-processor" => Ok(Scope::Processor),
            "ksp" => Ok(Scope::Ksp),
            other => bail!("unknown dependency scope `{other}` (expected compile, runtime, provided, processor or ksp)"),
        }
    }
}

/// A validated dependency declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The key used in the manifest (for error messages and `kargo remove`).
    pub name: String,
    pub module: ModuleId,
    pub version: Option<String>,
    pub scope: Scope,
    pub classifier: Option<String>,
    pub extension: Option<String>,
    pub exclusions: Vec<ModuleId>,
    pub force: bool,
    pub transitive: bool,
    /// A BOM whose managed versions apply to the whole project.
    pub bom: bool,
    /// Part of this package's API (`exported = true`): packages that depend on this one through a
    /// `path` dependency compile against it too, and its POM scope is `compile`. Otherwise they
    /// only get it at runtime.
    pub exported: bool,
}

impl Dependency {
    pub fn new(module: ModuleId, version: Option<String>) -> Self {
        Self {
            name: module.to_string(),
            module,
            version,
            scope: Scope::Compile,
            classifier: None,
            extension: None,
            exclusions: Vec::new(),
            force: false,
            transitive: true,
            bom: false,
            exported: false,
        }
    }

    /// Whether both declare the same artifact: `group:artifact:classifier:extension`. The version is
    /// not part of the key, and one module may be declared once per classifier (native libraries).
    pub fn same_artifact(&self, other: &Dependency) -> bool {
        self.module == other.module && self.classifier == other.classifier && self.extension == other.extension
    }
}

/// Another Kargo package that this one depends on, built from source:
/// `name = { path = "../name" }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDependency {
    /// The manifest key, which must be the dependency's package name.
    pub name: String,
    /// As written: relative to the declaring package's directory, or absolute.
    pub path: PathBuf,
    pub scope: Scope,
    /// Declared in `[dev-dependencies]`.
    pub dev: bool,
    /// Declared in `[target.<target>.dependencies]`.
    pub target: Option<TargetKind>,
    /// Part of the declaring package's API (`exported = true`): its consumers compile against
    /// this package too, instead of only getting it at runtime.
    pub exported: bool,
    /// Also brings the dependency's test fixtures (`test-fixtures = true`, only in `[dev-dependencies]`).
    pub test_fixtures: bool,
}

impl PathDependency {
    /// Whether this dependency applies to `target`'s main (or, with `dev`, test) compilation.
    pub fn applies_to(&self, target: TargetKind, dev: bool) -> bool {
        self.dev == dev && self.target.is_none_or(|t| t == target)
    }
}

/// Kotlin compiler settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KotlinSettings {
    pub version: String,
    /// Whether the version was set in `[kotlin]` or `[workspace.kotlin]` (otherwise it is the default).
    pub version_set: bool,
    pub language_version: Option<String>,
    pub api_version: Option<String>,
    /// KSP version (`[kotlin] ksp`); `None` uses the version Kargo pins.
    pub ksp_version: Option<String>,
    pub opt_in: Vec<String>,
    pub compiler_args: Vec<String>,
    pub warnings_as_errors: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmTarget {
    pub jvm_target: String,
    /// Whether `jvm-target` was written in `kargo.toml` (otherwise it is the default).
    pub jvm_target_set: bool,
    pub main_class: Option<String>,
    pub jvm_args: Vec<String>,
    /// Extra `javac` arguments for Java sources (`javac-args`).
    pub javac_args: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

impl JvmTarget {
    /// The bytecode target to compile for with a JDK of major version `jdk`: the
    /// configured `jvm-target`, or the default lowered to `jdk` when that JDK is older.
    /// Fails when the configured target is newer than `jdk`, which cannot compile for it.
    pub fn target_for(&self, jdk: u32) -> Result<String> {
        let major = jvm_target_major(&self.jvm_target);
        if major > jdk && !self.jvm_target_set {
            return Ok(jvm_target_spelling(jdk));
        }
        if major > jdk {
            bail!(
                "[target.jvm] jvm-target {major} is newer than JDK {jdk}, which builds the project; \
                 set `[toolchain] jdk = \"{major}\"` in kargo.toml to build with a newer JDK, or lower `jvm-target`"
            );
        }
        Ok(self.jvm_target.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WasmEnvironment {
    /// Node.js (the default); the module can also be loaded by a bundler or a page.
    #[default]
    Js,
    /// A web browser: Kargo adds an HTML page, serves it for `kargo run` and runs tests in a headless browser.
    Browser,
    /// The WebAssembly System Interface.
    Wasi,
}

impl WasmEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            WasmEnvironment::Js => "js",
            WasmEnvironment::Browser => "browser",
            WasmEnvironment::Wasi => "wasi",
        }
    }

    /// The Kotlin/Wasm target (`-Xwasm-target=wasm-<platform>`) and Maven variant:
    /// `js` for Node.js and browsers alike, or `wasi`.
    pub fn platform(self) -> &'static str {
        match self {
            WasmEnvironment::Js | WasmEnvironment::Browser => "js",
            WasmEnvironment::Wasi => "wasi",
        }
    }
}

/// What a Wasm build produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WasmKind {
    /// A program with a `main` function (the default).
    #[default]
    Application,
    /// An npm package: `@JsExport` functions, a `package.json` and TypeScript declarations.
    Library,
}

impl WasmKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WasmKind::Application => "application",
            WasmKind::Library => "library",
        }
    }
}

/// `[target.wasm] wasm-opt`: whether release builds optimize the module with Binaryen's `wasm-opt`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WasmOpt {
    /// Use `wasm-opt` from `PATH` when there is one (the default).
    #[default]
    Auto,
    /// Never run `wasm-opt`.
    Off,
    /// Run this executable; relative to the project directory.
    Path(PathBuf),
}

/// The tool that installs the npm packages Wasm libraries need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Bun,
}

impl PackageManager {
    /// Tried in this order when `package-manager` is not set.
    pub const ALL: [PackageManager; 3] = [PackageManager::Npm, PackageManager::Pnpm, PackageManager::Bun];

    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Bun => "bun",
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PackageManager {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "npm" => Ok(PackageManager::Npm),
            "pnpm" => Ok(PackageManager::Pnpm),
            "bun" => Ok(PackageManager::Bun),
            other => bail!("unknown wasm package-manager `{other}` (expected `npm`, `pnpm` or `bun`)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmTarget {
    pub environment: WasmEnvironment,
    pub kind: WasmKind,
    /// Name of the output files (`<module-name>.mjs`, `.wasm`); `None` uses the package name.
    pub module_name: Option<String>,
    /// The npm package name of a library; `None` uses the package name.
    pub npm_name: Option<String>,
    /// `None` picks the first of npm, pnpm and bun found on `PATH`.
    pub package_manager: Option<PackageManager>,
    /// npm packages the project needs besides those its Wasm libraries ask for, by name.
    pub npm_dependencies: BTreeMap<String, String>,
    pub wasm_opt: WasmOpt,
    pub dependencies: Vec<Dependency>,
}

impl WasmTarget {
    /// The name of the compiled module and its files: `module-name`, else the package name.
    pub fn module_name<'a>(&'a self, package: &'a Package) -> &'a str {
        self.module_name.as_deref().unwrap_or(&package.name)
    }

    /// The name of the npm package a library publishes: `npm-name`, else the lowercase package name.
    pub fn npm_package_name(&self, package: &Package) -> String {
        self.npm_name.clone().unwrap_or_else(|| package.name.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeKind {
    #[default]
    Executable,
    Library,
    Shared,
    Static,
}

impl NativeKind {
    /// The name written in `kind` of `[target.native]`.
    pub fn as_str(self) -> &'static str {
        match self {
            NativeKind::Executable => "executable",
            NativeKind::Library => "library",
            NativeKind::Shared => "shared",
            NativeKind::Static => "static",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    /// Konan target names such as `linux_x64`; `host` means the current machine.
    pub platforms: Vec<String>,
    pub kind: NativeKind,
    pub entry_point: Option<String>,
    pub linker_options: Vec<String>,
    /// C libraries bound with `cinterop`, from `[target.native.cinterop.<name>]`, by name.
    pub cinterop: Vec<Cinterop>,
    pub dependencies: Vec<Dependency>,
}

/// A C library bound to Kotlin with the Kotlin/Native `cinterop` tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cinterop {
    pub name: String,
    /// The `.def` file, relative to the project root, when set explicitly.
    pub def: Option<PathBuf>,
    /// Kotlin package of the bindings; `None` keeps the one in the `.def` file.
    pub package: Option<String>,
    /// Extra headers to bind, as named in `#include`.
    pub headers: Vec<String>,
    pub compiler_options: Vec<String>,
    pub linker_options: Vec<String>,
}

impl Cinterop {
    /// The `.def` file: `def` if set, otherwise `interop/<name>.def`.
    pub fn def_path(&self) -> PathBuf {
        self.def.clone().unwrap_or_else(|| Path::new("interop").join(format!("{}.def", self.name)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidTarget {
    pub config: AndroidConfig,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Targets {
    pub jvm: Option<JvmTarget>,
    pub wasm: Option<WasmTarget>,
    pub native: Option<NativeTarget>,
    pub android: Option<AndroidTarget>,
}

impl Targets {
    pub fn enabled(&self) -> Vec<TargetKind> {
        let mut out = Vec::new();
        if self.jvm.is_some() {
            out.push(TargetKind::Jvm);
        }
        if self.wasm.is_some() {
            out.push(TargetKind::Wasm);
        }
        if self.native.is_some() {
            out.push(TargetKind::Native);
        }
        if self.android.is_some() {
            out.push(TargetKind::Android);
        }
        out
    }

    pub fn is_enabled(&self, kind: TargetKind) -> bool {
        self.enabled().contains(&kind)
    }

    /// Target-specific dependencies.
    pub fn dependencies(&self, kind: TargetKind) -> &[Dependency] {
        match kind {
            TargetKind::Jvm => self.jvm.as_ref().map(|t| t.dependencies.as_slice()),
            TargetKind::Wasm => self.wasm.as_ref().map(|t| t.dependencies.as_slice()),
            TargetKind::Native => self.native.as_ref().map(|t| t.dependencies.as_slice()),
            TargetKind::Android => self.android.as_ref().map(|t| t.dependencies.as_slice()),
        }
        .unwrap_or(&[])
    }
}

/// Built-in framework integrations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frameworks {
    pub compose: Option<ComposeConfig>,
    pub serialization: Option<SerializationConfig>,
    pub coroutines: Option<CoroutinesConfig>,
    pub ktor: Option<KtorConfig>,
    pub lombok: Option<LombokConfig>,
    pub spring_boot: Option<SpringBootConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoroutinesConfig {
    pub version: Option<String>,
}

/// A Kotlin compiler plugin fetched from Maven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerPlugin {
    pub name: String,
    pub artifact: CoordSpec,
    pub id: Option<String>,
    pub options: BTreeMap<String, String>,
}

/// Lifecycle points where external plugins can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Hook {
    PreBuild,
    PostBuild,
    PreTest,
    PostTest,
    PreRun,
    PostPackage,
}

impl Hook {
    pub const ALL: [Hook; 6] = [Hook::PreBuild, Hook::PostBuild, Hook::PreTest, Hook::PostTest, Hook::PreRun, Hook::PostPackage];

    pub fn as_str(self) -> &'static str {
        match self {
            Hook::PreBuild => "pre-build",
            Hook::PostBuild => "post-build",
            Hook::PreTest => "pre-test",
            Hook::PostTest => "post-test",
            Hook::PreRun => "pre-run",
            Hook::PostPackage => "post-package",
        }
    }
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Hook {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Hook::ALL
            .into_iter()
            .find(|h| h.as_str() == s)
            .with_context(|| format!("unknown hook `{s}` (expected one of: {})", Hook::ALL.map(Hook::as_str).join(", ")))
    }
}

/// An external executable that runs at lifecycle hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPlugin {
    pub name: String,
    pub command: String,
    /// Replaces `command` on Windows when set.
    pub windows_command: Option<String>,
    pub args: Vec<String>,
    /// Replaces `args` on Windows when set.
    pub windows_args: Option<Vec<String>>,
    pub hooks: Vec<Hook>,
    pub env: BTreeMap<String, String>,
}

impl ProcessPlugin {
    /// The command for the current platform: `windows-command` on Windows if set, else `command`.
    pub fn platform_command(&self) -> &str {
        self.windows_command.as_deref().filter(|_| cfg!(windows)).unwrap_or(&self.command)
    }

    /// The arguments for the current platform: `windows-args` on Windows if set, else `args`.
    pub fn platform_args(&self) -> &[String] {
        self.windows_args.as_deref().filter(|_| cfg!(windows)).unwrap_or(&self.args)
    }
}

/// A user-defined task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub description: Option<String>,
    /// Shell commands, run in order.
    pub run: Vec<String>,
    /// Replaces `run` on Windows when set.
    pub windows_run: Option<Vec<String>>,
    /// The shell the commands run in; `None` is the platform shell (`sh` on Unix, `cmd` on Windows).
    pub shell: Option<TaskShell>,
    pub depends_on: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// Built-in hooks this task attaches to.
    pub hooks: Vec<Hook>,
}

/// A shell a task can choose with `shell = "..."`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskShell {
    Sh,
    Bash,
    Cmd,
    PowerShell,
    Pwsh,
}

impl TaskShell {
    pub const ALL: [TaskShell; 5] = [TaskShell::Sh, TaskShell::Bash, TaskShell::Cmd, TaskShell::PowerShell, TaskShell::Pwsh];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskShell::Sh => "sh",
            TaskShell::Bash => "bash",
            TaskShell::Cmd => "cmd",
            TaskShell::PowerShell => "powershell",
            TaskShell::Pwsh => "pwsh",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shell| shell.as_str() == s)
    }

    /// The shell used when a task doesn't set one.
    pub fn platform_default() -> Self {
        if cfg!(windows) { TaskShell::Cmd } else { TaskShell::Sh }
    }
}

impl Task {
    /// The commands for the current platform: `windows-run` on Windows if set, else `run`.
    pub fn platform_run(&self) -> &[String] {
        self.windows_run.as_deref().filter(|_| cfg!(windows)).unwrap_or(&self.run)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub debug: bool,
    pub optimize: bool,
    pub compiler_args: Vec<String>,
}

impl Profile {
    pub fn dev() -> Self {
        Self { debug: true, optimize: false, compiler_args: Vec::new() }
    }

    pub fn release() -> Self {
        Self { debug: false, optimize: true, compiler_args: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub group: Option<String>,
    pub description: Option<String>,
    /// `Name <email>` entries; they become the POM's developers.
    pub authors: Vec<String>,
    /// An SPDX license expression such as `MIT OR Apache-2.0`.
    pub license: Option<String>,
    /// The project's web site; the POM's `<url>`.
    pub homepage: Option<String>,
    /// The source repository, e.g. `https://github.com/me/lib`; the POM's `<scm>`.
    pub repository: Option<String>,
    /// Whether `kargo publish` may publish the package (`publish = false` prevents it).
    pub publish: bool,
}

/// The `kargo publish` destination that means Maven Central (through the Central Portal).
pub const MAVEN_CENTRAL_DESTINATION: &str = "maven-central";

/// Whether `name` is somewhere `kargo publish` can publish to: Maven Central, the
/// local Maven repository, or a `[repositories]` entry.
pub fn is_publish_destination(name: &str, repositories: &[Repository]) -> bool {
    name == MAVEN_CENTRAL_DESTINATION || name == kargo_maven::repo::MAVEN_LOCAL || repositories.iter().any(|r| r.name == name)
}

/// What `kargo publish` puts in the `-javadoc.jar` (`[publish] javadoc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Javadoc {
    /// HTML documentation generated by Dokka (the default).
    #[default]
    Dokka,
    /// A jar with only a manifest, which satisfies Maven Central.
    Empty,
    /// No javadoc jar.
    Off,
}

/// What the Central Portal does with a validated upload (`[publish] central-publishing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CentralPublishing {
    /// Wait for a release from the Portal's web UI (the default).
    #[default]
    Manual,
    /// Release right away.
    Automatic,
}

/// `[publish]`: options for `kargo publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSettings {
    /// Where `kargo publish` publishes without `--repository`.
    pub repository: Option<String>,
    /// Sign every file with GnuPG; `None` signs only for Maven Central.
    pub sign: Option<bool>,
    /// Publish `-sources.jar` files.
    pub sources: bool,
    pub javadoc: Javadoc,
    pub central_publishing: CentralPublishing,
}

impl Default for PublishSettings {
    fn default() -> Self {
        Self { repository: None, sign: None, sources: true, javadoc: Javadoc::Dokka, central_publishing: CentralPublishing::Manual }
    }
}

/// Which JDK builds and runs the project (`[toolchain] jdk = ...`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JdkRequest {
    /// Use `JAVA_HOME` or `java` on `PATH` (the default).
    #[default]
    System,
    /// The newest Temurin release of a feature version, e.g. `21`.
    Major(u32),
    /// An exact Temurin release such as `jdk-21.0.5+11` (also accepted as `21.0.5+11`).
    Release(String),
}

impl JdkRequest {
    /// The Java feature version this request implies, if known.
    pub fn major(&self) -> Option<u32> {
        match self {
            JdkRequest::System => None,
            JdkRequest::Major(m) => Some(*m),
            JdkRequest::Release(r) => release_major(r),
        }
    }
}

/// Extracts the feature version from a release name: `jdk-21.0.5+11` -> 21, `jdk8u432-b06` -> 8.
pub fn release_major(release: &str) -> Option<u32> {
    let rest = release.strip_prefix("jdk-").or_else(|| release.strip_prefix("jdk"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl FromStr for JdkRequest {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s == "system" {
            return Ok(JdkRequest::System);
        }
        if let Ok(major) = s.parse::<u32>() {
            if major < 8 {
                bail!("JDK {major} is not supported; use 8 or newer");
            }
            return Ok(JdkRequest::Major(major));
        }
        let valid = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_');
        if s.is_empty() || !s.chars().all(valid) {
            bail!("invalid JDK `{s}` (expected `system`, a major version like `21`, or a release like `21.0.5+11`)");
        }
        let release = if s.starts_with("jdk") { s.to_owned() } else { format!("jdk-{s}") };
        match release_major(&release) {
            Some(m) if m >= 8 => Ok(JdkRequest::Release(release)),
            _ => bail!("invalid JDK release `{s}` (expected something like `21.0.5+11` or `jdk8u432-b06`)"),
        }
    }
}

impl fmt::Display for JdkRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JdkRequest::System => f.write_str("system"),
            JdkRequest::Major(m) => write!(f, "{m}"),
            JdkRequest::Release(r) => f.write_str(r),
        }
    }
}

/// `[toolchain]`: toolchains Kargo should provision for this project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolchainSettings {
    pub jdk: JdkRequest,
}

/// `[doc]`: options for `kargo doc` (Dokka).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocSettings {
    /// Dokka version; `None` uses Kargo's pinned default.
    pub dokka_version: Option<String>,
    /// Markdown files with module and package documentation, relative to the project root.
    pub includes: Vec<PathBuf>,
    /// Base URL of the source tree (e.g. `https://github.com/me/repo/blob/main`), without a trailing slash.
    pub source_link: Option<String>,
    /// Documentation sites of dependencies to link to.
    pub external_links: Vec<String>,
}

/// `[fmt]`: options for `kargo fmt` (ktlint).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FmtSettings {
    /// ktlint version; `None` uses Kargo's pinned default.
    pub ktlint_version: Option<String>,
    /// Globs, relative to the project root, of files ktlint skips.
    pub exclude: Vec<String>,
}

/// `[lint]`: options for `kargo lint` (detekt).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LintSettings {
    /// detekt version; `None` uses Kargo's pinned default.
    pub detekt_version: Option<String>,
    /// detekt configuration files, relative to the project root; empty means `detekt.yml` if it exists.
    pub config: Vec<PathBuf>,
    /// detekt baseline file, relative to the project root; `None` means `detekt-baseline.xml`.
    pub baseline: Option<PathBuf>,
}

/// `[test]`: options for `kargo test`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSettings {
    /// Stop at the first target whose tests fail; `kargo test --no-fail-fast` overrides it.
    pub fail_fast: bool,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self { fail_fast: true }
    }
}

/// A named program from `[[bin]]`, run with `kargo run --bin <name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bin {
    pub name: String,
    /// JVM main class (`main`), e.g. `com.example.tools.MigrateKt`.
    pub main: Option<String>,
    /// Kotlin/Native entry function (`entry-point`), e.g. `com.example.tools.main`.
    pub entry_point: Option<String>,
    /// Source file, relative to the project root, whose `main` function is the entry point (`path`).
    pub path: Option<PathBuf>,
}

/// A fully validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub package: Package,
    pub kotlin: KotlinSettings,
    pub targets: Targets,
    /// Extra programs from `[[bin]]`, in declaration order.
    pub bins: Vec<Bin>,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    /// `path` dependencies from every dependency table.
    pub path_dependencies: Vec<PathDependency>,
    pub repositories: Vec<Repository>,
    pub frameworks: Frameworks,
    pub compiler_plugins: Vec<CompilerPlugin>,
    pub process_plugins: Vec<ProcessPlugin>,
    pub tasks: BTreeMap<String, Task>,
    pub dev_profile: Profile,
    pub release_profile: Profile,
    pub toolchain: ToolchainSettings,
    pub doc: DocSettings,
    pub fmt: FmtSettings,
    pub lint: LintSettings,
    pub test: TestSettings,
    pub publish: PublishSettings,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_in(path, None)
    }

    /// Loads a package manifest; `workspace` supplies what `{ workspace = true }` entries inherit.
    pub fn load_in(path: &Path, workspace: Option<&WorkspaceManifest>) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse_in(&text, workspace).with_context(|| format!("failed to parse manifest at {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_in(text, None)
    }

    pub fn parse_in(text: &str, workspace: Option<&WorkspaceManifest>) -> Result<Self> {
        if is_workspace(text)? {
            bail!("this is a workspace manifest ([workspace]), not a package; select a member with `-p <name>`");
        }
        let raw: raw::RawManifest = match raw::inherit(toml::from_str(text)?, workspace)? {
            Some(table) => toml::Value::Table(table).try_into()?,
            None => toml::from_str(text)?,
        };
        raw.validate(workspace)
    }

    /// The `[[bin]]` named `name`.
    pub fn bin(&self, name: &str) -> Option<&Bin> {
        self.bins.iter().find(|b| b.name == name)
    }

    /// Dependencies for `target`, including the common ones.
    pub fn dependencies_for(&self, target: TargetKind) -> Vec<Dependency> {
        self.dependencies.iter().chain(self.targets.dependencies(target)).cloned().collect()
    }
}

/// A workspace's `kargo.toml`: `[workspace]` with its `members`, and what members share or
/// inherit: `[workspace.package]`, `[workspace.kotlin]`, `[workspace.dependencies]`,
/// `[workspace.frameworks]`, `[workspace.plugins]` and `[workspace.tasks]`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceManifest {
    /// The directory holding the workspace's `kargo.toml`.
    pub root: PathBuf,
    /// Member directories or globs (`libs/*`) as written, relative to `root`.
    pub members: Vec<String>,
    /// Directories that `members` globs must not match, relative to `root`.
    pub exclude: Vec<PathBuf>,
    /// The members `kargo build` and the other commands work on at the workspace root
    /// (`default-members`); empty means every member.
    pub default_members: Vec<PathBuf>,
    /// The Kotlin version every member builds with.
    pub kotlin_version: Option<String>,
    /// `[workspace.kotlin]`: compiler settings every member starts from.
    pub kotlin: WorkspaceKotlin,
    /// Tasks run at the workspace root (`[workspace.tasks]`); members may inherit them.
    pub tasks: BTreeMap<String, Task>,
    /// Raw `[workspace.dependencies]` entries, inherited with `{ workspace = true }`.
    dependencies: toml::Table,
    /// Raw `[workspace.package]`, `[workspace.frameworks]`, `[workspace.plugins]` and
    /// `[workspace.tasks]` entries, inherited with `{ workspace = true }`.
    package: toml::Table,
    frameworks: toml::Table,
    plugins: toml::Table,
    raw_tasks: toml::Table,
}

/// `[workspace.kotlin]`: Kotlin settings that apply to every member. A member's own `opt-in` and
/// `compiler-args` are added to these, and its other `[kotlin]` keys take precedence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceKotlin {
    pub language_version: Option<String>,
    pub api_version: Option<String>,
    pub ksp_version: Option<String>,
    pub opt_in: Vec<String>,
    pub compiler_args: Vec<String>,
    pub warnings_as_errors: bool,
    /// The default `[target.jvm] jvm-target` of members.
    pub jvm_target: Option<String>,
}

impl WorkspaceManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        let root = path.parent().unwrap_or(Path::new("."));
        Self::parse(&text, root).with_context(|| format!("failed to parse manifest at {}", path.display()))
    }

    /// Parses a workspace manifest whose directory is `root`.
    pub fn parse(text: &str, root: &Path) -> Result<Self> {
        let raw: raw::RawWorkspaceManifest = toml::from_str(text)?;
        raw.validate(root)
    }

    /// Whether `[workspace.dependencies]` declares `name`.
    pub fn has_dependency(&self, name: &str) -> bool {
        self.dependencies.contains_key(name)
    }

    /// The member directories, relative to `root`: each `members` entry, with globs expanded to
    /// the matching directories that hold a `kargo.toml` (sorted), minus `exclude`.
    pub fn member_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = Vec::new();
        for pattern in &self.members {
            let found = if is_glob(pattern) {
                let mut found = expand_glob(&self.root, pattern)?;
                found.retain(|dir| self.root.join(dir).join(MANIFEST_NAME).is_file());
                found.sort();
                found
            } else {
                vec![PathBuf::from(pattern)]
            };
            for dir in found {
                let excluded = self.exclude.iter().any(|e| normalize(e) == normalize(&dir));
                if !excluded && !out.iter().any(|d| normalize(d) == normalize(&dir)) {
                    out.push(dir);
                }
            }
        }
        if out.is_empty() && !self.members.is_empty() {
            bail!("[workspace] `members` matches no package; check the globs and `exclude`");
        }
        Ok(out)
    }

    /// Whether `dir` (relative to `root`) is a member: listed in `members` or matched by one of
    /// its globs, and not excluded. Unlike [`member_dirs`](Self::member_dirs), it doesn't read the disk.
    pub fn lists_member(&self, dir: &Path) -> bool {
        let dir = normalize(dir);
        if self.exclude.iter().any(|e| normalize(e) == dir) {
            return false;
        }
        self.members.iter().any(|pattern| {
            if is_glob(pattern) { glob_matches(&normalize(Path::new(pattern)), &dir) } else { normalize(Path::new(pattern)) == dir }
        })
    }
}

/// Whether a `members` entry is a glob rather than a directory.
fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

/// A relative path with `/` separators and without `.` components, for comparisons.
fn normalize(path: &Path) -> String {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Matches one path segment against a glob segment with `*` (any run of characters) and `?`.
fn segment_matches(pattern: &[char], name: &[char]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => segment_matches(&pattern[1..], name) || (!name.is_empty() && segment_matches(pattern, &name[1..])),
        (Some('?'), Some(_)) => segment_matches(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => segment_matches(&pattern[1..], &name[1..]),
        _ => false,
    }
}

/// Whether the `/`-separated `path` matches the `/`-separated glob `pattern`, segment by segment.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let (pattern, path): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    pattern.len() == path.len()
        && pattern.iter().zip(&path).all(|(p, n)| segment_matches(&p.chars().collect::<Vec<_>>(), &n.chars().collect::<Vec<_>>()))
}

/// The directories under `root` matching the glob `pattern`, relative to `root`.
fn expand_glob(root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let mut found = vec![PathBuf::new()];
    for segment in normalize(Path::new(pattern)).split('/') {
        let mut next = Vec::new();
        for dir in found {
            if !is_glob(segment) {
                if root.join(&dir).join(segment).is_dir() {
                    next.push(dir.join(segment));
                }
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root.join(&dir)) else { continue };
            let pattern: Vec<char> = segment.chars().collect();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // Like a shell, `*` does not match hidden directories.
                if entry.path().is_dir() && !name.starts_with('.') && segment_matches(&pattern, &name.chars().collect::<Vec<_>>()) {
                    next.push(dir.join(name));
                }
            }
        }
        found = next;
    }
    Ok(found)
}

/// Whether `text` is a workspace manifest (a `[workspace]` table) rather than a package.
pub fn is_workspace(text: &str) -> Result<bool> {
    let table: toml::Table = toml::from_str(text)?;
    if !table.contains_key("workspace") {
        return Ok(false);
    }
    if table.contains_key("package") {
        bail!("a kargo.toml with [workspace] cannot also have [package] yet; move the package into a member directory");
    }
    Ok(true)
}

/// Whether the manifest at `path` is a workspace manifest.
pub fn is_workspace_manifest(path: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    is_workspace(&text).with_context(|| format!("failed to parse manifest at {}", path.display()))
}

/// Validates a package name (used for artifact and binary names).
pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("package name must not be empty");
    }
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        bail!("package name `{name}` must start with a letter");
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_')) {
        bail!("package name `{name}` contains invalid character `{bad}` (use letters, digits, `-` and `_`)");
    }
    Ok(())
}

/// A Wasm module name becomes file names and an import path, so it stays portable.
fn validate_module_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('.') || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("invalid [target.wasm] module-name `{name}` (use letters, digits, `-`, `_` and `.`, not starting with `.`)");
    }
    Ok(())
}

/// npm package names: lowercase, URL-safe, optionally scoped (`@scope/name`).
fn validate_npm_name(name: &str) -> Result<()> {
    let part_ok = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '~'))
    };
    let valid = match name.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').is_some_and(|(scope, rest)| part_ok(scope) && part_ok(rest)),
        None => part_ok(name),
    };
    if !valid || name.len() > 214 {
        bail!(
            "invalid [target.wasm] npm-name `{name}` (use lowercase letters, digits, `-`, `.`, `_` and `~`, optionally scoped like `@scope/name`)"
        );
    }
    Ok(())
}

fn validate_version_string(what: &str, version: &str) -> Result<()> {
    if version.trim().is_empty() {
        bail!("{what} must not be empty");
    }
    version.parse::<VersionReq>().with_context(|| format!("invalid {what} `{version}`"))?;
    Ok(())
}

/// Reads repository credentials from the named environment variables. Unset
/// variables yield no credentials (the repository then answers 401 with a
/// helpful message) so that unrelated commands keep working.
fn credentials_from_env(name: &str, user_env: Option<&str>, pass_env: Option<&str>) -> Result<Option<Credentials>> {
    match (user_env, pass_env) {
        (None, None) => Ok(None),
        (Some(u), Some(p)) => match (std::env::var(u), std::env::var(p)) {
            (Ok(username), Ok(password)) => Ok(Some(Credentials { username, password })),
            _ => Ok(None),
        },
        _ => bail!("repository `{name}`: set both `username-env` and `password-env`"),
    }
}

#[cfg(test)]
mod tests;
