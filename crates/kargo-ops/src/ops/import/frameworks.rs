//! Recognizing the libraries and plugins that Kargo's built-in frameworks stand for. The
//! dependencies a framework adds are taken out of the dependency lists, so `[frameworks]` replaces
//! them instead of repeating them.

use super::model::{Dependency, Frameworks, Ktor, Scope, Serialization, SpringBoot};
use crate::builtins::{COMPOSE_VERSION, COROUTINES_VERSION, KTOR_VERSION, LOMBOK_VERSION, SERIALIZATION_VERSION};

/// What the build files say besides the dependencies.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// The Spring Boot plugin or parent was applied, with this version.
    pub spring_boot: Option<Option<String>>,
    /// The `jpa` / `plugin.jpa` Kotlin compiler plugin was applied.
    pub jpa: bool,
    pub serialization_plugin: bool,
    /// The Compose plugin was applied, with this version.
    pub compose: Option<Option<String>>,
    /// The Ktor Gradle plugin was applied, with this version.
    pub ktor: Option<Option<String>>,
    pub lombok: bool,
}

/// The result of [`detect`]: the frameworks, and a line for the report about each.
#[derive(Debug, Default)]
pub struct Detected {
    pub frameworks: Frameworks,
    pub translated: Vec<String>,
}

/// Finds the frameworks `dependencies` and `dev_dependencies` use, and removes the dependencies they
/// replace.
pub fn detect(dependencies: &mut Vec<Dependency>, dev_dependencies: &mut Vec<Dependency>, hints: &Hints) -> Detected {
    let mut out = Detected::default();
    spring_boot(dependencies, dev_dependencies, hints, &mut out);
    ktor(dependencies, dev_dependencies, hints, &mut out);
    serialization(dependencies, hints, &mut out);
    coroutines(dependencies, &mut out);
    compose(dependencies, hints, &mut out);
    lombok(dependencies, dev_dependencies, hints, &mut out);
    out
}

fn is(dep: &Dependency, group: &str, artifact: &str) -> bool {
    dep.group == group && dep.artifact == artifact
}

/// `ktor-server-core-jvm` is `ktor-server-core` on the JVM; Kargo's frameworks name the base module.
fn base_artifact(artifact: &str) -> &str {
    artifact.strip_suffix("-jvm").unwrap_or(artifact)
}

/// A dependency's version, or a version the BOMs and plugins of the build give the framework.
fn version_of(deps: &[Dependency], matches: impl Fn(&Dependency) -> bool) -> Option<&str> {
    deps.iter().filter(|d| matches(d)).find_map(|d| d.version.as_deref())
}

/// Takes the dependencies for which `matches` holds out of `deps`.
fn take(deps: &mut Vec<Dependency>, matches: impl Fn(&Dependency) -> bool) -> Vec<Dependency> {
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(deps).into_iter().partition(|d| matches(d));
    *deps = kept;
    taken
}

/// `None` when `version` is the default of the framework, so `kargo.toml` doesn't repeat it.
fn unless_default(version: Option<&str>, default: &str) -> Option<String> {
    version.filter(|v| *v != default).map(str::to_owned)
}

