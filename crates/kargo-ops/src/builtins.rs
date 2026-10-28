//! Built-in framework integrations: each enabled framework contributes
//! dependencies, compiler plugins and compiler arguments for a target.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};

use kargo_manifest::{ComposeConfig, Dependency, JsEnvironment, Manifest, Scope, TargetKind, WasmEnvironment};
use kargo_maven::ModuleId;

pub const COROUTINES_VERSION: &str = "1.11.0";
/// The browser APIs of Kotlin/Wasm (`kotlinx.browser`, `org.w3c.dom`), which the standard library
/// doesn't contain. Added for `environment = "browser"` of Wasm and JS.
pub const KOTLINX_BROWSER_VERSION: &str = "0.5.0";
pub const SERIALIZATION_VERSION: &str = "1.11.0";
pub const COMPOSE_VERSION: &str = "1.12.1";
pub const COMPOSE_MATERIAL3_VERSION: &str = "1.9.0";
pub const KTOR_VERSION: &str = "3.6.0";
/// The value of `[frameworks.ktor] client-engines` for a target that needs `ktor-client-core` alone.
const NO_KTOR_ENGINE: &str = "none";
pub const LOGBACK_VERSION: &str = "1.5.32";
pub const LOMBOK_VERSION: &str = "1.18.48";
pub const SPRING_BOOT_VERSION: &str = "4.1.1";
pub const JUNIT_VERSION: &str = "6.1.3";
/// kotlinx-benchmark's annotations and runtime for `kargo bench` (JVM), unless `[frameworks] benchmark` pins another.
pub const KOTLINX_BENCHMARK_VERSION: &str = "0.5.0";
/// The JMH that runs the benchmarks of the JVM target. kotlinx-benchmark's annotations are aliases of
/// JMH's, and its jar does not bring JMH along.
pub const JMH_VERSION: &str = "1.37";
/// The oldest JDK that runs JUnit 6 and its console launcher, which are compiled for Java 17.
pub const JUNIT_MIN_JDK: u32 = 17;
/// KSP2 (`com.google.devtools.ksp`), used for `scope = "ksp"` dependencies on the JVM and Android
/// unless `[kotlin] ksp` overrides it.
pub const KSP_VERSION: &str = "2.3.12";

/// The KSP version to use: `[kotlin] ksp`, or [`KSP_VERSION`].
pub fn ksp_version(manifest: &Manifest) -> &str {
    manifest.kotlin.ksp_version.as_deref().unwrap_or(KSP_VERSION)
}

/// A Kotlin compiler plugin shipped with the compiler (`org.jetbrains.kotlin:<artifact>:<kotlin version>`)
/// or fetched from Maven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    pub group: String,
    pub artifact: String,
    /// `None` means "same as the Kotlin compiler".
    pub version: Option<String>,
    pub id: Option<String>,
    pub options: Vec<(String, String)>,
}

impl PluginSpec {
    fn kotlin(artifact: &str, id: &str) -> Self {
        Self { group: "org.jetbrains.kotlin".into(), artifact: artifact.into(), version: None, id: Some(id.into()), options: Vec::new() }
    }

    fn option(mut self, key: &str, value: impl Into<String>) -> Self {
        self.options.push((key.into(), value.into()));
        self
    }
}

/// Everything frameworks and the manifest's `[plugins]` add to a build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contributions {
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    pub plugins: Vec<PluginSpec>,
    pub compiler_args: Vec<String>,
    /// Extra `javac` arguments (e.g. `-parameters`).
    pub javac_args: Vec<String>,
    /// Java annotation processors (e.g. Lombok) for mixed Java sources.
    pub annotation_processors: Vec<Dependency>,
}

/// A framework library. It is part of the API of a package that uses the framework (`exported`),
/// as the framework's annotations and types show up in its declarations.
fn dep(coord: &str, version: Option<&str>) -> Dependency {
    let module: ModuleId = coord.parse().expect("built-in coordinates are valid");
    Dependency { exported: true, ..Dependency::new(module, version.map(str::to_owned)) }
}

fn dep_scoped(coord: &str, version: Option<&str>, scope: Scope) -> Dependency {
    let mut d = dep(coord, version);
    d.scope = scope;
    d
}

/// The libraries the benchmarks of the JVM target compile and run against: kotlinx-benchmark's
/// annotations (`@Benchmark`, `@State`, `Blackhole`) and the JMH they stand for.
pub fn benchmark_dependencies(manifest: &Manifest) -> Vec<Dependency> {
    let version = manifest.frameworks.benchmark.as_ref().and_then(|b| b.version.as_deref()).unwrap_or(KOTLINX_BENCHMARK_VERSION);
    vec![dep("org.jetbrains.kotlinx:kotlinx-benchmark-runtime", Some(version)), dep("org.openjdk.jmh:jmh-core", Some(JMH_VERSION))]
}

/// The compiler plugin that opens the `@State` classes of benchmarks: JMH generates subclasses of them,
/// and Kotlin classes are final.
pub fn benchmark_plugin() -> PluginSpec {
    PluginSpec::kotlin("kotlin-allopen-compiler-plugin-embeddable", "org.jetbrains.kotlin.allopen")
        .option("annotation", "org.openjdk.jmh.annotations.State")
}

/// The `os-arch` suffix of Compose Desktop artifacts for this machine.
pub fn compose_desktop_classifier() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("macos", "aarch64") => "macos-arm64",
        (os, arch) => bail!("Compose Desktop does not support {os}/{arch}"),
    })
}

