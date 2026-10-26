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
pub mod script;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use kargo_maven::repo::Credentials;
use kargo_maven::{CoordSpec, ModuleId, Repository, VersionReq};

pub use raw::{
    AndroidConfig, AndroidFlavor, AndroidSigning, BenchmarkConfig, BuildConfigValue, COMPOSE_FEATURE_FLAGS, ComposeConfig, KtorConfig,
    LombokConfig, SerializationConfig, SpringBootConfig,
};

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
    /// Kotlin/JS (the IR backend), for Node.js and browsers.
    Js,
    Native,
    Android,
}

impl TargetKind {
    pub const ALL: [TargetKind; 5] = [TargetKind::Jvm, TargetKind::Wasm, TargetKind::Js, TargetKind::Native, TargetKind::Android];

    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Jvm => "jvm",
            TargetKind::Wasm => "wasm",
            TargetKind::Js => "js",
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
            "js" | "javascript" => Ok(TargetKind::Js),
            "native" => Ok(TargetKind::Native),
            "android" => Ok(TargetKind::Android),
            other => bail!("unknown target `{other}` (expected one of: jvm, wasm, js, native, android)"),
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
    /// The oldest JDK that can compile for the configured `jvm-target`; `None` when it is the
    /// default, which [`JvmTarget::target_for`] lowers to fit any JDK.
    pub fn required_jdk(&self) -> Option<u32> {
        self.jvm_target_set.then(|| jvm_target_major(&self.jvm_target))
    }

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

/// Binaryen optimization preset used for release modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WasmOptLevel {
    Size,
    #[default]
    Balanced,
    Speed,
}

/// Precompressed representations emitted alongside web assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmCompression {
    Gzip,
    Brotli,
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
            other => bail!("unknown package-manager `{other}` (expected `npm`, `pnpm` or `bun`)"),
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
    pub opt_level: WasmOptLevel,
    pub wasm_opt_flags: Vec<String>,
    pub debug_info: bool,
    pub compress: Vec<WasmCompression>,
    pub content_hash: bool,
    pub max_size: Option<u64>,
    pub max_gzip_size: Option<u64>,
    pub size_report: bool,
    pub wasm_enable_array_range_checks: Option<bool>,
    pub wasm_debugger_custom_formatters: bool,
    /// Files and directories of a library, relative to the package, that every browser app depending
    /// on it gets next to its module, under their own names.
    pub web_resources: Vec<PathBuf>,
    /// A script of a library, relative to the package, that the page Kargo generates for a dependent
    /// browser app imports before the app module. It is shipped like `web_resources`.
    pub web_init: Option<PathBuf>,
    /// Directories of the package's own JavaScript and TypeScript modules, relative to the package
    /// ([`DEFAULT_JS_SOURCES`] unless `js-sources` says otherwise; none for WASI).
    pub js_sources: Vec<PathBuf>,
    pub dependencies: Vec<Dependency>,
}

/// The directory of a package's own JavaScript and TypeScript modules when `js-sources` is not set.
pub const DEFAULT_JS_SOURCES: &str = "js";

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

/// Where a Kotlin/JS program runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsEnvironment {
    /// Node.js (the default).
    #[default]
    Node,
    /// A web browser: Kargo adds an HTML page, serves it for `kargo run` and runs tests in a headless browser.
    Browser,
}

impl JsEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            JsEnvironment::Node => "node",
            JsEnvironment::Browser => "browser",
        }
    }
}

/// What a Kotlin/JS build produces; the same choices as Wasm.
pub type JsKind = WasmKind;

/// The kind of JavaScript module the Kotlin/JS compiler writes (`-module-kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsModuleKind {
    /// An ES module, `<name>.mjs` (the default).
    #[default]
    Es,
    /// A CommonJS module for `require`, `<name>.js`.
    CommonJs,
    /// A UMD module, `<name>.js`, which works with `require`, AMD and a plain `<script>` tag.
    Umd,
}

impl JsModuleKind {
    /// The name written in `module-kind` and passed to `-module-kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            JsModuleKind::Es => "es",
            JsModuleKind::CommonJs => "commonjs",
            JsModuleKind::Umd => "umd",
        }
    }

    /// The extension of the files the compiler writes for this kind of module.
    pub fn extension(self) -> &'static str {
        match self {
            JsModuleKind::Es => "mjs",
            JsModuleKind::CommonJs | JsModuleKind::Umd => "js",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsTarget {
    pub environment: JsEnvironment,
    pub kind: JsKind,
    pub module_kind: JsModuleKind,
    /// Name of the output files (`<module-name>.mjs`); `None` uses the package name.
    pub module_name: Option<String>,
    /// The npm package name of a library; `None` uses the package name.
    pub npm_name: Option<String>,
    /// `None` picks the first of npm, pnpm and bun found on `PATH`.
    pub package_manager: Option<PackageManager>,
    /// npm packages the project needs besides those its Kotlin/JS libraries ask for, by name.
    pub npm_dependencies: BTreeMap<String, String>,
    /// Directories of the package's own JavaScript and TypeScript modules, as for Wasm.
    pub js_sources: Vec<PathBuf>,
    pub dependencies: Vec<Dependency>,
}

impl JsTarget {
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
    /// An Apple framework (`-produce framework`) for Swift and Objective-C.
    Framework,
}

impl NativeKind {
    /// The name written in `kind` of `[target.native]`.
    pub fn as_str(self) -> &'static str {
        match self {
            NativeKind::Executable => "executable",
            NativeKind::Library => "library",
            NativeKind::Shared => "shared",
            NativeKind::Static => "static",
            NativeKind::Framework => "framework",
        }
    }
}