fn spring_boot(deps: &mut Vec<Dependency>, dev: &mut Vec<Dependency>, hints: &Hints, out: &mut Detected) {
    let boot = "org.springframework.boot";
    let is_starter = |d: &Dependency| d.group == boot && d.artifact.starts_with("spring-boot-starter");
    if hints.spring_boot.is_none() && !deps.iter().chain(dev.iter()).any(is_starter) {
        return;
    }
    let bom_version = deps.iter().find(|d| d.bom && is(d, boot, "spring-boot-dependencies")).and_then(|d| d.version.clone());
    let version = hints
        .spring_boot
        .clone()
        .flatten()
        .or(bom_version)
        .or_else(|| version_of(deps, is_starter).map(str::to_owned))
        .or_else(|| version_of(dev, is_starter).map(str::to_owned));
    let starters: Vec<Dependency> = take(deps, |d| is_starter(d) && d.artifact != "spring-boot-starter-test");
    take(dev, |d| is(d, boot, "spring-boot-starter-test"));
    take(deps, |d| d.bom && is(d, boot, "spring-boot-dependencies"));
    take(deps, |d| is(d, "org.jetbrains.kotlin", "kotlin-reflect"));
    // Jackson's Kotlin module comes with the web starters, from the group the Spring Boot version manages.
    let jackson = crate::builtins::spring_jackson_group(version.as_deref().unwrap_or(crate::builtins::SPRING_BOOT_VERSION));
    let serves_json =
        |d: &Dependency| matches!(d.artifact.strip_prefix("spring-boot-starter-"), Some("web" | "webmvc" | "webflux" | "json" | "jackson"));
    if starters.iter().any(serves_json) {
        take(deps, |d| is(d, jackson, "jackson-module-kotlin"));
    }

    let mut names: Vec<String> = Vec::new();
    for starter in &starters {
        match starter.artifact.strip_prefix("spring-boot-starter-") {
            Some(name) if !names.iter().any(|n| n == name) => names.push(name.to_owned()),
            _ => {}
        }
    }
    let jpa = hints.jpa && !names.iter().any(|n| n == "data-jpa");
    out.translated.push(format!(
        "Spring Boot{} -> [frameworks] spring-boot (starters: {})",
        version.as_deref().map(|v| format!(" {v}")).unwrap_or_default(),
        if names.is_empty() { "none".to_owned() } else { names.join(", ") }
    ));
    out.frameworks.spring_boot = Some(SpringBoot { version, starters: names, jpa });
}

const KTOR_SERVER_ENGINES: &[&str] = &["netty", "cio", "jetty", "jetty-jakarta", "tomcat", "tomcat-jakarta"];
const KTOR_CLIENT_ENGINES: &[&str] = &["cio", "okhttp", "java", "apache", "apache5", "curl", "darwin", "winhttp", "android", "js"];

fn ktor(deps: &mut Vec<Dependency>, dev: &mut Vec<Dependency>, hints: &Hints, out: &mut Detected) {
    let is_ktor = |d: &Dependency| d.group == "io.ktor" && !d.bom;
    let has_ktor = deps.iter().any(|d| is_ktor(d) && (d.artifact.starts_with("ktor-server-") || d.artifact.starts_with("ktor-client-")));
    if !has_ktor {
        return;
    }
    let bom_version = deps.iter().find(|d| d.bom && is(d, "io.ktor", "ktor-bom")).and_then(|d| d.version.clone());
    let version = hints.ktor.clone().flatten().or(bom_version).or_else(|| version_of(deps, is_ktor).map(str::to_owned));

    let names = |prefix: &str, deps: &[Dependency]| -> Vec<String> {
        deps.iter().filter(|d| is_ktor(d)).filter_map(|d| base_artifact(&d.artifact).strip_prefix(prefix).map(str::to_owned)).collect()
    };
    let server_names = names("ktor-server-", deps);
    let client_names = names("ktor-client-", deps);
    let server = KTOR_SERVER_ENGINES.iter().find(|e| server_names.iter().any(|n| n == *e)).map(|e| (*e).to_owned());
    let client = KTOR_CLIENT_ENGINES.iter().find(|e| client_names.iter().any(|n| n == *e)).map(|e| (*e).to_owned());
    if server.is_none() && client.is_none() {
        return;
    }
    let json = deps.iter().any(|d| is_ktor(d) && base_artifact(&d.artifact) == "ktor-serialization-kotlinx-json");

    let mut ktor = Ktor { version: unless_default(version.as_deref(), KTOR_VERSION), json, ..Ktor::default() };
    if let Some(engine) = &server {
        for name in &server_names {
            if name != "core" && name != "test-host" && name != engine && !ktor.server_plugins.contains(name) {
                ktor.server_plugins.push(name.clone());
            }
        }
        ktor.server = Some(engine.clone());
    }
    if let Some(engine) = &client {
        for name in &client_names {
            if name != "core" && name != engine && !ktor.client_plugins.contains(name) {
                ktor.client_plugins.push(name.clone());
            }
        }
        ktor.client = Some(engine.clone());
    }
    let server_engine = server.clone();
    let client_engine = client.clone();
    let replaced = move |d: &Dependency| {
        if !is_ktor(d) {
            return false;
        }
        let artifact = base_artifact(&d.artifact);
        let in_server = server_engine.is_some() && artifact.starts_with("ktor-server-");
        let in_client = client_engine.is_some() && artifact.starts_with("ktor-client-");
        in_server || in_client || (json && artifact == "ktor-serialization-kotlinx-json")
    };
    take(deps, replaced);
    take(deps, |d| d.bom && is(d, "io.ktor", "ktor-bom"));
    if server.is_some() {
        take(dev, |d| is_ktor(d) && base_artifact(&d.artifact) == "ktor-server-test-host");
    }
    out.translated.push(format!(
        "Ktor{} -> [frameworks] ktor (server: {}, client: {})",
        version.as_deref().map(|v| format!(" {v}")).unwrap_or_default(),
        server.as_deref().unwrap_or("none"),
        client.as_deref().unwrap_or("none")
    ));
    out.frameworks.ktor = Some(ktor);
}