/// The id of the Compose compiler plugin, the prefix of its `-P` options.
pub const COMPOSE_PLUGIN_ID: &str = "androidx.compose.compiler.plugins.kotlin";

/// The Compose compiler options of `[frameworks.compose]` for `target`: its stability configuration
/// files (relative to `project_dir`), feature flags, and metrics and reports under `out_dir/<target>`.
pub fn compose_compiler_options(compose: &ComposeConfig, project_dir: &Path, out_dir: &Path, target: TargetKind) -> Vec<(String, String)> {
    let mut options = Vec::new();
    for file in &compose.stability_config {
        options.push(("stabilityConfigurationPath".to_owned(), kargo_util::process::slash(&project_dir.join(file))));
    }
    if let Some(strong) = compose.strong_skipping {
        options.push(("featureFlag".to_owned(), if strong { "StrongSkipping" } else { "-StrongSkipping" }.to_owned()));
    }
    for flag in &compose.feature_flags {
        options.push(("featureFlag".to_owned(), flag.clone()));
    }
    for (enabled, key, dir) in [(compose.metrics, "metricsDestination", "metrics"), (compose.reports, "reportsDestination", "reports")] {
        if enabled.unwrap_or(false) {
            options.push((key.to_owned(), kargo_util::process::slash(&out_dir.join(target.as_str()).join(dir))));
        }
    }
    options
}

/// Fails unless every stability configuration file of `[frameworks.compose]` exists. Checked before
/// compiling rather than in [`compose_compiler_options`], so `kargo metadata` works without them.
pub fn check_compose_stability_config(manifest: &Manifest, project_dir: &Path) -> Result<()> {
    for file in manifest.frameworks.compose.iter().flat_map(|c| &c.stability_config) {
        let path = project_dir.join(file);
        if !path.is_file() {
            bail!(
                "the Compose stability configuration file `{}` of [frameworks.compose] `stability-config` does not exist; create it or remove it from the list",
                path.display()
            );
        }
    }
    Ok(())
}

/// The Kotlin/Native platforms Compose Multiplatform builds iOS apps for.
pub const COMPOSE_IOS_PLATFORMS: [&str; 3] = ["ios_arm64", "ios_simulator_arm64", "ios_x64"];

/// The Compose libraries of the Wasm and iOS targets, which resolve to their platform's variant:
/// the runtime, foundation and UI, Material 3 unless disabled, and the resources runtime of `Res`.
fn multiplatform_compose(c: &mut Contributions, compose: &ComposeConfig, version: &str, project_dir: &Path) {
    for module in ["org.jetbrains.compose.runtime:runtime", "org.jetbrains.compose.foundation:foundation", "org.jetbrains.compose.ui:ui"] {
        c.dependencies.push(dep(module, Some(version)));
    }
    if compose.material3.unwrap_or(true) {
        c.dependencies.push(dep("org.jetbrains.compose.material3:material3", Some(COMPOSE_MATERIAL3_VERSION)));
    }
    if crate::compose::resources::present(project_dir) {
        c.dependencies.push(dep(crate::compose::resources::RUNTIME, Some(version)));
    }
}

/// The group of Jackson's Kotlin module in the Spring Boot BOM of version `spring`: Spring Boot 4
/// moved to Jackson 3 (`tools.jackson`), earlier versions manage the `com.fasterxml.jackson` one.
pub fn spring_jackson_group(spring: &str) -> &'static str {
    match spring_major(spring) {
        Some(major) if major < 4 => "com.fasterxml.jackson.module",
        _ => "tools.jackson.module",
    }
}

/// The major version of a Spring Boot version, when it starts with one.
fn spring_major(spring: &str) -> Option<u32> {
    spring.split('.').next().and_then(|major| major.parse().ok())
}

/// Starters that Spring Boot renamed: `(old, new, first major version with the new name)`, without
/// the `spring-boot-starter-` prefix. The old name stops resolving from that version on, and the new
/// one doesn't exist before it.
const RENAMED_STARTERS: &[(&str, &str, u32)] = &[("aop", "aspectj", 4)];

/// The starter that replaces `starter` (without its prefix) in Spring Boot `spring`, when `starter`
/// was renamed and doesn't exist in that version.
fn renamed_starter(starter: &str, spring: &str) -> Option<&'static str> {
    let major = spring_major(spring)?;
    RENAMED_STARTERS.iter().find_map(|&(old, new, since)| match (major >= since, starter) {
        (true, s) if s == old => Some(new),
        (false, s) if s == new => Some(old),
        _ => None,
    })
}

/// Fails with the replacement when a starter of `[frameworks] spring-boot` or a
/// `org.springframework.boot:spring-boot-starter-*` dependency was renamed and doesn't exist in
/// Spring Boot `spring`, instead of leaving it to fail as a missing artifact on download.
fn check_spring_starters(manifest: &Manifest, starters: &[String], spring: &str) -> Result<()> {
    for starter in starters {
        if let Some(replacement) = renamed_starter(starter, spring) {
            bail!(
                "the Spring Boot starter `{starter}` of `[frameworks] spring-boot` does not exist in Spring Boot {spring}; \
                 it is called `{replacement}` there (`starters = [\"{replacement}\"]`)"
            );
        }
    }
    let declared =
        manifest.dependencies.iter().chain(&manifest.dev_dependencies).chain(manifest.targets.jvm.iter().flat_map(|t| &t.dependencies));
    for dependency in declared {
        let module = &dependency.module;
        let Some(starter) = module.artifact.strip_prefix("spring-boot-starter-").filter(|_| module.group == "org.springframework.boot")
        else {
            continue;
        };
        if let Some(replacement) = renamed_starter(starter, spring) {
            bail!(
                "`{module}` does not exist in Spring Boot {spring}; the starter is called `org.springframework.boot:spring-boot-starter-{replacement}` there"
            );
        }
    }
    Ok(())
}