/// Whether `konan_target` builds for an Apple platform (macOS, iOS, tvOS, watchOS).
pub fn is_apple_target(konan_target: &str) -> bool {
    ["macos_", "ios_", "tvos_", "watchos_"].iter().any(|p| konan_target.starts_with(p))
}

/// C compiler and linker options, from `[target.native]`, `[target.native.cinterop.<name>]` and
/// their `platform.<konan-target>` tables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeOptions {
    /// Options for clang (`-I`, `-D`, ...); only C bindings have them.
    pub compiler_options: Vec<String>,
    pub linker_options: Vec<String>,
    /// Static libraries (`libfoo.a`) found in `library-paths` and embedded in the output.
    pub static_libraries: Vec<String>,
    /// Directories searched for `static-libraries` and passed to the linker as `-L`, relative to the project root.
    pub library_paths: Vec<PathBuf>,
    /// Apple frameworks to link (`-framework <name>`), on Apple platforms only.
    pub frameworks: Vec<String>,
}

impl NativeOptions {
    /// These options followed by `more`.
    pub fn extend(&mut self, more: &NativeOptions) {
        self.compiler_options.extend(more.compiler_options.iter().cloned());
        self.linker_options.extend(more.linker_options.iter().cloned());
        self.static_libraries.extend(more.static_libraries.iter().cloned());
        self.library_paths.extend(more.library_paths.iter().cloned());
        self.frameworks.extend(more.frameworks.iter().cloned());
    }

    pub fn is_empty(&self) -> bool {
        *self == NativeOptions::default()
    }
}

/// The options of a table for `konan_target`: the common ones, then those of `platform.<konan_target>`.
fn options_for(common: &NativeOptions, platform: &BTreeMap<String, NativeOptions>, konan_target: &str) -> NativeOptions {
    let mut options = common.clone();
    if let Some(more) = platform.get(konan_target) {
        options.extend(more);
    }
    options
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    /// Konan target names such as `linux_x64`; `host` means the current machine.
    pub platforms: Vec<String>,
    pub kind: NativeKind,
    pub entry_point: Option<String>,
    /// Linker options for every platform (`compiler_options` is always empty here).
    pub options: NativeOptions,
    /// More options per Konan target, from `[target.native.platform.<konan-target>]`.
    pub platform: BTreeMap<String, NativeOptions>,
    /// C libraries bound with `cinterop`, from `[target.native.cinterop.<name>]`, by name.
    pub cinterop: Vec<Cinterop>,
    /// C and C++ sources compiled into a static library and linked in, from `[target.native.c-sources]`.
    pub c_sources: Option<CSources>,
    /// `bundle-id` of a framework's `Info.plist`.
    pub bundle_id: Option<String>,
    /// Dependencies (`group:artifact`) whose API a framework, shared or static library exports too.
    pub export: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

impl NativeTarget {
    /// The linker options for `konan_target`, with those of its platform table.
    pub fn options_for(&self, konan_target: &str) -> NativeOptions {
        options_for(&self.options, &self.platform, konan_target)
    }
}

/// How a C library found with `pkg-config` (or listed with `-l`) is linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// The program loads the system's shared library (`-lfoo`).
    #[default]
    Dynamic,
    /// The static library (`libfoo.a`) is embedded, so the program runs without the library installed.
    Static,
}

/// The language of the headers a `cinterop` entry binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLanguage {
    C,
    ObjectiveC,
}

impl CLanguage {
    /// The value of `language` in a `.def` file.
    pub fn def_value(self) -> &'static str {
        match self {
            CLanguage::C => "C",
            CLanguage::ObjectiveC => "Objective-C",
        }
    }
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
    pub options: NativeOptions,
    /// More options per Konan target, from `platform.<konan-target>`.
    pub platform: BTreeMap<String, NativeOptions>,
    /// `.def` properties written in kargo.toml: `headerFilter`, `excludedFunctions`, `strictEnums`,
    /// `nonStrictEnums`, `modules` and `language`.
    pub header_filter: Vec<String>,
    pub excluded_functions: Vec<String>,
    pub strict_enums: Vec<String>,
    pub non_strict_enums: Vec<String>,
    /// Clang modules (Objective-C frameworks) to bind.
    pub modules: Vec<String>,
    pub language: Option<CLanguage>,
    /// `pkg-config` packages whose compiler and linker flags the bindings use.
    pub pkg_config: Vec<String>,
    pub link: LinkMode,
}

impl Cinterop {
    /// The `.def` file: `def` if set, otherwise `interop/<name>.def`.
    pub fn def_path(&self) -> PathBuf {
        self.def.clone().unwrap_or_else(|| Path::new("interop").join(format!("{}.def", self.name)))
    }

    /// The options for `konan_target`, with those of its platform table.
    pub fn options_for(&self, konan_target: &str) -> NativeOptions {
        options_for(&self.options, &self.platform, konan_target)
    }