fn serialization_format(artifact: &str) -> Option<&'static str> {
    let artifact = artifact.strip_prefix("kotlinx-serialization-")?;
    let artifact = artifact.strip_suffix("-jvm").unwrap_or(artifact);
    Some(match artifact {
        "json" => "json",
        "cbor" => "cbor",
        "protobuf" => "protobuf",
        "properties" => "properties",
        "hocon" => "hocon",
        "core" => "core",
        _ => return None,
    })
}

fn serialization(deps: &mut Vec<Dependency>, hints: &Hints, out: &mut Detected) {
    let is_library = |d: &Dependency| d.group == "org.jetbrains.kotlinx" && serialization_format(&d.artifact).is_some() && !d.bom;
    if !hints.serialization_plugin && !deps.iter().any(is_library) {
        return;
    }
    let version = version_of(deps, is_library).map(str::to_owned);
    let mut formats: Vec<String> = Vec::new();
    for dep in deps.iter().filter(|d| is_library(d)) {
        let format = serialization_format(&dep.artifact).expect("filtered above");
        if !formats.iter().any(|f| f == format) {
            formats.push(format.to_owned());
        }
    }
    take(deps, is_library);
    take(deps, |d| d.bom && is(d, "org.jetbrains.kotlinx", "kotlinx-serialization-bom"));
    if formats.is_empty() {
        formats.push("core".to_owned());
    }
    out.translated.push(format!("kotlinx.serialization ({}) -> [frameworks] serialization", formats.join(", ")));
    out.frameworks.serialization = Some(Serialization { version: unless_default(version.as_deref(), SERIALIZATION_VERSION), formats });
}

fn coroutines(deps: &mut Vec<Dependency>, out: &mut Detected) {
    let is_core = |d: &Dependency| d.group == "org.jetbrains.kotlinx" && base_artifact(&d.artifact) == "kotlinx-coroutines-core" && !d.bom;
    if !deps.iter().any(is_core) {
        return;
    }
    let version = version_of(deps, is_core).map(str::to_owned).or_else(|| {
        deps.iter().find(|d| d.bom && is(d, "org.jetbrains.kotlinx", "kotlinx-coroutines-bom")).and_then(|d| d.version.clone())
    });
    take(deps, is_core);
    take(deps, |d| d.bom && is(d, "org.jetbrains.kotlinx", "kotlinx-coroutines-bom"));
    out.translated.push("kotlinx.coroutines -> [frameworks] coroutines".to_owned());
    out.frameworks.coroutines = Some(unless_default(version.as_deref(), COROUTINES_VERSION));
}

fn compose(deps: &mut Vec<Dependency>, hints: &Hints, out: &mut Detected) {
    let Some(version) = &hints.compose else { return };
    take(deps, |d| d.group == "org.jetbrains.compose.desktop" || is(d, "org.jetbrains.compose.material3", "material3"));
    out.translated.push("Compose Multiplatform -> [frameworks] compose".to_owned());
    out.frameworks.compose = Some(unless_default(version.as_deref(), COMPOSE_VERSION));
}

fn lombok(deps: &mut Vec<Dependency>, dev: &mut Vec<Dependency>, hints: &Hints, out: &mut Detected) {
    let is_lombok = |d: &Dependency| is(d, "org.projectlombok", "lombok");
    if !hints.lombok && !deps.iter().chain(dev.iter()).any(is_lombok) {
        return;
    }
    let version = version_of(deps, is_lombok).or_else(|| version_of(dev, is_lombok)).map(str::to_owned);
    let processors = deps.iter().filter(|d| is_lombok(d) && d.scope == Scope::Processor).count();
    take(deps, is_lombok);
    take(dev, is_lombok);
    let _ = processors;
    out.translated.push("Lombok -> [frameworks] lombok".to_owned());
    out.frameworks.lombok = Some(unless_default(version.as_deref(), LOMBOK_VERSION));
}