/// Computes contributions for `target`. Android is handled by the Gradle
/// generator, which maps frameworks to Gradle plugins instead.
pub fn contributions(manifest: &Manifest, target: TargetKind, project_dir: &Path) -> Result<Contributions> {
    let mut c = Contributions::default();
    let kotlin = manifest.kotlin.version.as_str();
    let fw = &manifest.frameworks;

    // Standard library and test framework.
    match target {
        TargetKind::Jvm => {
            c.dependencies.push(dep("org.jetbrains.kotlin:kotlin-stdlib", Some(kotlin)));
            c.dev_dependencies.push(dep("org.jetbrains.kotlin:kotlin-test-junit5", Some(kotlin)));
            c.dev_dependencies.push(dep("org.junit.jupiter:junit-jupiter", Some(JUNIT_VERSION)));
        }
        TargetKind::Wasm => {
            let env = manifest.targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
            // Node.js and browsers share the `wasm-js` libraries.
            let suffix = format!("wasm-{}", env.platform());
            c.dependencies.push(dep(&format!("org.jetbrains.kotlin:kotlin-stdlib-{suffix}"), Some(kotlin)));
            c.dev_dependencies.push(dep(&format!("org.jetbrains.kotlin:kotlin-test-{suffix}"), Some(kotlin)));
            // A page's DOM and `window` are a separate library, so the build and the IDE both need it.
            if env == WasmEnvironment::Browser {
                c.dependencies.push(dep(&format!("org.jetbrains.kotlinx:kotlinx-browser-{suffix}"), Some(KOTLINX_BROWSER_VERSION)));
            }
        }
        TargetKind::Js => {
            c.dependencies.push(dep("org.jetbrains.kotlin:kotlin-stdlib-js", Some(kotlin)));
            c.dev_dependencies.push(dep("org.jetbrains.kotlin:kotlin-test-js", Some(kotlin)));
            // As on Wasm, `document` and `window` (`kotlinx.browser`) are no longer in the standard library.
            if manifest.targets.js.as_ref().is_some_and(|j| j.environment == JsEnvironment::Browser) {
                c.dependencies.push(dep("org.jetbrains.kotlinx:kotlinx-browser", Some(KOTLINX_BROWSER_VERSION)));
            }
        }
        TargetKind::Native | TargetKind::Android => {}
    }

    if let Some(co) = &fw.coroutines {
        let v = co.version.as_deref().unwrap_or(COROUTINES_VERSION);
        c.dependencies.push(dep("org.jetbrains.kotlinx:kotlinx-coroutines-core", Some(v)));
        if target == TargetKind::Jvm {
            c.dev_dependencies.push(dep("org.jetbrains.kotlinx:kotlinx-coroutines-test", Some(v)));
        }
    }

    if let Some(ser) = &fw.serialization {
        c.plugins.push(PluginSpec::kotlin("kotlin-serialization-compiler-plugin-embeddable", "org.jetbrains.kotlinx.serialization"));
        let v = ser.version.as_deref().unwrap_or(SERIALIZATION_VERSION);
        let formats = ser.formats.clone().unwrap_or_else(|| vec!["json".to_owned()]);
        for format in formats {
            let artifact = match format.as_str() {
                "json" => "kotlinx-serialization-json",
                "protobuf" => "kotlinx-serialization-protobuf",
                "cbor" => "kotlinx-serialization-cbor",
                "properties" => "kotlinx-serialization-properties",
                "hocon" if target == TargetKind::Jvm => "kotlinx-serialization-hocon",
                "hocon" => continue,
                "core" => "kotlinx-serialization-core",
                other => bail!("unknown serialization format `{other}` (expected json, protobuf, cbor, properties, hocon or core)"),
            };
            c.dependencies.push(dep(&format!("org.jetbrains.kotlinx:{artifact}"), Some(v)));
        }
    }

    if let Some(compose) = &fw.compose {
        c.plugins.push(PluginSpec::kotlin("kotlin-compose-compiler-plugin-embeddable", COMPOSE_PLUGIN_ID));
        let v = compose.version.as_deref().unwrap_or(COMPOSE_VERSION);
        match target {
            TargetKind::Jvm => {
                c.dependencies.push(dep(&format!("org.jetbrains.compose.desktop:desktop-jvm-{}", compose_desktop_classifier()?), Some(v)));
                if compose.material3.unwrap_or(true) {
                    c.dependencies.push(dep("org.jetbrains.compose.material3:material3", Some(COMPOSE_MATERIAL3_VERSION)));
                }
                if fw.coroutines.is_some() {
                    let cv = fw.coroutines.as_ref().and_then(|co| co.version.as_deref()).unwrap_or(COROUTINES_VERSION);
                    c.dependencies.push(dep("org.jetbrains.kotlinx:kotlinx-coroutines-swing", Some(cv)));
                }
                // The runtime of the generated `Res` class.
                if crate::compose::resources::present(project_dir) {
                    c.dependencies.push(dep(crate::compose::resources::RUNTIME, Some(v)));
                }
            }
            TargetKind::Wasm => {
                let env = manifest.targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
                if env != WasmEnvironment::Browser {
                    bail!(
                        "Compose for Web draws on a browser canvas; set `environment = \"browser\"` in [target.wasm] (it is `{}`)",
                        env.as_str()
                    );
                }
                multiplatform_compose(&mut c, compose, v, project_dir);
            }
            TargetKind::Js => {
                bail!("Compose for Web runs on Kotlin/Wasm; build it with [target.wasm] environment = \"browser\" instead of [target.js]")
            }
            TargetKind::Native => {
                let native = manifest.targets.native.as_ref();
                let platforms = native.map(|n| n.platforms.clone()).unwrap_or_default();
                if let Some(other) = platforms.iter().find(|p| !COMPOSE_IOS_PLATFORMS.contains(&p.as_str())) {
                    bail!(
                        "Compose on Kotlin/Native builds iOS apps, so [target.native] `platforms` can only list {}; `{other}` is not one",
                        COMPOSE_IOS_PLATFORMS.join(", ")
                    );
                }
                if native.is_some_and(|n| n.kind != kargo_manifest::NativeKind::Framework) {
                    bail!("Compose for iOS is built as a framework for an Xcode project; set `kind = \"framework\"` in [target.native]");
                }
                multiplatform_compose(&mut c, compose, v, project_dir);
            }
            TargetKind::Android => {}
        }
    }

    if let Some(ktor) = &fw.ktor {
        let v = ktor.version.as_deref().unwrap_or(KTOR_VERSION);
        if let Some(engine) = &ktor.server {
            if target != TargetKind::Jvm && target != TargetKind::Native {
                bail!("the Ktor server is only available on the JVM and Kotlin/Native targets");
            }
            if target == TargetKind::Native && engine != "cio" {
                bail!("Kotlin/Native only supports the `cio` Ktor server engine, not `{engine}`");
            }
            c.dependencies.push(dep("io.ktor:ktor-server-core", Some(v)));
            c.dependencies.push(dep(&format!("io.ktor:ktor-server-{engine}"), Some(v)));
            for plugin in &ktor.server_plugins {
                c.dependencies.push(dep(&format!("io.ktor:ktor-server-{plugin}"), Some(v)));
            }
            if target == TargetKind::Jvm {
                c.dependencies.push(dep_scoped("ch.qos.logback:logback-classic", Some(LOGBACK_VERSION), Scope::Runtime));
                c.dev_dependencies.push(dep("io.ktor:ktor-server-test-host", Some(v)));
            }
        }
        if let Some(default_engine) = &ktor.client {
            c.dependencies.push(dep("io.ktor:ktor-client-core", Some(v)));
            let engine = ktor.client_engines.get(target.as_str()).unwrap_or(default_engine);
            if engine != NO_KTOR_ENGINE {
                c.dependencies.push(dep(&format!("io.ktor:ktor-client-{engine}"), Some(v)));
            }
            for plugin in &ktor.client_plugins {
                c.dependencies.push(dep(&format!("io.ktor:ktor-client-{plugin}"), Some(v)));
            }
        }
        if ktor.json.unwrap_or(fw.serialization.is_some()) {
            c.dependencies.push(dep("io.ktor:ktor-serialization-kotlinx-json", Some(v)));
            if ktor.server.is_some() && !ktor.server_plugins.iter().any(|p| p == "content-negotiation") {
                c.dependencies.push(dep("io.ktor:ktor-server-content-negotiation", Some(v)));
            }
            if ktor.client.is_some() && !ktor.client_plugins.iter().any(|p| p == "content-negotiation") {
                c.dependencies.push(dep("io.ktor:ktor-client-content-negotiation", Some(v)));
            }
        }
    }

    if let Some(lombok) = &fw.lombok {
        if target != TargetKind::Jvm && target != TargetKind::Android {
            bail!("Lombok is a JVM technology and cannot be used with the {target} target");
        }
        let v = lombok.version.as_deref().unwrap_or(LOMBOK_VERSION);
        let lombok_dep = dep_scoped("org.projectlombok:lombok", Some(v), Scope::Provided);
        c.dependencies.push(lombok_dep.clone());
        c.annotation_processors.push(lombok_dep);
        let mut plugin = PluginSpec::kotlin("kotlin-lombok-compiler-plugin-embeddable", "org.jetbrains.kotlin.lombok");
        let config = project_dir.join(lombok.config.as_deref().unwrap_or("lombok.config"));
        if config.is_file() {
            plugin = plugin.option("config", config.to_string_lossy());
        } else if lombok.config.is_some() {
            bail!("Lombok config file {} does not exist", config.display());
        }
        c.plugins.push(plugin);
    }

    // The benchmarks of the JVM target (`benches/`) compile against these, like tests do.
    if fw.benchmark.is_some() && target == TargetKind::Jvm {
        c.dev_dependencies.extend(benchmark_dependencies(manifest));
    }

    if let Some(spring) = &fw.spring_boot {
        if target != TargetKind::Jvm {
            bail!("Spring Boot is only supported on the JVM target");
        }
        let v = spring.version.as_deref().unwrap_or(SPRING_BOOT_VERSION);
        check_spring_starters(manifest, spring.starters.as_deref().unwrap_or(&[]), v)?;
        let mut bom = dep("org.springframework.boot:spring-boot-dependencies", Some(v));
        bom.bom = true;
        c.dependencies.push(bom);
        let starters = spring.starters.clone().unwrap_or_else(|| vec!["web".to_owned()]);
        for starter in &starters {
            let artifact = if starter.is_empty() { "spring-boot-starter".to_owned() } else { format!("spring-boot-starter-{starter}") };
            c.dependencies.push(dep(&format!("org.springframework.boot:{artifact}"), None));
        }
        if !starters.iter().any(String::is_empty) {
            c.dependencies.push(dep("org.springframework.boot:spring-boot-starter", None));
        }
        c.dependencies.push(dep("org.jetbrains.kotlin:kotlin-reflect", Some(kotlin)));
        if starters.iter().any(|s| matches!(s.as_str(), "web" | "webmvc" | "webflux" | "json" | "jackson")) {
            c.dependencies.push(dep(&format!("{}:jackson-module-kotlin", spring_jackson_group(v)), None));
        }
        c.dev_dependencies.push(dep("org.springframework.boot:spring-boot-starter-test", None));
        c.plugins.push(
            PluginSpec::kotlin("kotlin-allopen-compiler-plugin-embeddable", "org.jetbrains.kotlin.allopen").option("preset", "spring"),
        );
        if spring.jpa.unwrap_or_else(|| starters.iter().any(|s| s == "data-jpa")) {
            c.plugins
                .push(PluginSpec::kotlin("kotlin-noarg-compiler-plugin-embeddable", "org.jetbrains.kotlin.noarg").option("preset", "jpa"));
            c.plugins.push(
                PluginSpec::kotlin("kotlin-allopen-compiler-plugin-embeddable", "org.jetbrains.kotlin.allopen")
                    .option("annotation", "jakarta.persistence.Entity")
                    .option("annotation", "jakarta.persistence.MappedSuperclass")
                    .option("annotation", "jakarta.persistence.Embeddable"),
            );
        }
        c.compiler_args.push("-Xjsr305=strict".into());
        c.compiler_args.push("-java-parameters".into());
        c.javac_args.push("-parameters".into());
    }

    // User compiler plugins from [plugins].
    for plugin in &manifest.compiler_plugins {
        let mut options: Vec<(String, String)> = plugin.options.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        options.sort();
        c.plugins.push(PluginSpec {
            group: plugin.artifact.module.group.clone(),
            artifact: plugin.artifact.module.artifact.clone(),
            version: plugin.artifact.version.clone(),
            id: plugin.id.clone(),
            options,
        });
    }

    merge_plugins(&mut c.plugins);
    Ok(c)
}