    /// The `.def` properties set in kargo.toml, in the order they are written.
    pub fn def_properties(&self) -> Vec<(&'static str, String)> {
        let mut props = Vec::new();
        let mut list = |key, values: &[String]| {
            if !values.is_empty() {
                props.push((key, values.join(" ")));
            }
        };
        list("headerFilter", &self.header_filter);
        list("excludedFunctions", &self.excluded_functions);
        list("strictEnums", &self.strict_enums);
        list("nonStrictEnums", &self.non_strict_enums);
        list("modules", &self.modules);
        if let Some(language) = self.language {
            props.push(("language", language.def_value().to_owned()));
        }
        props
    }

    /// Whether kargo.toml describes the library without a `.def` file: headers or Clang modules to bind.
    pub fn describes_library(&self) -> bool {
        !self.headers.is_empty() || !self.modules.is_empty()
    }
}

/// `[target.native.c-sources]`: C, C++ and Objective-C files compiled with the clang of the
/// Kotlin/Native toolchain into a static library that every native binary links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CSources {
    /// Files, directories (searched recursively) and globs, relative to the project root.
    pub sources: Vec<String>,
    /// `-I` directories, relative to the project root; the C bindings get them too.
    pub include_dirs: Vec<PathBuf>,
    /// `-D` macros: `NAME` or `NAME=value`.
    pub defines: Vec<String>,
    pub compiler_options: Vec<String>,
    /// More defines and options per Konan target.
    pub platform: BTreeMap<String, CFlags>,
}

/// Defines and compiler options of a `[target.native.c-sources.platform.<konan-target>]` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CFlags {
    pub defines: Vec<String>,
    pub compiler_options: Vec<String>,
}

impl CSources {
    /// The directory searched when `sources` is not set, which is also the default include directory.
    pub const DEFAULT_DIR: &'static str = "c";

    /// The defines and compiler options for `konan_target`, with those of its platform table.
    pub fn flags_for(&self, konan_target: &str) -> CFlags {
        let mut flags = CFlags { defines: self.defines.clone(), compiler_options: self.compiler_options.clone() };
        if let Some(more) = self.platform.get(konan_target) {
            flags.defines.extend(more.defines.iter().cloned());
            flags.compiler_options.extend(more.compiler_options.iter().cloned());
        }
        flags
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
    pub js: Option<JsTarget>,
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
        if self.js.is_some() {
            out.push(TargetKind::Js);
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
            TargetKind::Js => self.js.as_ref().map(|t| t.dependencies.as_slice()),
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
    pub benchmark: Option<BenchmarkConfig>,
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

/// A build profile: `dev`, `release` or a custom `[profile.<name>]` with its `inherits` resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub debug: bool,
    pub optimize: bool,
    pub compiler_args: Vec<String>,
    /// `jvm-args`: JVM options for `kargo run` and `kargo test`, after `[target.jvm] jvm-args`.
    pub jvm_args: Vec<String>,
    /// `[profile.<name>.env]`: replaces `[env]` variable by variable.
    pub env: EnvTable,
    /// `[profile.<name>.dependencies]`: dependencies only this profile's builds have. They are added
    /// to the dependencies of every target and replace the ones with the same artifact.
    pub dependencies: Vec<Dependency>,
}

impl Profile {
    pub fn dev() -> Self {
        Self::builtin(true, false)
    }

    pub fn release() -> Self {
        Self::builtin(false, true)
    }

    fn builtin(debug: bool, optimize: bool) -> Self {
        Self { debug, optimize, compiler_args: Vec::new(), jvm_args: Vec::new(), env: EnvTable::default(), dependencies: Vec::new() }
    }
}

/// An `[env]` or `[profile.<name>.env]` table: the variables, the `.env` files to read and,
/// in `[env]` only, the variables to expose to the code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvTable {
    pub vars: BTreeMap<String, EnvVar>,
    /// `env-files`: more `.env` files, read after the standard ones and relative to the package root.
    pub files: Vec<String>,
    /// `expose`: variables compiled into a generated `BuildConfig` object.
    pub expose: Vec<String>,
}

/// A variable of `[env]` or `[profile.<name>.env]`: the program's environment for `kargo run`,
/// `kargo test`, tasks and hooks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvVar {
    /// The value; `None` for a `required` variable that the shell or a `.env` file must set.
    pub value: Option<String>,
    /// Use `value` even when the shell or a `.env` file sets the variable.
    pub force: bool,
    /// Resolve `value` against the package root.
    pub relative: bool,
    /// Fail before anything runs unless the shell or a `.env` file sets the variable.
    pub required: bool,
}

/// Whether `name` is a valid environment variable name: letters, digits and `_`, not starting with a digit.
pub fn is_env_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    /// The GraalVM that `kargo package --format native-image` and `kargo run --native-agent` use.
    pub graalvm: GraalvmRequest,
    /// The Kargo versions the project builds with (`kargo = ">=0.3"`). Already checked against the running Kargo when the
    /// manifest was read.
    pub kargo: Option<semver::VersionReq>,
}

