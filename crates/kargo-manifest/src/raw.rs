//! Serde model of `kargo.toml` and its validation into [`super::Manifest`].

use std::collections::BTreeMap;
use std::path::PathBuf;

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
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawToolchain {
    jdk: Option<Value>,
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
    dependencies: toml::Table,
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
    #[serde(default)]
    args: Vec<String>,
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
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    hooks: Vec<String>,
}

fn parse_dependency(name: &str, value: &Value, section: &str) -> Result<Dependency> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    let detailed: DetailedDependency = match value {
        Value::String(version) => DetailedDependency { version: Some(version.clone()), ..Default::default() },
        Value::Table(_) => value.clone().try_into().with_context(ctx)?,
        _ => bail!("{}: expected a version string or a table", ctx()),
    };

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
        return finish_dependency(name, spec.module, version, detailed, section);
    } else if let Some(group) = &detailed.group {
        ModuleId::new(group, detailed.artifact.clone().unwrap_or_else(|| name.to_owned())).to_string().parse().with_context(ctx)?
    } else {
        bail!("{}: the key must be `group:artifact`, or the table must specify `coords` or `group`", ctx());
    };
    let version = detailed.version.clone();
    finish_dependency(name, module, version, detailed, section)
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

fn parse_dependencies(table: &toml::Table, section: &str) -> Result<Vec<Dependency>> {
    let deps = table.iter().map(|(name, value)| parse_dependency(name, value, section)).collect::<Result<Vec<_>>>()?;
    for (i, dep) in deps.iter().enumerate() {
        if deps[..i].iter().any(|d| d.module == dep.module && d.classifier == dep.classifier) {
            bail!("dependency `{}` is declared more than once in [{section}]", dep.module);
        }
    }
    Ok(deps)
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
    pub(super) fn validate(self) -> Result<Manifest> {
        validate_package_name(&self.package.name)?;
        validate_version_string("package version", &self.package.version)?;
        if let Some(group) = &self.package.group {
            if group.is_empty()
                || group.split('.').any(|seg| seg.is_empty() || !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            {
                bail!("invalid package group `{group}` (expected something like `com.example`)");
            }
        }

        let kotlin_version = self.kotlin.version.unwrap_or_else(|| DEFAULT_KOTLIN_VERSION.to_owned());
        validate_version_string("Kotlin version", &kotlin_version)?;

        let targets = match self.target {
            // No [target] section means a plain JVM project.
            None => Targets {
                jvm: Some(JvmTarget {
                    jvm_target: DEFAULT_JVM_TARGET.to_owned(),
                    main_class: None,
                    jvm_args: Vec::new(),
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
                                jvm_target: j.jvm_target.unwrap_or_else(|| DEFAULT_JVM_TARGET.to_owned()),
                                main_class: j.main_class,
                                jvm_args: j.jvm_args,
                                dependencies: parse_dependencies(&j.dependencies, "target.jvm.dependencies")?,
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
                                dependencies: parse_dependencies(&w.dependencies, "target.wasm.dependencies")?,
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
                            Ok(NativeTarget {
                                platforms,
                                kind,
                                entry_point: n.entry_point,
                                linker_options: n.linker_options,
                                dependencies: parse_dependencies(&n.dependencies, "target.native.dependencies")?,
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
                            Ok(AndroidTarget { config, dependencies: parse_dependencies(&dependencies, "target.android.dependencies")? })
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
        if let Some(ktor) = &frameworks.ktor {
            if ktor.server.is_none() && ktor.client.is_none() {
                bail!("[frameworks.ktor] enable at least one of `server = \"netty\"` or `client = \"cio\"`");
            }
        }

        let mut compiler_plugins = Vec::new();
        let mut process_plugins = Vec::new();
        for (name, value) in &self.plugins {
            let raw: RawPlugin = value.clone().try_into().with_context(|| format!("invalid plugin `{name}`"))?;
            match (&raw.maven, &raw.command) {
                (Some(maven), None) => {
                    if !raw.hooks.is_empty() || !raw.args.is_empty() || !raw.env.is_empty() {
                        bail!("plugin `{name}`: `hooks`, `args` and `env` only apply to `command` plugins");
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
                        args: raw.args,
                        hooks: parse_hooks(&raw.hooks, &format!("plugin `{name}`"))?,
                        env: raw.env,
                    });
                }
                (Some(_), Some(_)) => bail!("plugin `{name}`: specify either `maven` or `command`, not both"),
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
            let run = match raw.run {
                None => Vec::new(),
                Some(Value::String(s)) => vec![s],
                Some(Value::Array(items)) => items
                    .into_iter()
                    .map(|v| match v {
                        Value::String(s) => Ok(s),
                        _ => bail!("task `{name}`: `run` entries must be strings"),
                    })
                    .collect::<Result<_>>()?,
                Some(_) => bail!("task `{name}`: `run` must be a string or an array of strings"),
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
        if let (Some(major), Some(jvm)) = (jdk.major(), &targets.jvm) {
            let target: u32 = jvm.jvm_target.strip_prefix("1.").unwrap_or(&jvm.jvm_target).parse().unwrap_or(0);
            if target > major {
                bail!("[target.jvm] jvm-target {} is newer than the [toolchain] JDK {major}", jvm.jvm_target);
            }
        }

        Ok(Manifest {
            toolchain: ToolchainSettings { jdk },
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
                language_version: self.kotlin.language_version,
                api_version: self.kotlin.api_version,
                opt_in: self.kotlin.opt_in,
                compiler_args: self.kotlin.compiler_args,
                warnings_as_errors: self.kotlin.warnings_as_errors,
            },
            targets,
            dependencies: parse_dependencies(&self.dependencies, "dependencies")?,
            dev_dependencies: parse_dependencies(&self.dev_dependencies, "dev-dependencies")?,
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