/// Merges duplicate plugin entries (e.g. two allopen configurations) into one.
fn merge_plugins(plugins: &mut Vec<PluginSpec>) {
    let mut merged: Vec<PluginSpec> = Vec::new();
    for plugin in plugins.drain(..) {
        match merged.iter_mut().find(|p| p.group == plugin.group && p.artifact == plugin.artifact && p.version == plugin.version) {
            Some(existing) => {
                for opt in plugin.options {
                    if !existing.options.contains(&opt) {
                        existing.options.push(opt);
                    }
                }
            }
            None => merged.push(plugin),
        }
    }
    *plugins = merged;
}

/// Names of the built-in frameworks, for `kargo add --framework` and `kargo new --with`.
pub const FRAMEWORK_NAMES: &[&str] = &["compose", "serialization", "coroutines", "ktor", "lombok", "spring-boot", "benchmark"];

/// The names of the frameworks `[frameworks]` enables, in [`FRAMEWORK_NAMES`] order.
pub fn enabled_frameworks(manifest: &Manifest) -> Vec<&'static str> {
    let fw = &manifest.frameworks;
    let enabled = [
        fw.compose.is_some(),
        fw.serialization.is_some(),
        fw.coroutines.is_some(),
        fw.ktor.is_some(),
        fw.lombok.is_some(),
        fw.spring_boot.is_some(),
        fw.benchmark.is_some(),
    ];
    FRAMEWORK_NAMES.iter().zip(enabled).filter_map(|(name, on)| on.then_some(*name)).collect()
}