/// The version of the running Kargo.
pub const KARGO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Fails when `current` does not satisfy the `[toolchain] kargo` requirement `req`.
pub fn check_kargo_version(req: &semver::VersionReq, current: &str) -> anyhow::Result<()> {
    let version =
        semver::Version::parse(current).map_err(|e| anyhow::anyhow!("cannot read the version of this Kargo (`{current}`): {e}"))?;
    // semver never matches a pre-release (0.3.0-rc.1) against a plain requirement (>=0.2), but a release candidate of
    // 0.3.0 is as new as 0.3.0 for this purpose.
    let release = semver::Version { pre: semver::Prerelease::EMPTY, build: semver::BuildMetadata::EMPTY, ..version.clone() };
    if req.matches(&version) || req.matches(&release) {
        return Ok(());
    }
    anyhow::bail!(
        "this project needs kargo {req} (`[toolchain] kargo` in kargo.toml), but this is kargo {current}: run `kargo self update` to get a newer Kargo"
    )
}

/// Which GraalVM distribution to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraalvmDistribution {
    /// GraalVM Community Edition (GPLv2 with the Classpath Exception).
    Community,
    /// Oracle GraalVM (GraalVM Free Terms and Conditions).
    Oracle,
}

impl GraalvmDistribution {
    pub fn as_str(self) -> &'static str {
        match self {
            GraalvmDistribution::Community => "community",
            GraalvmDistribution::Oracle => "oracle",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            GraalvmDistribution::Community => "GraalVM CE",
            GraalvmDistribution::Oracle => "Oracle GraalVM",
        }
    }
}

/// Which GraalVM builds native images (`[toolchain] graalvm = ...`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GraalvmRequest {
    /// `GRAALVM_HOME`, then a `JAVA_HOME` or `PATH` that has `native-image` (the default).
    #[default]
    System,
    /// The newest release of a Java feature version, e.g. `25` or `oracle-25`.
    Major { distribution: GraalvmDistribution, major: u32 },
}

impl GraalvmRequest {
    pub fn major(&self) -> Option<u32> {
        match self {
            GraalvmRequest::System => None,
            GraalvmRequest::Major { major, .. } => Some(*major),
        }
    }
}

impl FromStr for GraalvmRequest {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s == "system" {
            return Ok(GraalvmRequest::System);
        }
        let (distribution, version) = match s.split_once('-') {
            Some(("community" | "ce", v)) => (GraalvmDistribution::Community, v),
            Some(("oracle", v)) => (GraalvmDistribution::Oracle, v),
            Some(_) => bail!("invalid GraalVM `{s}` (expected `system`, a Java version like `25`, or `oracle-25`)"),
            None => (GraalvmDistribution::Community, s),
        };
        match version.parse::<u32>() {
            Ok(major) if major >= 17 => Ok(GraalvmRequest::Major { distribution, major }),
            Ok(major) => bail!("GraalVM for JDK {major} is not supported; use 17 or newer"),
            Err(_) => bail!("invalid GraalVM `{s}` (expected `system`, a Java version like `25`, or `oracle-25`)"),
        }
    }
}

impl fmt::Display for GraalvmRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraalvmRequest::System => f.write_str("system"),
            GraalvmRequest::Major { distribution: GraalvmDistribution::Community, major } => write!(f, "{major}"),
            GraalvmRequest::Major { distribution, major } => write!(f, "{}-{major}", distribution.as_str()),
        }
    }
}

/// `[package.jlink]`, `[package.optimize]`, `[package.native-image]` and `[package.installer]`: how `kargo package
/// --format` builds runtime images, native executables and installers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackagingSettings {
    pub jlink: JlinkSettings,
    pub optimize: OptimizeSettings,
    pub native_image: NativeImageSettings,
    pub installer: InstallerSettings,
}

/// `[package.optimize]`: R8's JVM class-file shrinking and obfuscation settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizeSettings {
    pub shrink: bool,
    pub optimize: bool,
    pub obfuscate: bool,
    pub rules: PathBuf,
}

impl Default for OptimizeSettings {
    fn default() -> Self {
        Self { shrink: true, optimize: true, obfuscate: true, rules: PathBuf::from("proguard-rules.pro") }
    }
}

/// `[package.jlink]`: the runtime image of `--format jlink` (and of installers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JlinkSettings {
    /// JDK modules to add to the ones `jdeps` finds, e.g. ones only loaded through reflection.
    pub modules: Vec<String>,
    /// Ask `jdeps` which modules the program uses; `false` uses only `modules`.
    pub jdeps: bool,
}

impl Default for JlinkSettings {
    fn default() -> Self {
        Self { modules: Vec::new(), jdeps: true }
    }
}

/// `[package.native-image]`: options for GraalVM's `native-image`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImageSettings {
    /// Extra `native-image` arguments.
    pub args: Vec<String>,
    /// Link a static executable against musl (Linux only).
    pub static_link: bool,
    /// Packages or classes initialized while the image is built.
    pub initialize_at_build_time: Vec<String>,
    /// Use the GraalVM reachability metadata repository for the dependencies.
    pub metadata_repository: bool,
}

impl Default for NativeImageSettings {
    fn default() -> Self {
        Self { args: Vec::new(), static_link: false, initialize_at_build_time: Vec::new(), metadata_repository: true }
    }
}