/// Dependencies Kargo adds by itself: the Kotlin standard library, `kotlin-test` and JUnit.
pub fn is_built_in(dep: &Dependency) -> bool {
    let kotlin = dep.group == "org.jetbrains.kotlin"
        && (dep.artifact == "kotlin-stdlib" || dep.artifact.starts_with("kotlin-stdlib-") || dep.artifact.starts_with("kotlin-test"));
    let junit =
        dep.group == "org.junit.jupiter" && matches!(dep.artifact.as_str(), "junit-jupiter" | "junit-jupiter-api" | "junit-jupiter-engine");
    (kotlin || junit) && !dep.bom && dep.classifier.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(group: &str, artifact: &str, version: Option<&str>) -> Dependency {
        Dependency::new(group, artifact, version.map(str::to_owned))
    }

    #[test]
    fn spring_boot_replaces_its_starters_and_the_libraries_it_adds() {
        let mut deps = vec![
            dep("org.springframework.boot", "spring-boot-starter-web", None),
            dep("org.springframework.boot", "spring-boot-starter-data-jpa", None),
            dep("org.jetbrains.kotlin", "kotlin-reflect", None),
            dep("org.postgresql", "postgresql", Some("42.7.3")),
        ];
        let mut dev = vec![dep("org.springframework.boot", "spring-boot-starter-test", None)];
        let hints = Hints { spring_boot: Some(Some("3.3.1".into())), ..Hints::default() };
        let found = detect(&mut deps, &mut dev, &hints);
        let spring = found.frameworks.spring_boot.unwrap();
        assert_eq!(spring.version.as_deref(), Some("3.3.1"));
        assert_eq!(spring.starters, ["web", "data-jpa"]);
        assert!(!spring.jpa, "data-jpa already implies it");
        assert_eq!(deps.len(), 1);
        assert!(dev.is_empty());
    }

    #[test]
    fn ktor_artifacts_become_engines_and_plugins() {
        let mut deps = vec![
            dep("io.ktor", "ktor-server-core-jvm", None),
            dep("io.ktor", "ktor-server-netty-jvm", None),
            dep("io.ktor", "ktor-server-content-negotiation-jvm", None),
            dep("io.ktor", "ktor-serialization-kotlinx-json-jvm", None),
            dep("io.ktor", "ktor-server-config-yaml", None),
            dep("ch.qos.logback", "logback-classic", Some("1.4.14")),
        ];
        let mut dev = vec![dep("io.ktor", "ktor-server-test-host-jvm", None)];
        let hints = Hints { ktor: Some(Some("3.0.1".into())), ..Hints::default() };
        let found = detect(&mut deps, &mut dev, &hints);
        let ktor = found.frameworks.ktor.unwrap();
        assert_eq!(ktor.version.as_deref(), Some("3.0.1"));
        assert_eq!(ktor.server.as_deref(), Some("netty"));
        assert_eq!(ktor.server_plugins, ["content-negotiation", "config-yaml"]);
        assert!(ktor.json);
        assert_eq!(deps.len(), 1, "logback stays: {deps:?}");
        assert!(dev.is_empty());
    }

    #[test]
    fn serialization_formats_follow_the_libraries() {
        let mut deps = vec![
            dep("org.jetbrains.kotlinx", "kotlinx-serialization-json", Some("1.6.3")),
            dep("org.jetbrains.kotlinx", "kotlinx-serialization-protobuf-jvm", Some("1.6.3")),
        ];
        let found = detect(&mut deps, &mut Vec::new(), &Hints::default());
        let ser = found.frameworks.serialization.unwrap();
        assert_eq!(ser.formats, ["json", "protobuf"]);
        assert_eq!(ser.version.as_deref(), Some("1.6.3"));
        assert!(deps.is_empty());
    }

    #[test]
    fn built_in_libraries_are_recognized() {
        assert!(is_built_in(&dep("org.jetbrains.kotlin", "kotlin-stdlib-jdk8", Some("1.9.0"))));
        assert!(is_built_in(&dep("org.jetbrains.kotlin", "kotlin-test-junit5", None)));
        assert!(!is_built_in(&dep("org.jetbrains.kotlin", "kotlin-reflect", None)));
    }
}