/// Summary of plugin options as `-P` arguments.
pub fn plugin_option_args(plugin: &PluginSpec) -> Vec<String> {
    let Some(id) = &plugin.id else { return Vec::new() };
    plugin.options.iter().map(|(k, v)| format!("plugin:{id}:{k}={v}")).collect()
}

/// Groups options by key, for display.
pub fn describe_options(plugin: &PluginSpec) -> BTreeMap<&str, Vec<&str>> {
    let mut map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (k, v) in &plugin.options {
        map.entry(k.as_str()).or_default().push(v.as_str());
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Manifest {
        Manifest::parse(&format!("[package]\nname = \"t\"\nversion = \"1\"\n{extra}")).unwrap()
    }

    fn coords(deps: &[Dependency]) -> Vec<String> {
        deps.iter().map(|d| d.module.to_string()).collect()
    }

    #[test]
    fn plain_jvm_project_gets_stdlib_and_junit() {
        let c = contributions(&manifest(""), TargetKind::Jvm, Path::new(".")).unwrap();
        assert_eq!(coords(&c.dependencies), ["org.jetbrains.kotlin:kotlin-stdlib"]);
        assert!(coords(&c.dev_dependencies).contains(&"org.jetbrains.kotlin:kotlin-test-junit5".to_owned()));
        assert!(c.plugins.is_empty());
    }

    #[test]
    fn wasm_stdlib_follows_environment() {
        let m = manifest("[target.wasm]\nenvironment = \"wasi\"\n");
        let c = contributions(&m, TargetKind::Wasm, Path::new(".")).unwrap();
        assert_eq!(coords(&c.dependencies), ["org.jetbrains.kotlin:kotlin-stdlib-wasm-wasi"]);
    }

    #[test]
    fn browser_uses_the_wasm_js_libraries() {
        let m = manifest("[target.wasm]\nenvironment = \"browser\"\n");
        let c = contributions(&m, TargetKind::Wasm, Path::new(".")).unwrap();
        assert_eq!(
            coords(&c.dependencies),
            ["org.jetbrains.kotlin:kotlin-stdlib-wasm-js", "org.jetbrains.kotlinx:kotlinx-browser-wasm-js"]
        );
        assert_eq!(coords(&c.dev_dependencies), ["org.jetbrains.kotlin:kotlin-test-wasm-js"]);
    }

    #[test]
    fn only_browsers_get_the_browser_apis() {
        let js = manifest("[target.wasm]\nenvironment = \"js\"\n");
        let c = contributions(&js, TargetKind::Wasm, Path::new(".")).unwrap();
        assert_eq!(coords(&c.dependencies), ["org.jetbrains.kotlin:kotlin-stdlib-wasm-js"]);
    }

    #[test]
    fn js_gets_its_standard_library_and_frameworks() {
        let m = manifest("[target.js]\n[frameworks]\ncoroutines = true\nserialization = true\nktor = { client = \"js\" }\n");
        let c = contributions(&m, TargetKind::Js, Path::new(".")).unwrap();
        let deps = coords(&c.dependencies);
        assert_eq!(deps[0], "org.jetbrains.kotlin:kotlin-stdlib-js");
        for expected in
            ["org.jetbrains.kotlinx:kotlinx-coroutines-core", "org.jetbrains.kotlinx:kotlinx-serialization-json", "io.ktor:ktor-client-js"]
        {
            assert!(deps.contains(&expected.to_owned()), "missing {expected}: {deps:?}");
        }
        assert_eq!(coords(&c.dev_dependencies), ["org.jetbrains.kotlin:kotlin-test-js"]);
        assert_eq!(c.plugins[0].artifact, "kotlin-serialization-compiler-plugin-embeddable");
        let compose = manifest("[target.js]\n[frameworks]\ncompose = true\n");
        let err = contributions(&compose, TargetKind::Js, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("[target.wasm]"), "{err}");
        let server = manifest("[target.js]\n[frameworks]\nktor = { server = \"cio\" }\n");
        assert!(contributions(&server, TargetKind::Js, Path::new(".")).is_err(), "the Ktor server does not run on Kotlin/JS");
    }

    #[test]
    fn serialization_adds_plugin_and_formats() {
        let m = manifest("[frameworks]\nserialization = { formats = [\"json\", \"cbor\"] }\n");
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        assert_eq!(c.plugins[0].artifact, "kotlin-serialization-compiler-plugin-embeddable");
        assert!(coords(&c.dependencies).contains(&"org.jetbrains.kotlinx:kotlinx-serialization-cbor".to_owned()));
        let bad = manifest("[frameworks]\nserialization = { formats = [\"xml\"] }\n");
        assert!(contributions(&bad, TargetKind::Jvm, Path::new(".")).is_err());
    }

    #[test]
    fn compose_for_web_and_ios() {
        let web = manifest("[target.wasm]\nenvironment = \"browser\"\n[frameworks]\ncompose = { material3 = false }\n");
        let c = contributions(&web, TargetKind::Wasm, Path::new(".")).unwrap();
        let deps = coords(&c.dependencies);
        for module in
            ["org.jetbrains.compose.runtime:runtime", "org.jetbrains.compose.foundation:foundation", "org.jetbrains.compose.ui:ui"]
        {
            assert!(deps.contains(&module.to_owned()), "{deps:?}");
        }
        assert!(!deps.iter().any(|d| d.contains("material3")), "material3 = false");
        assert_eq!(c.plugins[0].id.as_deref(), Some(COMPOSE_PLUGIN_ID));
        let node = manifest("[target.wasm]\nenvironment = \"js\"\n[frameworks]\ncompose = true\n");
        let err = contributions(&node, TargetKind::Wasm, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("environment = \"browser\""), "{err}");

        let ios = manifest(
            "[target.native]\nplatforms = [\"ios_arm64\", \"ios_simulator_arm64\"]\nkind = \"framework\"\n[frameworks]\ncompose = true\n",
        );
        let deps = coords(&contributions(&ios, TargetKind::Native, Path::new(".")).unwrap().dependencies);
        assert!(deps.contains(&"org.jetbrains.compose.material3:material3".to_owned()), "{deps:?}");
        let linux = manifest("[target.native]\nplatforms = [\"linux_x64\"]\n[frameworks]\ncompose = true\n");
        let err = contributions(&linux, TargetKind::Native, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("`linux_x64` is not one"), "{err}");
        let executable = manifest("[target.native]\nplatforms = [\"ios_arm64\"]\n[frameworks]\ncompose = true\n");
        let err = contributions(&executable, TargetKind::Native, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("kind = \"framework\""), "{err}");
    }

    #[test]
    fn compose_compiler_options_from_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("target/compose");
        let m = manifest("[frameworks]\ncompose = true\n");
        let compose = m.frameworks.compose.as_ref().unwrap();
        assert!(compose_compiler_options(compose, dir.path(), &out, TargetKind::Jvm).is_empty(), "none by default");

        std::fs::write(dir.path().join("stability.conf"), "java.time.*\n").unwrap();
        let m = manifest(
            "[frameworks]\ncompose = { stability-config = \"stability.conf\", strong-skipping = false, feature-flags = [\"OptimizeNonSkippingGroups\"], metrics = true, reports = true }\n",
        );
        let options = compose_compiler_options(m.frameworks.compose.as_ref().unwrap(), dir.path(), &out, TargetKind::Jvm);
        check_compose_stability_config(&m, dir.path()).unwrap();
        let slash = |p: &Path| kargo_util::process::slash(p);
        assert_eq!(
            options,
            [
                ("stabilityConfigurationPath".to_owned(), slash(&dir.path().join("stability.conf"))),
                ("featureFlag".to_owned(), "-StrongSkipping".to_owned()),
                ("featureFlag".to_owned(), "OptimizeNonSkippingGroups".to_owned()),
                ("metricsDestination".to_owned(), slash(&out.join("jvm/metrics"))),
                ("reportsDestination".to_owned(), slash(&out.join("jvm/reports"))),
            ]
        );

        let missing = manifest("[frameworks]\ncompose = { stability-config = [\"stability.conf\", \"missing.conf\"] }\n");
        let err = check_compose_stability_config(&missing, dir.path()).unwrap_err();
        assert!(err.to_string().contains("missing.conf"), "{err}");
    }

    #[test]
    fn compose_desktop() {
        let m = manifest("[frameworks]\ncompose = true\ncoroutines = true\n");
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        let deps = coords(&c.dependencies);
        assert!(deps.iter().any(|d| d.starts_with("org.jetbrains.compose.desktop:desktop-jvm-")));
        assert!(deps.contains(&"org.jetbrains.compose.material3:material3".to_owned()));
        assert!(deps.contains(&"org.jetbrains.kotlinx:kotlinx-coroutines-swing".to_owned()));
        assert_eq!(c.plugins[0].id.as_deref(), Some("androidx.compose.compiler.plugins.kotlin"));
        assert!(contributions(&m, TargetKind::Wasm, Path::new(".")).is_err());
        assert!(!deps.iter().any(|d| d.contains("components-resources")), "no composeResources/, no resources runtime");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("composeResources/drawable")).unwrap();
        let c = contributions(&m, TargetKind::Jvm, dir.path()).unwrap();
        let resources = c.dependencies.iter().find(|d| d.module.artifact == "components-resources").unwrap();
        assert_eq!(resources.version.as_deref(), Some(COMPOSE_VERSION));
    }

    #[test]
    fn ktor_server_and_client() {
        let m =
            manifest("[frameworks]\nserialization = true\nktor = { server = \"netty\", client = \"cio\", server-plugins = [\"cors\"] }\n");
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        let deps = coords(&c.dependencies);
        for expected in [
            "io.ktor:ktor-server-core",
            "io.ktor:ktor-server-netty",
            "io.ktor:ktor-server-cors",
            "io.ktor:ktor-client-cio",
            "io.ktor:ktor-serialization-kotlinx-json",
            "io.ktor:ktor-server-content-negotiation",
            "io.ktor:ktor-client-content-negotiation",
            "ch.qos.logback:logback-classic",
        ] {
            assert!(deps.contains(&expected.to_owned()), "missing {expected}");
        }
        assert!(contributions(&m, TargetKind::Native, Path::new(".")).is_err(), "netty is JVM-only");
    }

    #[test]
    fn ktor_client_engines_follow_the_target() {
        let m = manifest(
            "[frameworks]\nktor = { client = \"cio\", client-engines = { native = \"darwin\", wasm = \"none\", android = \"okhttp\" } }\n",
        );
        let client_modules = |target| {
            let c = contributions(&m, target, Path::new(".")).unwrap();
            coords(&c.dependencies).into_iter().filter(|d| d.starts_with("io.ktor:")).collect::<Vec<_>>()
        };
        assert_eq!(client_modules(TargetKind::Jvm), ["io.ktor:ktor-client-core", "io.ktor:ktor-client-cio"]);
        assert_eq!(client_modules(TargetKind::Native), ["io.ktor:ktor-client-core", "io.ktor:ktor-client-darwin"]);
        assert_eq!(client_modules(TargetKind::Android), ["io.ktor:ktor-client-core", "io.ktor:ktor-client-okhttp"]);
        assert_eq!(client_modules(TargetKind::Wasm), ["io.ktor:ktor-client-core"]);
    }

    #[test]
    fn lombok_is_provided_and_configures_plugin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lombok.config"), "config.stopBubbling = true").unwrap();
        let m = manifest("[frameworks]\nlombok = true\n");
        let c = contributions(&m, TargetKind::Jvm, dir.path()).unwrap();
        let lombok = c.dependencies.iter().find(|d| d.module.artifact == "lombok").unwrap();
        assert_eq!(lombok.scope, Scope::Provided);
        assert_eq!(c.annotation_processors.len(), 1);
        assert_eq!(c.plugins[0].options[0].0, "config");
        assert!(contributions(&m, TargetKind::Wasm, dir.path()).is_err());

        let missing = manifest("[frameworks]\nlombok = { config = \"nope.config\" }\n");
        assert!(contributions(&missing, TargetKind::Jvm, dir.path()).is_err());
    }

    #[test]
    fn benchmark_adds_the_annotations_for_benches_only() {
        let m = manifest(
            "[frameworks]
benchmark = true
",
        );
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        assert_eq!(c.dependencies.len(), 1, "the benchmarks are not part of the program: just the standard library");
        assert_eq!(
            coords(&c.dev_dependencies)[2..],
            ["org.jetbrains.kotlinx:kotlinx-benchmark-runtime", "org.openjdk.jmh:jmh-core"],
            "after the test framework"
        );
        assert!(contributions(&m, TargetKind::Native, Path::new(".")).unwrap().dev_dependencies.is_empty());
        let pinned = manifest(
            "[frameworks]
benchmark = { version = \"0.4.14\" }
",
        );
        let deps = benchmark_dependencies(&pinned);
        assert_eq!(deps[0].version.as_deref(), Some("0.4.14"));
        assert_eq!(deps[1].version.as_deref(), Some(JMH_VERSION));
        assert_eq!(
            plugin_option_args(&benchmark_plugin()),
            ["plugin:org.jetbrains.kotlin.allopen:annotation=org.openjdk.jmh.annotations.State"]
        );
    }

    #[test]
    fn spring_boot_uses_bom_and_presets() {
        let m = manifest("[frameworks]\nspring-boot = { starters = [\"web\", \"data-jpa\"] }\n");
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        assert!(c.dependencies.iter().any(|d| d.bom && d.module.artifact == "spring-boot-dependencies"));
        let starter = c.dependencies.iter().find(|d| d.module.artifact == "spring-boot-starter-web").unwrap();
        assert!(starter.version.is_none(), "versions come from the BOM");
        assert!(c.compiler_args.contains(&"-Xjsr305=strict".to_owned()));
        let allopen: Vec<_> = c.plugins.iter().filter(|p| p.artifact.contains("allopen")).collect();
        assert_eq!(allopen.len(), 1, "allopen configurations are merged");
        assert!(allopen[0].options.contains(&("preset".into(), "spring".into())));
        assert!(allopen[0].options.contains(&("annotation".into(), "jakarta.persistence.Entity".into())));
        assert!(c.plugins.iter().any(|p| p.artifact.contains("noarg")));
        assert_eq!(plugin_option_args(allopen[0])[0], "plugin:org.jetbrains.kotlin.allopen:preset=spring");
        assert_eq!(describe_options(allopen[0])["annotation"].len(), 3);
    }

    #[test]
    fn spring_boot_3_manages_the_fasterxml_jackson_module() {
        let jackson = |version: &str| {
            let m = manifest(&format!(
                "[frameworks]
spring-boot = {{ version = \"{version}\" }}
"
            ));
            let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
            c.dependencies.iter().find(|d| d.module.artifact == "jackson-module-kotlin").map(|d| d.module.group.clone()).unwrap()
        };
        assert_eq!(jackson("3.3.1"), "com.fasterxml.jackson.module");
        assert_eq!(jackson("4.0.0"), "tools.jackson.module");
        assert_eq!(jackson(SPRING_BOOT_VERSION), "tools.jackson.module");
    }

    #[test]
    fn spring_boot_renamed_starters_name_the_replacement() {
        let fail = |toml: &str| contributions(&manifest(toml), TargetKind::Jvm, Path::new(".")).unwrap_err().to_string();
        let message = fail("[frameworks]\nspring-boot = { starters = [\"web\", \"aop\"] }\n");
        assert!(message.contains("`aop`") && message.contains("`aspectj`"), "{message}");

        let message = fail("[frameworks]\nspring-boot = true\n[dependencies]\n\"org.springframework.boot:spring-boot-starter-aop\" = {}\n");
        assert!(message.contains("spring-boot-starter-aspectj"), "{message}");

        let message = fail("[frameworks]\nspring-boot = { version = \"3.5.0\", starters = [\"aspectj\"] }\n");
        assert!(message.contains("`aspectj`") && message.contains("`aop`"), "{message}");

        let ok = manifest("[frameworks]\nspring-boot = { starters = [\"web\", \"aspectj\"] }\n");
        assert!(contributions(&ok, TargetKind::Jvm, Path::new(".")).is_ok());
        let ok = manifest("[frameworks]\nspring-boot = { version = \"3.5.0\", starters = [\"aop\"] }\n");
        assert!(contributions(&ok, TargetKind::Jvm, Path::new(".")).is_ok());
    }

    #[test]
    fn user_compiler_plugins() {
        let m = manifest(
            "[plugins.pa]\nmaven = \"org.jetbrains.kotlin:kotlin-power-assert-compiler-plugin-embeddable:2.4.21\"\nid = \"org.jetbrains.kotlin.powerassert\"\noptions = { function = \"kotlin.assert\" }\n",
        );
        let c = contributions(&m, TargetKind::Jvm, Path::new(".")).unwrap();
        assert_eq!(c.plugins.len(), 1);
        assert_eq!(c.plugins[0].version.as_deref(), Some("2.4.21"));
        assert_eq!(plugin_option_args(&c.plugins[0]), ["plugin:org.jetbrains.kotlin.powerassert:function=kotlin.assert"]);
    }
}