/// What `jpackage` builds (`[package.installer] type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerType {
    Msi,
    Exe,
    Dmg,
    Pkg,
    Deb,
    Rpm,
    /// The application directory with its launcher and runtime, without an installer.
    AppImage,
}

impl InstallerType {
    pub const ALL: [InstallerType; 7] = [
        InstallerType::Msi,
        InstallerType::Exe,
        InstallerType::Dmg,
        InstallerType::Pkg,
        InstallerType::Deb,
        InstallerType::Rpm,
        InstallerType::AppImage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            InstallerType::Msi => "msi",
            InstallerType::Exe => "exe",
            InstallerType::Dmg => "dmg",
            InstallerType::Pkg => "pkg",
            InstallerType::Deb => "deb",
            InstallerType::Rpm => "rpm",
            InstallerType::AppImage => "app-image",
        }
    }

    /// The OS (`std::env::consts::OS`) that builds this type, or `None` for any.
    pub fn os(self) -> Option<&'static str> {
        match self {
            InstallerType::Msi | InstallerType::Exe => Some("windows"),
            InstallerType::Dmg | InstallerType::Pkg => Some("macos"),
            InstallerType::Deb | InstallerType::Rpm => Some("linux"),
            InstallerType::AppImage => None,
        }
    }

    /// The default type on `os`: `.msi` on Windows, `.dmg` on macOS, `.deb` elsewhere.
    pub fn default_for(os: &str) -> InstallerType {
        match os {
            "windows" => InstallerType::Msi,
            "macos" => InstallerType::Dmg,
            _ => InstallerType::Deb,
        }
    }
}

impl FromStr for InstallerType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match InstallerType::ALL.into_iter().find(|t| t.as_str() == s) {
            Some(t) => Ok(t),
            None => {
                let names: Vec<&str> = InstallerType::ALL.iter().map(|t| t.as_str()).collect();
                bail!("unknown installer type `{s}` (expected one of: {})", names.join(", "))
            }
        }
    }
}

/// The installer's icon on each OS: `.ico` on Windows, `.icns` on macOS, `.png` on Linux.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallerIcons {
    pub windows: Option<PathBuf>,
    pub macos: Option<PathBuf>,
    pub linux: Option<PathBuf>,
}

impl InstallerIcons {
    /// The icon for `os` (`std::env::consts::OS`).
    pub fn for_os(&self, os: &str) -> Option<&Path> {
        match os {
            "windows" => self.windows.as_deref(),
            "macos" => self.macos.as_deref(),
            _ => self.linux.as_deref(),
        }
    }
}

/// `[package.installer]`: options for `jpackage`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallerSettings {
    /// The application name; defaults to the package (or `--bin`) name.
    pub name: Option<String>,
    pub vendor: Option<String>,
    /// Defaults to `[package] description`.
    pub description: Option<String>,
    /// A numeric version such as `1.2.3`; defaults to the numeric start of `[package] version`.
    pub app_version: Option<String>,
    /// The types to build; `jpackage` builds the ones for the host OS. Empty builds the host's default type.
    pub kinds: Vec<InstallerType>,
    pub icon: InstallerIcons,
    pub license_file: Option<PathBuf>,
    /// Add a Start menu entry (Windows) or a menu entry (Linux).
    pub menu: bool,
    /// Add a desktop shortcut (Windows, Linux).
    pub shortcut: bool,
    /// Windows: make the launcher a console program, for command-line tools (`--win-console`).
    pub console: bool,
    /// Windows: the upgrade code that lets a newer `.msi` replace an older one.
    pub upgrade_uuid: Option<String>,
    /// macOS: the bundle identifier, e.g. `com.example.app`.
    pub bundle_id: Option<String>,
    /// macOS: the signing identity (`Developer ID Application: ...`); signs the app when set.
    pub signing_identity: Option<String>,
    /// Linux: the package's category (`--linux-app-category`).
    pub category: Option<String>,
    /// File types the installed application opens (`--file-associations`).
    pub file_associations: Vec<FileAssociation>,
    /// macOS: send the installer to Apple's notary service and staple the ticket to it.
    pub notarize: Option<Notarization>,
}

impl InstallerSettings {
    /// The types to build on `os` (`std::env::consts::OS`): the listed ones that `os` builds, or its
    /// default type when none are listed. Empty when every listed type is built on another OS.
    pub fn kinds_for(&self, os: &str) -> Vec<InstallerType> {
        if self.kinds.is_empty() {
            return vec![InstallerType::default_for(os)];
        }
        self.kinds.iter().copied().filter(|k| k.os().is_none_or(|needed| needed == os)).collect()
    }

    /// The application's name: `name`, or `default` (the package's or the `--bin`'s).
    pub fn app_name<'a>(&'a self, default: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(default)
    }
}

/// A `[package.installer] file-associations` entry: a file type the application opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAssociation {
    /// The file extension, without the dot.
    pub extension: String,
    pub mime_type: String,
    pub description: Option<String>,
    /// The icon of the file type, relative to the project root.
    pub icon: Option<PathBuf>,
}

/// `[package.installer] notarize`: the Apple credentials `xcrun notarytool` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notarization {
    /// An Apple ID and app-specific password, read from environment variables, and the team ID.
    AppleId { apple_id_env: String, password_env: String, team_id: String },
    /// Credentials that `xcrun notarytool store-credentials` saved in the keychain under this name.
    KeychainProfile(String),
}

