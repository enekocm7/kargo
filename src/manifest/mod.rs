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

use crate::maven::{CoordSpec, ModuleId, Repository, VersionReq};
use crate::maven::repo::Credentials;

pub use raw::{AndroidConfig, ComposeConfig, KtorConfig, LombokConfig, SerializationConfig, SpringBootConfig};

pub const MANIFEST_NAME: &str = "kargo.toml";
pub const LOCKFILE_NAME: &str = "kargo.lock";

/// Default Kotlin version for new projects.
pub const DEFAULT_KOTLIN_VERSION: &str = "2.4.21";
/// Default JVM bytecode target.
pub const DEFAULT_JVM_TARGET: &str = "17";

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
}

impl FromStr for Scope {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "compile" | "implementation" | "api" => Ok(Scope::Compile),
            "runtime" | "runtime-only" => Ok(Scope::Runtime),
            "provided" | "compile-only" => Ok(Scope::Provided),
            other => bail!("unknown dependency scope `{other}` (expected compile, runtime or provided)"),
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
        }
    }
}

/// Kotlin compiler settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KotlinSettings {
    pub version: String,
    pub language_version: Option<String>,
    pub api_version: Option<String>,
    pub opt_in: Vec<String>,
    pub compiler_args: Vec<String>,
    pub warnings_as_errors: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmTarget {
    pub jvm_target: String,
    pub main_class: Option<String>,
    pub jvm_args: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WasmEnvironment {
    #[default]
    Js,
    Wasi,
}

impl WasmEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            WasmEnvironment::Js => "js",
            WasmEnvironment::Wasi => "wasi",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmTarget {
    pub environment: WasmEnvironment,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeKind {
    #[default]
    Executable,
    Library,
    Shared,
    Static,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    /// Konan target names such as `linux_x64`; `host` means the current machine.
    pub platforms: Vec<String>,
    pub kind: NativeKind,
    pub entry_point: Option<String>,
    pub linker_options: Vec<String>,
    pub dependencies: Vec<Dependency>,
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
    pub args: Vec<String>,
    pub hooks: Vec<Hook>,
    pub env: BTreeMap<String, String>,
}

/// A user-defined task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub description: Option<String>,
    /// Shell commands, run in order.
    pub run: Vec<String>,
    pub depends_on: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// Built-in hooks this task attaches to.
    pub hooks: Vec<Hook>,
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
    pub authors: Vec<String>,
    pub license: Option<String>,
}

/// A fully validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub package: Package,
    pub kotlin: KotlinSettings,
    pub targets: Targets,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    pub repositories: Vec<Repository>,
    pub frameworks: Frameworks,
    pub compiler_plugins: Vec<CompilerPlugin>,
    pub process_plugins: Vec<ProcessPlugin>,
    pub tasks: BTreeMap<String, Task>,
    pub dev_profile: Profile,
    pub release_profile: Profile,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("failed to parse manifest at {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let raw: raw::RawManifest = toml::from_str(text)?;
        raw.validate()
    }

    /// Dependencies for `target`, including the common ones.
    pub fn dependencies_for(&self, target: TargetKind) -> Vec<Dependency> {
        self.dependencies.iter().chain(self.targets.dependencies(target)).cloned().collect()
    }
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
