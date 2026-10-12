//! Serde model of `kargo.toml` and its validation into [`super::Manifest`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml::Value;

use super::*;
use kargo_maven::Repository;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RawManifest {
    package: RawPackage,
    #[serde(default)]
    kotlin: RawKotlin,
    #[serde(default)]
    target: Option<RawTargets>,
    #[serde(default)]
    bin: Vec<RawBin>,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default)]
    dev_dependencies: toml::Table,
    #[serde(default)]
    repositories: toml::Table,
    #[serde(default)]
    frameworks: RawFrameworks,
    #[serde(default)]
    plugins: toml::Table,
    #[serde(default)]
    tasks: toml::Table,
    #[serde(default)]
    profile: RawProfiles,
    #[serde(default)]
    toolchain: RawToolchain,
    #[serde(default)]
    doc: RawDoc,
    #[serde(default)]
    fmt: RawFmt,
    #[serde(default)]
    lint: RawLint,
    #[serde(default)]
    test: RawTest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawBin {
    name: String,
    main: Option<String>,
    entry_point: Option<String>,
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawToolchain {
    jdk: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawDoc {
    dokka: Option<String>,
    #[serde(default)]
    includes: Vec<PathBuf>,
    source_link: Option<String>,
    #[serde(default)]
    external_links: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawFmt {
    ktlint: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTest {
    fail_fast: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawLint {
    detekt: Option<String>,
    #[serde(default)]
    config: Vec<PathBuf>,
    baseline: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    group: Option<String>,
    description: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    license: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawKotlin {
    version: Option<String>,
    language_version: Option<String>,
    api_version: Option<String>,
    ksp: Option<String>,
    #[serde(default)]
    opt_in: Vec<String>,
    #[serde(default)]
    compiler_args: Vec<String>,
    #[serde(default)]
    warnings_as_errors: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTargets {
    jvm: Option<RawJvm>,
    wasm: Option<RawWasm>,
    native: Option<RawNative>,
    android: Option<toml::Table>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawJvm {
    jvm_target: Option<String>,
    main_class: Option<String>,
    #[serde(default)]
    jvm_args: Vec<String>,
    #[serde(default)]
    javac_args: Vec<String>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWasm {
    environment: Option<String>,
    package_manager: Option<String>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNative {
    #[serde(default)]
    platforms: Vec<String>,
    kind: Option<String>,
    entry_point: Option<String>,
    #[serde(default)]
    linker_options: Vec<String>,
    #[serde(default)]
    cinterop: BTreeMap<String, RawCinterop>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawCinterop {
    def: Option<PathBuf>,
    package: Option<String>,
    #[serde(default)]
    headers: Vec<String>,
    #[serde(default)]
    compiler_options: Vec<String>,
    #[serde(default)]
    linker_options: Vec<String>,
}

/// Android build settings (rendered into the generated Gradle project).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidConfig {
    pub namespace: String,
    pub application_id: Option<String>,
    #[serde(default = "default_min_sdk")]
    pub min_sdk: u32,
    #[serde(default = "default_sdk")]
    pub target_sdk: u32,
    #[serde(default = "default_compile_sdk")]
    pub compile_sdk: u32,
    #[serde(default = "default_version_code")]
    pub version_code: u32,
    pub version_name: Option<String>,
    /// `application` (default) or `library`.
    #[serde(default = "default_android_kind")]
    pub kind: String,
    /// Android Gradle Plugin version.
    pub agp_version: Option<String>,
}

fn default_min_sdk() -> u32 {
    24
}
fn default_sdk() -> u32 {
    36
}
fn default_compile_sdk() -> u32 {
    37
}
fn default_version_code() -> u32 {
    1
}
fn default_android_kind() -> String {
    "application".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawFrameworks {
    compose: Option<Toggle<ComposeConfig>>,
    serialization: Option<Toggle<SerializationConfig>>,
    coroutines: Option<Value>,
    ktor: Option<Toggle<KtorConfig>>,
    lombok: Option<Toggle<LombokConfig>>,
    spring_boot: Option<Toggle<SpringBootConfig>>,
}

/// `feature = true`, `feature = false` or `feature = { ...options }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Toggle<T> {
    Enabled(bool),
    Config(T),
}

impl<T: Default> Toggle<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Toggle::Enabled(true) => Some(T::default()),
            Toggle::Enabled(false) => None,
            Toggle::Config(c) => Some(c),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComposeConfig {
    /// Compose Multiplatform version.
    pub version: Option<String>,
    /// Add Material 3 (default true).
    pub material3: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SerializationConfig {
    pub version: Option<String>,
    /// Formats such as `json`, `protobuf`, `cbor`, `properties` (default `json`).
    pub formats: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KtorConfig {
    pub version: Option<String>,
    /// Server engine (`netty`, `cio`, `jetty`, `tomcat`), or omitted for no server.
    pub server: Option<String>,
    /// Client engine (`cio`, `okhttp`, `java`, `apache5`, `js`, `curl`, `winhttp`, `darwin`).
    pub client: Option<String>,
    /// Extra server plugins, e.g. `content-negotiation`, `cors`, `auth`.
    #[serde(default)]
    pub server_plugins: Vec<String>,
    /// Extra client plugins, e.g. `content-negotiation`, `logging`, `auth`.
    #[serde(default)]
    pub client_plugins: Vec<String>,
    /// Add `ktor-serialization-kotlinx-json` (default true when serialization is enabled).
    pub json: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LombokConfig {
    pub version: Option<String>,
    /// Path to `lombok.config`, relative to the project root.
    pub config: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SpringBootConfig {
    pub version: Option<String>,
    /// Starters without the `spring-boot-starter-` prefix (default `["web"]`).
    pub starters: Option<Vec<String>>,
    /// Apply the JPA no-arg preset (default: true if `data-jpa` is a starter).
    pub jpa: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawProfiles {
    dev: Option<RawProfile>,
    release: Option<RawProfile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawProfile {
    debug: Option<bool>,
    optimize: Option<bool>,
    #[serde(default)]
    compiler_args: Vec<String>,
}

impl RawProfile {
    fn apply(self, mut base: Profile) -> Profile {
        if let Some(d) = self.debug {
            base.debug = d;
        }
        if let Some(o) = self.optimize {
            base.optimize = o;
        }
        base.compiler_args = self.compiler_args;
        base
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct DetailedDependency {
    path: Option<PathBuf>,
    group: Option<String>,
    artifact: Option<String>,
    coords: Option<String>,
    version: Option<String>,
    scope: Option<String>,
    classifier: Option<String>,
    #[serde(alias = "type")]
    extension: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    force: bool,
    transitive: Option<bool>,
    #[serde(default)]
    bom: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRepository {
    url: String,
    username_env: Option<String>,
    password_env: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPlugin {
    maven: Option<String>,
    id: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, String>,
    command: Option<String>,
    windows_command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    windows_args: Option<Vec<String>>,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTask {
    description: Option<String>,
    run: Option<Value>,
    windows_run: Option<Value>,
    shell: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    hooks: Vec<String>,
}

/// Parses a task's `run` or `windows-run`: a string or an array of strings.
fn parse_run(task: &str, key: &str, value: Option<Value>) -> Result<Option<Vec<String>>> {
    Ok(match value {
        None => None,
        Some(Value::String(s)) => Some(vec![s]),
        Some(Value::Array(items)) => Some(
            items
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s),
                    _ => bail!("task `{task}`: `{key}` entries must be strings"),
                })
                .collect::<Result<_>>()?,
        ),
        Some(_) => bail!("task `{task}`: `{key}` must be a string or an array of strings"),
    })
}

/// A parsed `[dependencies]` entry: a Maven module or another Kargo package on disk.
enum Entry {
    Maven(Dependency),
    Path(PathDependency),
}

/// Replaces a `{ workspace = true }` entry with the matching `[workspace.dependencies]` entry.
fn inherit_dependency(name: &str, value: &Value, section: &str, workspace: Option<&WorkspaceManifest>) -> Result<Option<Value>> {
    let Value::Table(table) = value else { return Ok(None) };
    let Some(flag) = table.get("workspace") else { return Ok(None) };
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    if flag.as_bool() != Some(true) {
        bail!("{}: `workspace` can only be `true`", ctx());
    }
    if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "workspace" | "scope")) {
        bail!("{}: `{other}` cannot be combined with `workspace = true`; set it in [workspace.dependencies] instead", ctx());
    }
    let Some(workspace) = workspace else {
        bail!("{}: `workspace = true` needs a [workspace] that lists this package in `members`", ctx());
    };
    let Some(inherited) = workspace.dependencies.get(name) else {
        bail!("{}: `{name}` is not declared in [workspace.dependencies]", ctx());
    };
    let mut inherited = match inherited {
        Value::String(version) => toml::Table::from_iter([("version".to_owned(), Value::String(version.clone()))]),
        Value::Table(t) => t.clone(),
        _ => bail!("{}: the [workspace.dependencies] entry must be a version string or a table", ctx()),
    };
    // Workspace paths are relative to the workspace root, not to the member.
    if let Some(Value::String(path)) = inherited.get("path") {
        let absolute = workspace.root.join(path);
        inherited.insert("path".to_owned(), Value::String(absolute.to_string_lossy().into_owned()));
    }
    if let Some(scope) = table.get("scope") {
        inherited.insert("scope".to_owned(), scope.clone());
    }
    Ok(Some(Value::Table(inherited)))
}

fn parse_dependency(name: &str, value: &Value, section: &str, workspace: Option<&WorkspaceManifest>) -> Result<Entry> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    let inherited = inherit_dependency(name, value, section, workspace)?;
    let value = inherited.as_ref().unwrap_or(value);
    let detailed: DetailedDependency = match value {
        Value::String(version) => DetailedDependency { version: Some(version.clone()), ..Default::default() },
        Value::Table(_) => value.clone().try_into().with_context(ctx)?,
        _ => bail!("{}: expected a version string or a table", ctx()),
    };

    if let Some(path) = detailed.path.clone() {
        return path_dependency(name, path, detailed, section).map(Entry::Path);
    }
    let module: ModuleId = if name.contains(':') {
        if detailed.coords.is_some() || detailed.group.is_some() || detailed.artifact.is_some() {
            bail!("{}: `coords`, `group` and `artifact` are not allowed when the key is already `group:artifact`", ctx());
        }
        name.parse().with_context(ctx)?
    } else if let Some(coords) = &detailed.coords {
        if detailed.group.is_some() || detailed.artifact.is_some() {
            bail!("{}: use either `coords` or `group`/`artifact`, not both", ctx());
        }
        let spec: CoordSpec = coords.parse().with_context(ctx)?;
        if spec.version.is_some() && detailed.version.is_some() {
            bail!("{}: version given both in `coords` and `version`", ctx());
        }
        let version = spec.version.clone().or(detailed.version.clone());
        return finish_dependency(name, spec.module, version, detailed, section).map(Entry::Maven);
    } else if let Some(group) = &detailed.group {
        ModuleId::new(group, detailed.artifact.clone().unwrap_or_else(|| name.to_owned())).to_string().parse().with_context(ctx)?
    } else {
        bail!("{}: the key must be `group:artifact`, or the table must specify `coords`, `group` or `path`", ctx());
    };
    let version = detailed.version.clone();
    finish_dependency(name, module, version, detailed, section).map(Entry::Maven)
}

/// `name = { path = "../name" }`: another Kargo package, built from source.
fn path_dependency(name: &str, path: PathBuf, detailed: DetailedDependency, section: &str) -> Result<PathDependency> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    let maven_only = [
        ("group", detailed.group.is_some()),
        ("artifact", detailed.artifact.is_some()),
        ("coords", detailed.coords.is_some()),
        ("version", detailed.version.is_some()),
        ("classifier", detailed.classifier.is_some()),
        ("extension", detailed.extension.is_some()),
        ("exclude", !detailed.exclude.is_empty()),
        ("force", detailed.force),
        ("transitive", detailed.transitive.is_some()),
        ("bom", detailed.bom),
    ];
    if let Some((key, _)) = maven_only.iter().find(|(_, set)| *set) {
        bail!("{}: `{key}` does not apply to a `path` dependency, which is built from source (only `scope` can be set)", ctx());
    }
    validate_package_name(name).with_context(|| format!("{}: the key of a `path` dependency is the package name", ctx()))?;
    if path.as_os_str().is_empty() {
        bail!("{}: `path` must not be empty", ctx());
    }
    let scope = match &detailed.scope {
        Some(s) => s.parse().with_context(ctx)?,
        None => Scope::Compile,
    };
    if matches!(scope, Scope::Processor | Scope::Ksp) {
        bail!("{}: a `path` dependency can't be an annotation or KSP processor yet; depend on it as `compile` or `runtime`", ctx());
    }
    Ok(PathDependency { name: name.to_owned(), path, scope, dev: false, target: None })
}

fn finish_dependency(
    name: &str,
    module: ModuleId,
    version: Option<String>,
    detailed: DetailedDependency,
    section: &str,
) -> Result<Dependency> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    if let Some(v) = &version {
        validate_version_string("version", v).with_context(ctx)?;
    }
    if detailed.bom && version.is_none() {
        bail!("{}: a BOM needs a version", ctx());
    }
    let exclusions = detailed
        .exclude
        .iter()
        .map(|e| {
            e.parse::<ModuleId>()
                .or_else(|_| match e.split_once(':') {
                    Some((g, "*")) if !g.is_empty() => Ok(ModuleId::new(g, "*")),
                    _ => Err(anyhow::anyhow!("invalid exclusion `{e}` (expected `group:artifact` or `group:*`)")),
                })
                .with_context(ctx)
        })
        .collect::<Result<Vec<_>>>()?;
    let scope = match &detailed.scope {
        Some(s) => s.parse().with_context(ctx)?,
        None => Scope::Compile,
    };
    Ok(Dependency {
        name: name.to_owned(),
        module,
        version,
        scope,
        classifier: detailed.classifier,
        extension: detailed.extension,
        exclusions,
        force: detailed.force,
        transitive: detailed.transitive.unwrap_or(true),
        bom: detailed.bom,
    })
}

/// Parses the dependency tables of one manifest, collecting `path` dependencies on the side.
struct DependencyParser<'a> {
    workspace: Option<&'a WorkspaceManifest>,
    paths: Vec<PathDependency>,
}

impl DependencyParser<'_> {
    fn parse(&mut self, table: &toml::Table, section: &str, dev: bool, target: Option<TargetKind>) -> Result<Vec<Dependency>> {
        let mut deps: Vec<Dependency> = Vec::new();
        for (name, value) in table {
            match parse_dependency(name, value, section, self.workspace)? {
                Entry::Maven(dep) => {
                    if deps.iter().any(|d| d.same_artifact(&dep)) {
                        bail!("dependency `{}` is declared more than once in [{section}]", dep.module);
                    }
                    deps.push(dep);
                }
                Entry::Path(path) => self.paths.push(PathDependency { dev, target, ..path }),
            }
        }
        Ok(deps)
    }
}

fn parse_hooks(hooks: &[String], ctx: &str) -> Result<Vec<Hook>> {
    hooks.iter().map(|h| h.parse::<Hook>().with_context(|| ctx.to_owned())).collect()
}

const KONAN_TARGETS: &[&str] = &[
    "host",
    "linux_x64",
    "linux_arm64",
    "mingw_x64",
    "macos_x64",
    "macos_arm64",
    "ios_arm64",
    "ios_x64",
    "ios_simulator_arm64",
    "watchos_arm64",
    "watchos_simulator_arm64",
    "tvos_arm64",
    "tvos_simulator_arm64",
    "android_arm64",
    "android_arm32",
    "android_x64",
    "android_x86",
];

/// Accepts both Konan names (`linux_x64`) and Gradle-style names (`linuxX64`).
pub fn normalize_konan_target(name: &str) -> Result<String> {
    let mut snake = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
    }
    let snake = snake.replace('-', "_");
    let snake = match snake.as_str() {
        "android_native_arm64" => "android_arm64".to_owned(),
        "android_native_arm32" => "android_arm32".to_owned(),
        "android_native_x64" => "android_x64".to_owned(),
        "android_native_x86" => "android_x86".to_owned(),
        _ => snake,
    };
    if !KONAN_TARGETS.contains(&snake.as_str()) {
        bail!("unknown Kotlin/Native platform `{name}` (expected one of: {})", KONAN_TARGETS.join(", "));
    }
    Ok(snake)
}

impl RawManifest {
    pub(super) fn validate(self, workspace: Option<&WorkspaceManifest>) -> Result<Manifest> {
        validate_package_name(&self.package.name)?;
        validate_version_string("package version", &self.package.version)?;
        if let Some(group) = &self.package.group
            && (group.is_empty()
                || group.split('.').any(|seg| seg.is_empty() || !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')))
        {
            bail!("invalid package group `{group}` (expected something like `com.example`)");
        }

        let workspace_kotlin = workspace.and_then(|w| w.kotlin_version.clone());
        let (kotlin_version, kotlin_version_set) = match (self.kotlin.version, workspace_kotlin) {
            (Some(own), Some(shared)) if own != shared => bail!(
                "[kotlin] version {own} differs from the workspace's [workspace.kotlin] version {shared};                  remove it to use the workspace's, since every member builds with the same Kotlin"
            ),
            (Some(v), _) | (None, Some(v)) => (v, true),
            (None, None) => (DEFAULT_KOTLIN_VERSION.to_owned(), false),
        };
        validate_version_string("Kotlin version", &kotlin_version)?;
        if let Some(ksp) = &self.kotlin.ksp {
            validate_version_string("KSP version", ksp)?;
        }
        let mut deps = DependencyParser { workspace, paths: Vec::new() };

        let mut targets = match self.target {
            // No [target] section means a plain JVM project.
            None => Targets {
                jvm: Some(JvmTarget {
                    jvm_target: DEFAULT_JVM_TARGET.to_owned(),
                    jvm_target_set: false,
                    main_class: None,
                    jvm_args: Vec::new(),
                    javac_args: Vec::new(),
                    dependencies: Vec::new(),
                }),
                ..Default::default()
            },
            Some(raw) => {
                let targets = Targets {
                    jvm: raw
                        .jvm
                        .map(|j| -> Result<JvmTarget> {
                            Ok(JvmTarget {
                                jvm_target_set: j.jvm_target.is_some(),
                                jvm_target: match j.jvm_target {
                                    Some(target) => normalize_jvm_target(&target)?,
                                    None => DEFAULT_JVM_TARGET.to_owned(),
                                },
                                main_class: j.main_class,
                                jvm_args: j.jvm_args,
                                javac_args: j.javac_args,
                                dependencies: deps.parse(&j.dependencies, "target.jvm.dependencies", false, Some(TargetKind::Jvm))?,
                            })
                        })
                        .transpose()?,
                    wasm: raw
                        .wasm
                        .map(|w| -> Result<WasmTarget> {
                            let environment = match w.environment.as_deref() {
                                None | Some("js") => WasmEnvironment::Js,
                                Some("wasi") => WasmEnvironment::Wasi,
                                Some(other) => bail!("unknown wasm environment `{other}` (expected `js` or `wasi`)"),
                            };
                            let package_manager = w.package_manager.as_deref().map(str::parse).transpose()?;
                            Ok(WasmTarget {
                                environment,
                                package_manager,
                                dependencies: deps.parse(&w.dependencies, "target.wasm.dependencies", false, Some(TargetKind::Wasm))?,
                            })
                        })
                        .transpose()?,
                    native: raw
                        .native
                        .map(|n| -> Result<NativeTarget> {
                            let platforms = if n.platforms.is_empty() {
                                vec!["host".to_owned()]
                            } else {
                                n.platforms.iter().map(|p| normalize_konan_target(p)).collect::<Result<Vec<_>>>()?
                            };
                            let kind = match n.kind.as_deref() {
                                None | Some("executable") | Some("program") => NativeKind::Executable,
                                Some("library") | Some("klib") => NativeKind::Library,
                                Some("shared") | Some("dynamic") => NativeKind::Shared,
                                Some("static") => NativeKind::Static,
                                Some(other) => bail!("unknown native kind `{other}` (expected executable, library, shared or static)"),
                            };
                            let cinterop = n
                                .cinterop
                                .into_iter()
                                .map(|(name, c)| -> Result<Cinterop> {
                                    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                                        bail!(
                                            "invalid cinterop name `{name}` in [target.native.cinterop] (use letters, digits, `_` and `-`)"
                                        );
                                    }
                                    if let Some(pkg) = &c.package
                                        && (pkg.is_empty() || !pkg.split('.').all(|part| !part.is_empty()))
                                    {
                                        bail!("invalid package `{pkg}` in [target.native.cinterop.{name}]");
                                    }
                                    Ok(Cinterop {
                                        name,
                                        def: c.def,
                                        package: c.package,
                                        headers: c.headers,
                                        compiler_options: c.compiler_options,
                                        linker_options: c.linker_options,
                                    })
                                })
                                .collect::<Result<Vec<_>>>()?;
                            Ok(NativeTarget {
                                platforms,
                                kind,
                                entry_point: n.entry_point,
                                linker_options: n.linker_options,
                                cinterop,
                                dependencies: deps.parse(&n.dependencies, "target.native.dependencies", false, Some(TargetKind::Native))?,
                            })
                        })
                        .transpose()?,
                    android: raw
                        .android
                        .map(|mut table| -> Result<AndroidTarget> {
                            let dependencies = match table.remove("dependencies") {
                                None => toml::Table::new(),
                                Some(Value::Table(t)) => t,
                                Some(_) => bail!("[target.android.dependencies] must be a table"),
                            };
                            let config: AndroidConfig = Value::Table(table).try_into().context("invalid [target.android] section")?;
                            if config.namespace.trim().is_empty() {
                                bail!("[target.android] `namespace` must not be empty");
                            }
                            if !matches!(config.kind.as_str(), "application" | "library") {
                                bail!("[target.android] `kind` must be `application` or `library`");
                            }
                            if config.min_sdk > config.target_sdk || config.target_sdk > config.compile_sdk {
                                bail!("[target.android] expected min-sdk <= target-sdk <= compile-sdk");
                            }
                            let dependencies =
                                deps.parse(&dependencies, "target.android.dependencies", false, Some(TargetKind::Android))?;
                            Ok(AndroidTarget { config, dependencies })
                        })
                        .transpose()?,
                };
                if targets.enabled().is_empty() {
                    bail!("[target] is present but no target is enabled; add e.g. [target.jvm]");
                }
                targets
            }
        };

        let mut repositories = vec![Repository::maven_central()];
        if targets.android.is_some() {
            repositories.push(Repository::google());
        }
        for (name, value) in &self.repositories {
            let raw: RawRepository = match value {
                Value::String(url) => RawRepository { url: url.clone(), username_env: None, password_env: None },
                Value::Table(_) => value.clone().try_into().with_context(|| format!("invalid repository `{name}`"))?,
                _ => bail!("invalid repository `{name}`: expected a URL or a table"),
            };
            let url = raw.url.trim();
            if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("file:")) {
                bail!("repository `{name}` must have an http(s):// or file: URL, got `{url}`");
            }
            let mut repo = Repository::new(name.clone(), url);
            repo.credentials = credentials_from_env(name, raw.username_env.as_deref(), raw.password_env.as_deref())?;
            if !repositories.iter().any(|r| r.url == repo.url) {
                repositories.push(repo);
            }
        }

        let frameworks = Frameworks {
            compose: self.frameworks.compose.and_then(Toggle::into_option),
            serialization: self.frameworks.serialization.and_then(Toggle::into_option),
            coroutines: match self.frameworks.coroutines {
                None | Some(Value::Boolean(false)) => None,
                Some(Value::Boolean(true)) => Some(CoroutinesConfig { version: None }),
                Some(Value::String(v)) => {
                    validate_version_string("coroutines version", &v)?;
                    Some(CoroutinesConfig { version: Some(v) })
                }
                Some(_) => bail!("[frameworks] `coroutines` must be true, false or a version string"),
            },
            ktor: self.frameworks.ktor.and_then(Toggle::into_option),
            lombok: self.frameworks.lombok.and_then(Toggle::into_option),
            spring_boot: self.frameworks.spring_boot.and_then(Toggle::into_option),
        };
        // Compose Multiplatform depends on androidx artifacts hosted by Google.
        if frameworks.compose.is_some() && !repositories.iter().any(|r| r.url == kargo_maven::repo::GOOGLE) {
            repositories.insert(1, Repository::google());
        }
        if let Some(ktor) = &frameworks.ktor
            && ktor.server.is_none()
            && ktor.client.is_none()
        {
            bail!("[frameworks.ktor] enable at least one of `server = \"netty\"` or `client = \"cio\"`");
        }

        let mut compiler_plugins = Vec::new();
        let mut process_plugins = Vec::new();
        for (name, value) in &self.plugins {
            let raw: RawPlugin = value.clone().try_into().with_context(|| format!("invalid plugin `{name}`"))?;
            match (&raw.maven, &raw.command) {
                (Some(maven), None) => {
                    if !raw.hooks.is_empty()
                        || !raw.args.is_empty()
                        || !raw.env.is_empty()
                        || raw.windows_command.is_some()
                        || raw.windows_args.is_some()
                    {
                        bail!(
                            "plugin `{name}`: `hooks`, `args`, `env`, `windows-command` and `windows-args` only apply to `command` plugins"
                        );
                    }
                    let artifact: CoordSpec = maven.parse().with_context(|| format!("invalid plugin `{name}`"))?;
                    if artifact.version.is_none() {
                        bail!("plugin `{name}`: `maven` must include a version (`group:artifact:version`)");
                    }
                    if !raw.options.is_empty() && raw.id.is_none() {
                        bail!("plugin `{name}`: `options` require the compiler plugin `id`");
                    }
                    compiler_plugins.push(CompilerPlugin { name: name.clone(), artifact, id: raw.id, options: raw.options });
                }
                (None, Some(command)) => {
                    if raw.id.is_some() || !raw.options.is_empty() {
                        bail!("plugin `{name}`: `id` and `options` only apply to `maven` compiler plugins");
                    }
                    if raw.hooks.is_empty() {
                        bail!("plugin `{name}`: a `command` plugin needs at least one hook (e.g. hooks = [\"post-build\"])");
                    }
                    process_plugins.push(ProcessPlugin {
                        name: name.clone(),
                        command: command.clone(),
                        windows_command: raw.windows_command,
                        args: raw.args,
                        windows_args: raw.windows_args,
                        hooks: parse_hooks(&raw.hooks, &format!("plugin `{name}`"))?,
                        env: raw.env,
                    });
                }
                (Some(_), Some(_)) => bail!("plugin `{name}`: specify either `maven` or `command`, not both"),
                (None, None) if raw.windows_command.is_some() => {
                    bail!("plugin `{name}`: `windows-command` needs a `command` to use on other platforms")
                }
                (None, None) => bail!("plugin `{name}`: specify `maven = \"group:artifact:version\"` or `command = \"...\"`"),
            }
        }

        let mut tasks = BTreeMap::new();
        for (name, value) in &self.tasks {
            if name.is_empty() || name.contains(char::is_whitespace) {
                bail!("invalid task name `{name}`");
            }
            let raw: RawTask = match value {
                Value::String(cmd) => RawTask { run: Some(Value::String(cmd.clone())), ..Default::default() },
                Value::Table(_) => value.clone().try_into().with_context(|| format!("invalid task `{name}`"))?,
                _ => bail!("invalid task `{name}`: expected a command string or a table"),
            };
            let run = parse_run(name, "run", raw.run)?.unwrap_or_default();
            let windows_run = parse_run(name, "windows-run", raw.windows_run)?;
            if windows_run.is_some() && run.is_empty() {
                bail!("task `{name}`: `windows-run` needs a `run` to use on other platforms");
            }
            let shell = match raw.shell.as_deref() {
                None => None,
                Some(s) => Some(TaskShell::parse(s).with_context(|| {
                    let known: Vec<_> = TaskShell::ALL.iter().map(|s| s.as_str()).collect();
                    format!("task `{name}`: unknown shell `{s}`; expected one of {}", known.join(", "))
                })?),
            };
            if run.is_empty() && raw.depends_on.is_empty() {
                bail!("task `{name}` has nothing to do: add `run` or `depends-on`");
            }
            tasks.insert(
                name.clone(),
                Task {
                    name: name.clone(),
                    description: raw.description,
                    run,
                    windows_run,
                    shell,
                    depends_on: raw.depends_on,
                    env: raw.env,
                    cwd: raw.cwd,
                    hooks: parse_hooks(&raw.hooks, &format!("task `{name}`"))?,
                },
            );
        }
        for task in tasks.values() {
            for dep in &task.depends_on {
                if !tasks.contains_key(dep) {
                    bail!("task `{}` depends on unknown task `{dep}`", task.name);
                }
            }
        }

        let jdk = match self.toolchain.jdk {
            None => JdkRequest::System,
            Some(Value::String(s)) => s.parse().context("invalid [toolchain] `jdk`")?,
            Some(Value::Integer(i)) => {
                u32::try_from(i).ok().map(|m| m.to_string()).unwrap_or_default().parse().context("invalid [toolchain] `jdk`")?
            }
            Some(_) => bail!("[toolchain] `jdk` must be a version like \"21\" or \"system\""),
        };
        if let (Some(major), Some(jvm)) = (jdk.major(), &mut targets.jvm) {
            // The default target never exceeds the pinned JDK; an explicit one must not.
            jvm.jvm_target = jvm.target_for(major)?;
        }

        let dependencies = deps.parse(&self.dependencies, "dependencies", false, None)?;
        let dev_dependencies = deps.parse(&self.dev_dependencies, "dev-dependencies", true, None)?;
        let path_dependencies = deps.paths;
        for (i, dep) in path_dependencies.iter().enumerate() {
            if path_dependencies[..i].iter().any(|d| d.name == dep.name && d.dev == dep.dev && d.target == dep.target) {
                bail!("path dependency `{}` is declared more than once", dep.name);
            }
        }

        let bins = validate_bins(self.bin)?;
        let doc = self.doc.validate()?;
        let fmt = self.fmt.validate()?;
        let lint = self.lint.validate()?;

        Ok(Manifest {
            toolchain: ToolchainSettings { jdk },
            doc,
            fmt,
            lint,
            test: TestSettings { fail_fast: self.test.fail_fast.unwrap_or(true) },
            package: Package {
                name: self.package.name,
                version: self.package.version,
                group: self.package.group,
                description: self.package.description,
                authors: self.package.authors,
                license: self.package.license,
            },
            kotlin: KotlinSettings {
                version: kotlin_version,
                version_set: kotlin_version_set,
                language_version: self.kotlin.language_version,
                api_version: self.kotlin.api_version,
                ksp_version: self.kotlin.ksp,
                opt_in: self.kotlin.opt_in,
                compiler_args: self.kotlin.compiler_args,
                warnings_as_errors: self.kotlin.warnings_as_errors,
            },
            targets,
            bins,
            dependencies,
            dev_dependencies,
            path_dependencies,
            repositories,
            frameworks,
            compiler_plugins,
            process_plugins,
            tasks,
            dev_profile: self.profile.dev.unwrap_or_default().apply(Profile::dev()),
            release_profile: self.profile.release.unwrap_or_default().apply(Profile::release()),
        })
    }
}

/// Validates `[[bin]]`: unique names, and a `main`/`entry-point` or a `path`, not both.
fn validate_bins(raw: Vec<RawBin>) -> Result<Vec<Bin>> {
    let mut bins: Vec<Bin> = Vec::new();
    for b in raw {
        let name = b.name;
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid [[bin]] name `{name}` (start with a letter; use letters, digits, `-` and `_`)");
        }
        if bins.iter().any(|other| other.name == name) {
            bail!("[[bin]] `{name}` is declared more than once");
        }
        let is_class = |s: &str| !s.is_empty() && s.split('.').all(|seg| !seg.is_empty() && !seg.contains(char::is_whitespace));
        for (key, value) in [("main", &b.main), ("entry-point", &b.entry_point)] {
            if let Some(v) = value
                && !is_class(v)
            {
                bail!("[[bin]] `{name}`: invalid `{key}` `{v}` (expected a qualified name such as `com.example.tools.MigrateKt`)");
            }
        }
        match (&b.path, b.main.is_some() || b.entry_point.is_some()) {
            (Some(_), true) => bail!("[[bin]] `{name}`: set either `path` or `main`/`entry-point`, not both"),
            (None, false) => {
                bail!(
                    "[[bin]] `{name}` needs an entry point: set `path` to the file with its `main`, or `main` (JVM) and `entry-point` (Native)"
                )
            }
            _ => {}
        }
        if let Some(path) = &b.path
            && path.extension().is_none_or(|e| e != "kt")
        {
            bail!("[[bin]] `{name}`: `path` must be a Kotlin source file (`.kt`), got `{}`", path.display());
        }
        bins.push(Bin { name, main: b.main, entry_point: b.entry_point, path: b.path });
    }
    Ok(bins)
}

/// Fails unless `version` (a tool version from `[section] key`) is one exact version.
fn validate_tool_version(section: &str, key: &str, tool: &str, example: &str, version: Option<&str>) -> Result<()> {
    if let Some(v) = version
        && (v.is_empty() || v.contains(|c: char| c.is_whitespace() || "[](),".contains(c)))
    {
        bail!("[{section}] `{key}` must be an exact {tool} version such as \"{example}\", got `{v}`");
    }
    Ok(())
}

/// `kargo.toml` of a workspace: only a `[workspace]` table.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RawWorkspaceManifest {
    workspace: RawWorkspace,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default)]
    kotlin: RawWorkspaceKotlin,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWorkspaceKotlin {
    version: Option<String>,
}

impl RawWorkspaceManifest {
    pub(super) fn validate(self, root: &Path) -> Result<WorkspaceManifest> {
        let raw = self.workspace;
        if raw.members.is_empty() {
            bail!("[workspace] `members` is empty; list the member directories, e.g. members = [\"app\", \"lib\"]");
        }
        let mut members: Vec<PathBuf> = Vec::new();
        for member in &raw.members {
            if member.is_empty() || member.contains(['*', '?', '[']) {
                bail!("[workspace] `members` entries must be directories such as \"app\" or \"libs/core\", got `{member}`");
            }
            let path = PathBuf::from(member);
            if members.contains(&path) {
                bail!("[workspace] member `{member}` is listed more than once");
            }
            members.push(path);
        }
        if let Some(v) = &raw.kotlin.version {
            validate_version_string("[workspace.kotlin] version", v)?;
        }
        let workspace =
            WorkspaceManifest { root: root.to_path_buf(), members, kotlin_version: raw.kotlin.version, dependencies: raw.dependencies };
        // Validate the shared entries once, so a typo is reported even before a member uses it.
        for (name, value) in &workspace.dependencies {
            if value.get("workspace").is_some() {
                bail!("invalid dependency `{name}` in [workspace.dependencies]: `workspace` is only valid in a member's dependencies");
            }
            parse_dependency(name, value, "workspace.dependencies", None)?;
        }
        Ok(workspace)
    }
}

impl RawFmt {
    fn validate(self) -> Result<FmtSettings> {
        validate_tool_version("fmt", "ktlint", "ktlint", "1.8.0", self.ktlint.as_deref())?;
        for pattern in &self.exclude {
            if pattern.is_empty() || pattern.starts_with('!') || pattern.starts_with('/') || pattern.contains('\\') || pattern.contains(':')
            {
                bail!("[fmt] `exclude` entries must be globs relative to the project root such as \"src/generated/**\", got `{pattern}`");
            }
        }
        Ok(FmtSettings { ktlint_version: self.ktlint, exclude: self.exclude })
    }
}

impl RawLint {
    fn validate(self) -> Result<LintSettings> {
        validate_tool_version("lint", "detekt", "detekt", "1.23.8", self.detekt.as_deref())?;
        Ok(LintSettings { detekt_version: self.detekt, config: self.config, baseline: self.baseline })
    }
}

impl RawDoc {
    fn validate(self) -> Result<DocSettings> {
        validate_tool_version("doc", "dokka", "Dokka", "2.2.0", self.dokka.as_deref())?;
        let is_web = |url: &str| url.starts_with("https://") || url.starts_with("http://");
        if let Some(url) = &self.source_link
            && !is_web(url)
        {
            bail!("[doc] `source-link` must be an http(s):// URL, got `{url}`");
        }
        for url in &self.external_links {
            if !is_web(url) {
                bail!("[doc] `external-links` entries must be http(s):// URLs, got `{url}`");
            }
        }
        Ok(DocSettings {
            dokka_version: self.dokka,
            includes: self.includes,
            source_link: self.source_link.map(|u| u.trim_end_matches('/').to_owned()),
            external_links: self.external_links,
        })
    }
}