/// `[build]` (`[workspace.build]` in a workspace): how the build commands run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuildSettings {
    /// How many packages and targets build at the same time; `None` leaves it to `KARGO_JOBS`,
    /// `config.toml` or the default.
    pub jobs: Option<kargo_util::jobs::Jobs>,
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
    /// How long the whole test run may take, from the first test starting; `None` means no limit.
    pub timeout: Option<Duration>,
    /// How long one JVM test may take (`junit.jupiter.execution.timeout.default`); `None` means no limit.
    pub test_timeout: Option<Duration>,
    /// JVM arguments for the test process, after `[target.jvm] jvm-args`.
    pub jvm_args: Vec<String>,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self { fail_fast: true, timeout: None, test_timeout: None, jvm_args: Vec::new() }
    }
}

/// How JMH measures a benchmark (`[bench] mode`, `kargo bench --mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchMode {
    /// Operations per unit of time (`thrpt`).
    Throughput,
    /// Time per operation (`avgt`).
    AverageTime,
    /// Time per operation, sampled, with percentiles (`sample`).
    SampleTime,
    /// Time of a single operation (`ss`).
    SingleShot,
    /// Every mode (`all`).
    All,
}

impl BenchMode {
    pub const ALL: [BenchMode; 5] =
        [BenchMode::Throughput, BenchMode::AverageTime, BenchMode::SampleTime, BenchMode::SingleShot, BenchMode::All];

    /// The mode's name in `kargo.toml` and on JMH's command line.
    pub fn as_str(self) -> &'static str {
        match self {
            BenchMode::Throughput => "thrpt",
            BenchMode::AverageTime => "avgt",
            BenchMode::SampleTime => "sample",
            BenchMode::SingleShot => "ss",
            BenchMode::All => "all",
        }
    }
}

impl fmt::Display for BenchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BenchMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        BenchMode::ALL.into_iter().find(|m| m.as_str() == s).with_context(|| {
            format!("unknown benchmark mode `{s}` (expected one of: {})", BenchMode::ALL.map(BenchMode::as_str).join(", "))
        })
    }
}

/// How many times JMH measures (or warms up) and for how long each time: `5x1s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchRounds {
    pub count: u32,
    pub time: Duration,
}

impl BenchRounds {
    /// Reads `<count>x<duration>`, such as `5x1s` or `3x500ms`. `what` names the setting, for the error.
    pub fn parse(what: &str, value: &str) -> Result<Self> {
        let invalid = || anyhow::anyhow!("{what} must be a count and a duration such as \"5x1s\" or \"3x500ms\", not \"{value}\"");
        let (count, time) = value.split_once('x').ok_or_else(invalid)?;
        let count = count.trim().parse::<u32>().ok().filter(|count| *count > 0).ok_or_else(invalid)?;
        let time = raw::parse_duration_in(what, "duration", time).map_err(|_| invalid())?;
        Ok(Self { count, time })
    }

    /// The duration in JMH's spelling: `500ms`, or whole seconds, minutes and hours.
    pub fn jmh_time(&self) -> String {
        let ms = self.time.as_millis();
        if !ms.is_multiple_of(1000) {
            format!("{ms}ms")
        } else if !ms.is_multiple_of(60_000) {
            format!("{}s", ms / 1000)
        } else if !ms.is_multiple_of(3_600_000) {
            format!("{}m", ms / 60_000)
        } else {
            format!("{}h", ms / 3_600_000)
        }
    }
}

impl fmt::Display for BenchRounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.count, self.jmh_time())
    }
}

/// The units JMH reports times in (`[bench] time-unit`).
pub const BENCH_TIME_UNITS: [&str; 7] = ["ns", "us", "ms", "s", "m", "h", "d"];

/// Checks a benchmark time unit; `what` names the setting, for the error.
pub fn validate_time_unit(what: &str, value: &str) -> Result<String> {
    if BENCH_TIME_UNITS.contains(&value) {
        return Ok(value.to_owned());
    }
    bail!("{what} must be one of {}, not \"{value}\"", BENCH_TIME_UNITS.join(", "))
}

/// `[bench]`: options for `kargo bench`. Unset keys fall back to the benchmarks' own JMH
/// annotations, then to Kargo's defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BenchSettings {
    /// Warmup iterations (`warmup = "5x1s"`).
    pub warmup: Option<BenchRounds>,
    /// Measured iterations (`iterations = "5x1s"`).
    pub iterations: Option<BenchRounds>,
    /// How many JVMs run each benchmark one after the other; `0` runs it in the JVM of the launcher.
    pub forks: Option<u32>,
    pub mode: Option<BenchMode>,
    /// The unit results are reported in (`ns`, `us`, `ms`, `s`, `m`, `h` or `d`).
    pub time_unit: Option<String>,
    /// JVM arguments for the benchmark JVMs, after `[target.jvm] jvm-args`.
    pub jvm_args: Vec<String>,
}

/// How serious a security advisory is, from its CVSS score or the severity its database gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 4] = [Severity::Low, Severity::Medium, Severity::High, Severity::Critical];

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }

    /// The severity of a CVSS base score: 0.1-3.9 low, 4.0-6.9 medium, 7.0-8.9 high, 9.0-10.0 critical
    /// (`None` for 0, which CVSS calls "none").
    pub fn from_score(score: f64) -> Option<Severity> {
        match score {
            s if s >= 9.0 => Some(Severity::Critical),
            s if s >= 7.0 => Some(Severity::High),
            s if s >= 4.0 => Some(Severity::Medium),
            s if s > 0.0 => Some(Severity::Low),
            _ => None,
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The lowest severity that makes `kargo audit` fail (`[audit] deny`, `kargo audit --deny`), or
/// `none` to report advisories without failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DenyLevel {
    Low,
    Medium,
    #[default]
    High,
    Critical,
    None,
}

impl DenyLevel {
    pub const ALL: [DenyLevel; 5] = [DenyLevel::Low, DenyLevel::Medium, DenyLevel::High, DenyLevel::Critical, DenyLevel::None];

    pub fn as_str(self) -> &'static str {
        match self {
            DenyLevel::Low => "low",
            DenyLevel::Medium => "medium",
            DenyLevel::High => "high",
            DenyLevel::Critical => "critical",
            DenyLevel::None => "none",
        }
    }

    /// Whether an advisory of `severity` fails the audit. One without a known severity fails at
    /// every level but `none`, since nothing says it is harmless.
    pub fn denies(self, severity: Option<Severity>) -> bool {
        let threshold = match self {
            DenyLevel::Low => Severity::Low,
            DenyLevel::Medium => Severity::Medium,
            DenyLevel::High => Severity::High,
            DenyLevel::Critical => Severity::Critical,
            DenyLevel::None => return false,
        };
        severity.is_none_or(|s| s >= threshold)
    }
}

impl fmt::Display for DenyLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DenyLevel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        DenyLevel::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .with_context(|| format!("unknown severity `{s}` (expected one of: {})", DenyLevel::ALL.map(DenyLevel::as_str).join(", ")))
    }
}

/// `[audit]`: settings of `kargo audit`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditSettings {
    /// The lowest severity that fails the audit; `kargo audit --deny` overrides it. Unset means `high`.
    pub deny: Option<DenyLevel>,
    /// Advisories that do not apply to this project.
    pub ignore: Vec<AuditIgnore>,
}

/// An advisory `kargo audit` does not report (`[audit] ignore`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditIgnore {
    /// The advisory's id or one of its aliases, such as `GHSA-xxxx-xxxx-xxxx` or `CVE-2024-1234`.
    pub id: String,
    /// Why the advisory does not apply (required).
    pub reason: String,
    /// The last day the entry applies, as `YYYY-MM-DD`; afterwards the advisory is reported again.
    pub until: Option<String>,
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

/// What a `[[generate]]` step writes, and so where Kargo adds its output directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeneratedKind {
    /// Kotlin and Java sources of the main code (the default).
    #[default]
    Sources,
    /// Resources packed with the main code.
    Resources,
    /// Kotlin and Java sources of the tests.
    TestSources,
}

impl GeneratedKind {
    pub const ALL: [GeneratedKind; 3] = [GeneratedKind::Sources, GeneratedKind::Resources, GeneratedKind::TestSources];

    pub fn as_str(self) -> &'static str {
        match self {
            GeneratedKind::Sources => "sources",
            GeneratedKind::Resources => "resources",
            GeneratedKind::TestSources => "test-sources",
        }
    }
}

impl fmt::Display for GeneratedKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GeneratedKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        GeneratedKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .with_context(|| format!("unknown kind `{s}` (expected one of: {})", GeneratedKind::ALL.map(GeneratedKind::as_str).join(", ")))
    }
}

/// The built-in generators a `[[generate]]` step can name with `builtin`.
pub const BUILTIN_GENERATORS: [&str; 2] = ["build-config", "protobuf"];

/// `builtin = "build-config"`: a Kotlin `object` with the package's version and Git commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildConfigGenerator {
    /// The Kotlin package; the default follows the package's group and name, as for `[env] expose`.
    pub package: Option<String>,
    /// The name of the object (`BuildConfig`).
    pub object: String,
    /// Constants of your own.
    pub fields: BTreeMap<String, BuildConfigValue>,
    /// Variables of the project environment (`[env]`, `.env` files) to expose, as `[env] expose` does.
    pub expose: Vec<String>,
    /// Also write `BUILD_TIME`. It changes on every build unless `SOURCE_DATE_EPOCH` is set.
    pub build_time: bool,
}

/// `builtin = "protobuf"`: `protoc` and, for gRPC, its plugins, downloaded from Maven Central.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtobufGenerator {
    /// The `protoc` version; the default is Kargo's own.
    pub version: Option<String>,
    /// Directories searched for imports (`-I`), relative to the project root.
    pub proto_path: Vec<String>,
    /// Generate Java classes (`--java_out`).
    pub java: bool,
    /// Generate the Kotlin DSL (`--kotlin_out`).
    pub kotlin: bool,
    /// Also generate gRPC stubs: Java ones, and Kotlin ones with `kotlin`.
    pub grpc: bool,
    pub grpc_java_version: Option<String>,
    pub grpc_kotlin_version: Option<String>,
    /// More arguments for `protoc`.
    pub args: Vec<String>,
}

/// How a `[[generate]]` step produces its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratorAction {
    /// `run`: shell commands, as a task has.
    Run {
        run: Vec<String>,
        windows_run: Option<Vec<String>>,
        shell: Option<TaskShell>,
    },
    /// `tool`: the main class of a Maven artifact, run on the project's JDK.
    Tool {
        module: ModuleId,
        version: String,
        main_class: Option<String>,
        args: Vec<String>,
    },
    BuildConfig(BuildConfigGenerator),
    Protobuf(ProtobufGenerator),
}

/// A step from `[[generate]]` that writes sources or resources before they are compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generator {
    pub name: String,
    /// Globs of the files the output comes from, relative to the project root. Without any, the
    /// step runs on every build.
    pub inputs: Vec<String>,
    pub action: GeneratorAction,
    /// The targets whose build uses the output.
    pub targets: Vec<TargetKind>,
    pub kind: GeneratedKind,
    /// The directories of the output, relative to it, that are the roots of the sources or resources. Empty
    /// means the whole output directory. A tool that writes a whole project (`src/main/kotlin`, `src/test/kotlin`,
    /// docs) names the directory to compile.
    pub roots: Vec<String>,
    /// Steps that run before this one.
    pub after: Vec<String>,
    /// Extra environment variables of the step.
    pub env: BTreeMap<String, String>,
}

impl Generator {
    /// Whether the output goes to `target`.
    pub fn applies_to(&self, target: TargetKind) -> bool {
        self.targets.contains(&target)
    }

    /// The `builtin` name of the step, if it is a built-in one.
    pub fn builtin(&self) -> Option<&'static str> {
        match self.action {
            GeneratorAction::BuildConfig(_) => Some("build-config"),
            GeneratorAction::Protobuf(_) => Some("protobuf"),
            _ => None,
        }
    }
}

/// A fully validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub package: Package,
    pub kotlin: KotlinSettings,
    pub targets: Targets,
    /// Extra programs from `[[bin]]`, in declaration order.
    pub bins: Vec<Bin>,
    /// The `[[generate]]` steps, in declaration order.
    pub generators: Vec<Generator>,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    /// `path` dependencies from every dependency table.
    pub path_dependencies: Vec<PathDependency>,
    pub repositories: Vec<Repository>,
    pub frameworks: Frameworks,
    pub compiler_plugins: Vec<CompilerPlugin>,
    pub process_plugins: Vec<ProcessPlugin>,
    pub tasks: BTreeMap<String, Task>,
    /// `[env]`: variables for `kargo run`, `kargo test`, tasks and hooks.
    pub env: EnvTable,
    /// Every profile by name: `dev` and `release` (always present) and the custom `[profile.<name>]` tables.
    pub profiles: BTreeMap<String, Profile>,
    pub toolchain: ToolchainSettings,
    /// `[package.jlink]`, `[package.native-image]` and `[package.installer]`.
    pub packaging: PackagingSettings,
    pub doc: DocSettings,
    pub fmt: FmtSettings,
    /// `[build]`: settings of every command that builds.
    pub build: BuildSettings,
    pub lint: LintSettings,
    pub test: TestSettings,
    pub bench: BenchSettings,
    pub audit: AuditSettings,
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

    /// The `[[generate]]` step named `name`.
    pub fn generator(&self, name: &str) -> Option<&Generator> {
        self.generators.iter().find(|g| g.name == name)
    }

    /// Dependencies for `target`, including the common ones.
    pub fn dependencies_for(&self, target: TargetKind) -> Vec<Dependency> {
        self.dependencies.iter().chain(self.targets.dependencies(target)).cloned().collect()
    }

    /// The profile named `name`: `dev`, `release` or a custom `[profile.<name>]`.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
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
    /// `[workspace.build]`: what `[build]` is for a single package, for the whole workspace.
    pub build: BuildSettings,
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

/// A Wasm or JS module name (of `[target.<target>]`) becomes file names and an import path, so it stays portable.
fn validate_module_name(target: &str, name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('.') || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("invalid [target.{target}] module-name `{name}` (use letters, digits, `-`, `_` and `.`, not starting with `.`)");
    }
    Ok(())
}

/// npm package names: lowercase, URL-safe, optionally scoped (`@scope/name`).
fn validate_npm_name(target: &str, name: &str) -> Result<()> {
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
            "invalid [target.{target}] npm-name `{name}` (use lowercase letters, digits, `-`, `.`, `_` and `~`, optionally scoped like `@scope/name`)"
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

/// Fails unless `version` (the value of `what`, such as `[kotlin] version`) is one concrete version.
/// Unlike [`validate_version_string`], it rejects Maven ranges (`[1.0,2.0)`), wildcards (`2.+`,
/// `1.*`) and anything with whitespace, so the value can name a jar, a POM or a compiler download.
/// A `+` inside a version stays valid: it is SemVer build metadata (`1.0.0+build.5`).
fn validate_exact_version(what: &str, example: &str, version: &str) -> Result<()> {
    if version.is_empty() || version.ends_with('+') || version.contains(|c: char| c.is_whitespace() || "[](),*".contains(c)) {
        bail!("{what} must be one exact version such as \"{example}\", got `{version}`");
    }
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
