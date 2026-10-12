use super::*;

fn parse(text: &str) -> Manifest {
    Manifest::parse(text).unwrap_or_else(|e| panic!("{e:#}"))
}

fn parse_err(text: &str) -> String {
    format!("{:#}", Manifest::parse(text).expect_err("manifest should be rejected"))
}

const MINIMAL: &str = r#"
[package]
name = "hello"
version = "0.1.0"
"#;

#[test]
fn minimal_manifest_defaults_to_jvm() {
    let m = parse(MINIMAL);
    assert_eq!(m.package.name, "hello");
    assert_eq!(m.kotlin.version, DEFAULT_KOTLIN_VERSION);
    assert_eq!(m.kotlin.ksp_version, None);
    assert_eq!(m.targets.enabled(), [TargetKind::Jvm]);
    assert_eq!(m.targets.jvm.as_ref().unwrap().jvm_target, DEFAULT_JVM_TARGET);
    assert_eq!(m.repositories.len(), 1);
    assert_eq!(m.repositories[0].url, kargo_maven::repo::MAVEN_CENTRAL);
    assert!(m.dev_profile.debug && !m.dev_profile.optimize);
    assert!(!m.release_profile.debug && m.release_profile.optimize);
}

#[test]
fn full_manifest() {
    let m = parse(
        r#"
[package]
name = "full-app"
version = "1.2.3"
group = "com.example"
description = "demo"
authors = ["Ada"]
license = "MIT"

[kotlin]
version = "2.4.0"
language-version = "2.4"
ksp = "2.3.10"
opt-in = ["kotlin.ExperimentalStdlibApi"]
compiler-args = ["-Xcontext-parameters"]
warnings-as-errors = true

[target.jvm]
jvm-target = "21"
main-class = "com.example.AppKt"
jvm-args = ["-Xmx1g"]
javac-args = ["-parameters", "-Xlint:all"]

[target.jvm.dependencies]
"ch.qos.logback:logback-classic" = { version = "1.5.0", scope = "runtime" }
"com.google.auto.value:auto-value" = { version = "1.11.0", scope = "processor" }
"com.squareup.moshi:moshi-kotlin-codegen" = { version = "1.15.2", scope = "ksp" }

[target.wasm]
environment = "wasi"
package-manager = "pnpm"

[target.native]
platforms = ["linuxX64", "mingw_x64", "host"]
kind = "shared"
entry-point = "com.example.main"

[target.android]
namespace = "com.example.app"
min-sdk = 26

[dependencies]
"org.jetbrains.kotlinx:kotlinx-coroutines-core" = "1.11.0"
datetime = { coords = "org.jetbrains.kotlinx:kotlinx-datetime:0.7.0" }
guava = { group = "com.google.guava", version = "33.0.0-jre", exclude = ["com.google.code.findbugs:jsr305", "org.checkerframework:*"] }
"org.springframework.boot:spring-boot-dependencies" = { version = "4.1.1", bom = true }
natives = { coords = "org.lwjgl:lwjgl", version = "3.3.6", classifier = "natives-windows", force = true, transitive = false }

[dev-dependencies]
"org.jetbrains.kotlin:kotlin-test" = "2.4.0"

[repositories]
jitpack = "https://jitpack.io"
private = { url = "https://repo.example.com/maven", username-env = "REPO_USER_UNSET_X", password-env = "REPO_PASS_UNSET_X" }

[frameworks]
compose = { version = "1.12.1", material3 = false }
serialization = { formats = ["json", "cbor"] }
coroutines = "1.11.0"
ktor = { server = "netty", client-plugins = ["logging"], client = "cio" }
lombok = true
spring-boot = { starters = ["web", "data-jpa"] }

[plugins.power-assert]
maven = "org.jetbrains.kotlin:kotlin-power-assert-compiler-plugin-embeddable:2.4.0"
id = "org.jetbrains.kotlin.powerassert"
options = { function = "kotlin.assert" }

[plugins.notify]
command = "notify-send"
args = ["built"]
windows-command = "notify.cmd"
hooks = ["post-build", "post-test"]

[tasks]
fmt = "ktlint -F"
gen = { run = ["echo a", "echo b"], description = "generate", env = { A = "1" }, hooks = ["pre-build"] }
all = { depends-on = ["fmt", "gen"] }
envs = { run = "echo $KARGO_PKG_NAME", windows-run = ["echo %KARGO_PKG_NAME%"] }
posix = { run = "mkdir -p build", shell = "sh" }

[profile.release]
compiler-args = ["-Xno-call-assertions"]

[profile.dev]
optimize = true
"#,
    );

    assert_eq!(m.package.group.as_deref(), Some("com.example"));
    assert_eq!(m.kotlin.version, "2.4.0");
    assert_eq!(m.kotlin.ksp_version.as_deref(), Some("2.3.10"));
    assert!(m.kotlin.warnings_as_errors);
    assert_eq!(m.targets.enabled(), [TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native, TargetKind::Android]);

    let jvm = m.targets.jvm.as_ref().unwrap();
    assert_eq!(jvm.jvm_target, "21");
    assert_eq!(jvm.main_class.as_deref(), Some("com.example.AppKt"));
    assert_eq!(jvm.dependencies[0].scope, Scope::Runtime);
    assert_eq!(jvm.dependencies[1].scope, Scope::Processor);
    assert_eq!(jvm.dependencies[2].scope, Scope::Ksp);
    assert_eq!(jvm.javac_args, ["-parameters", "-Xlint:all"]);

    assert_eq!(m.targets.wasm.as_ref().unwrap().environment, WasmEnvironment::Wasi);
    assert_eq!(m.targets.wasm.as_ref().unwrap().package_manager, Some(PackageManager::Pnpm));
    let native = m.targets.native.as_ref().unwrap();
    assert_eq!(native.platforms, ["linux_x64", "mingw_x64", "host"]);
    assert_eq!(native.kind, NativeKind::Shared);
    let android = m.targets.android.as_ref().unwrap();
    assert_eq!(android.config.min_sdk, 26);
    assert_eq!(android.config.compile_sdk, 37);
    assert_eq!(android.config.kind, "application");

    assert_eq!(m.dependencies.len(), 5);
    let datetime = m.dependencies.iter().find(|d| d.name == "datetime").unwrap();
    assert_eq!(datetime.module.to_string(), "org.jetbrains.kotlinx:kotlinx-datetime");
    assert_eq!(datetime.version.as_deref(), Some("0.7.0"));
    let guava = m.dependencies.iter().find(|d| d.name == "guava").unwrap();
    assert_eq!(guava.module.to_string(), "com.google.guava:guava");
    assert_eq!(guava.exclusions.len(), 2);
    assert_eq!(guava.exclusions[1].artifact, "*");
    assert!(m.dependencies.iter().any(|d| d.bom));
    let natives = m.dependencies.iter().find(|d| d.name == "natives").unwrap();
    assert!(natives.force && !natives.transitive);
    assert_eq!(natives.classifier.as_deref(), Some("natives-windows"));

    // Maven Central, Google (android enabled), then user repositories in order.
    let names: Vec<_> = m.repositories.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["maven-central", "google", "jitpack", "private"]);
    assert!(m.repositories[3].credentials.is_none(), "unset env vars yield no credentials");

    let compose = m.frameworks.compose.as_ref().unwrap();
    assert_eq!(compose.material3, Some(false));
    assert_eq!(m.frameworks.serialization.as_ref().unwrap().formats.as_ref().unwrap(), &["json", "cbor"]);
    assert_eq!(m.frameworks.coroutines.as_ref().unwrap().version.as_deref(), Some("1.11.0"));
    assert_eq!(m.frameworks.ktor.as_ref().unwrap().server.as_deref(), Some("netty"));
    assert!(m.frameworks.lombok.is_some());
    assert_eq!(m.frameworks.spring_boot.as_ref().unwrap().starters.as_ref().unwrap().len(), 2);

    assert_eq!(m.compiler_plugins.len(), 1);
    assert_eq!(m.compiler_plugins[0].options["function"], "kotlin.assert");
    let notify = &m.process_plugins[0];
    assert_eq!(notify.hooks, [Hook::PostBuild, Hook::PostTest]);
    assert_eq!(notify.windows_command.as_deref(), Some("notify.cmd"));
    assert_eq!(notify.platform_command(), if cfg!(windows) { "notify.cmd" } else { "notify-send" });
    assert_eq!(notify.platform_args(), ["built"], "`args` is used when `windows-args` is not set");

    assert_eq!(m.tasks.len(), 5);
    assert_eq!(m.tasks["posix"].shell, Some(TaskShell::Sh));
    assert_eq!(m.tasks["fmt"].shell, None);
    assert_eq!(m.tasks["fmt"].platform_run(), ["ktlint -F"], "`run` is used when `windows-run` is not set");
    let envs = &m.tasks["envs"];
    assert_eq!(envs.platform_run(), if cfg!(windows) { ["echo %KARGO_PKG_NAME%"] } else { ["echo $KARGO_PKG_NAME"] });
    assert_eq!(m.tasks["gen"].run, ["echo a", "echo b"]);
    assert_eq!(m.tasks["gen"].hooks, [Hook::PreBuild]);
    assert_eq!(m.tasks["all"].depends_on, ["fmt", "gen"]);

    assert_eq!(m.release_profile.compiler_args, ["-Xno-call-assertions"]);
    assert!(m.dev_profile.optimize && m.dev_profile.debug);

    let jvm_deps = m.dependencies_for(TargetKind::Jvm);
    assert_eq!(jvm_deps.len(), 8);
    assert_eq!(m.dependencies_for(TargetKind::Wasm).len(), 5);
}

#[test]
fn rejects_invalid_package_names() {
    for name in ["", "1abc", "has space", "dots.not.allowed", "ünï"] {
        let text = format!("[package]\nname = \"{name}\"\nversion = \"1\"\n");
        assert!(Manifest::parse(&text).is_err(), "{name:?}");
    }
    assert!(validate_package_name("good-name_2").is_ok());
}

#[test]
fn rejects_unknown_fields() {
    assert!(parse_err("[package]\nname = \"a\"\nversion = \"1\"\nfoo = 1\n").contains("foo"));
    assert!(parse_err("[package]\nname = \"a\"\nversion = \"1\"\n[bogus]\n").contains("bogus"));
    assert!(parse_err("[package]\nname = \"a\"\nversion = \"1\"\n[target.android]\nnamespace = \"x\"\nmin-skd = 1\n").contains("min-skd"));
}

#[test]
fn dependency_errors() {
    let with = |deps: &str| format!("{MINIMAL}\n[dependencies]\n{deps}\n");
    assert!(parse_err(&with("noversion = \"1.0\"")).contains("group:artifact"));
    assert!(parse_err(&with("\"a:b\" = 5")).contains("expected a version string"));
    assert!(parse_err(&with("\"a:b\" = { version = \"1\", scope = \"weird\" }")).contains("scope"));
    assert!(parse_err(&with("\"a:b\" = { version = \"1\", coords = \"x:y\" }")).contains("not allowed"));
    assert!(parse_err(&with("x = { coords = \"a:b:1\", version = \"2\" }")).contains("both"));
    assert!(parse_err(&with("\"a:b\" = { bom = true }")).contains("BOM needs a version"));
    assert!(parse_err(&with("\"a:b\" = \"[1.0\"")).contains("version"));
    assert!(parse_err(&with("\"a:b\" = { version = \"1\", exclude = [\"bad\"] }")).contains("exclusion"));
    assert!(parse_err(&with("\"a:b\" = \"1\"\nx = { coords = \"a:b\", version = \"2\" }")).contains("more than once"));
    assert!(parse_err(&with("\"a:b\" = { version = \"1\", unknown = 1 }")).contains("unknown"));
    // A dependency without version is allowed (BOM-managed).
    assert!(parse(&with("\"a:b\" = {}")).dependencies[0].version.is_none());
}

#[test]
fn native_cinterop() {
    let m = parse(&format!(
        "{MINIMAL}
[target.native.cinterop.sqlite]
package = \"sqlite3\"
compiler-options = [\"-I/usr/include\"]
linker-options = [\"-lsqlite3\"]

[target.native.cinterop.curl]
def = \"c/curl.def\"
headers = [\"curl/curl.h\"]
"
    ));
    let native = m.targets.native.as_ref().unwrap();
    let names: Vec<_> = native.cinterop.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["curl", "sqlite"]);
    let curl = &native.cinterop[0];
    assert_eq!(curl.def_path(), Path::new("c/curl.def"));
    assert_eq!(curl.headers, ["curl/curl.h"]);
    assert!(curl.package.is_none());
    let sqlite = &native.cinterop[1];
    assert_eq!(sqlite.def_path(), Path::new("interop").join("sqlite.def"));
    assert_eq!(sqlite.package.as_deref(), Some("sqlite3"));
    assert_eq!(sqlite.compiler_options, ["-I/usr/include"]);
    assert_eq!(sqlite.linker_options, ["-lsqlite3"]);
}

#[test]
fn target_errors() {
    let with = |t: &str| format!("{MINIMAL}\n{t}\n");
    assert!(parse_err(&with("[target]")).contains("no target is enabled"));
    assert!(parse_err(&with("[target.wasm]\nenvironment = \"node\"")).contains("wasm environment"));
    assert!(parse_err(&with("[target.wasm]\npackage-manager = \"yarn\"")).contains("expected `npm`, `pnpm` or `bun`"));
    assert!(parse_err(&with("[target.native]\nplatforms = [\"plan9\"]")).contains("Kotlin/Native platform"));
    assert!(parse_err(&with("[target.native]\nkind = \"weird\"")).contains("native kind"));
    assert!(parse_err(&with("[target.native.cinterop.\"a/b\"]")).contains("invalid cinterop name"));
    assert!(parse_err(&with("[target.native.cinterop.x]\npackage = \"a..b\"")).contains("invalid package"));
    assert!(parse_err(&with("[target.native.cinterop.x]\nlibs = []")).contains("unknown field"));
    assert!(parse_err(&with("[target.android]\nnamespace = \"\"")).contains("namespace"));
    assert!(parse_err(&with("[target.android]\nnamespace = \"a\"\nmin-sdk = 40")).contains("min-sdk"));
    assert!(parse_err(&with("[target.android]\nnamespace = \"a\"\nkind = \"game\"")).contains("kind"));
    assert!(parse_err(&with("[target.android]\nmin-sdk = 21")).contains("namespace"));
    let native = parse(&with("[target.native]"));
    assert_eq!(native.targets.native.unwrap().platforms, ["host"]);
    assert!(native.targets.jvm.is_none(), "explicit targets replace the JVM default");
}

#[test]
fn konan_target_normalization() {
    assert_eq!(raw::normalize_konan_target("macosArm64").unwrap(), "macos_arm64");
    assert_eq!(raw::normalize_konan_target("iosSimulatorArm64").unwrap(), "ios_simulator_arm64");
    assert_eq!(raw::normalize_konan_target("androidNativeArm64").unwrap(), "android_arm64");
    assert_eq!(raw::normalize_konan_target("linux-x64").unwrap(), "linux_x64");
    assert!(raw::normalize_konan_target("windows").is_err());
}

#[test]
fn repository_errors() {
    let with = |r: &str| format!("{MINIMAL}\n[repositories]\n{r}\n");
    assert!(parse_err(&with("x = \"ftp://nope\"")).contains("http"));
    assert!(parse_err(&with("x = { url = \"https://a\", username-env = \"U\" }")).contains("both"));
    assert!(parse_err(&with("x = 3")).contains("expected a URL"));
    // Duplicate of Maven Central is ignored.
    let m = parse(&with(&format!("central = \"{}/\"", kargo_maven::repo::MAVEN_CENTRAL)));
    assert_eq!(m.repositories.len(), 1);
}

#[test]
fn framework_toggles() {
    let with = |f: &str| format!("{MINIMAL}\n[frameworks]\n{f}\n");
    let off = parse(&with("compose = false\nserialization = false\ncoroutines = false"));
    assert_eq!(off.frameworks, Frameworks::default());
    let compose = parse(&with("compose = true"));
    assert_eq!(compose.repositories[1].url, kargo_maven::repo::GOOGLE, "compose needs Google Maven");
    let on = parse(&with("coroutines = true"));
    assert_eq!(on.frameworks.coroutines.unwrap().version, None);
    assert!(parse_err(&with("coroutines = 3")).contains("coroutines"));
    assert!(parse_err(&with("ktor = true")).contains("server"));
    assert!(Manifest::parse(&with("compose = { colour = 1 }")).is_err());
}

#[test]
fn plugin_errors() {
    let with = |p: &str| format!("{MINIMAL}\n{p}\n");
    assert!(parse_err(&with("[plugins.x]\nid = \"y\"")).contains("specify"));
    assert!(parse_err(&with("[plugins.x]\nmaven = \"a:b:1\"\ncommand = \"c\"")).contains("not both"));
    assert!(parse_err(&with("[plugins.x]\nmaven = \"a:b\"")).contains("version"));
    assert!(parse_err(&with("[plugins.x]\nmaven = \"a:b:1\"\noptions = { k = \"v\" }")).contains("id"));
    assert!(parse_err(&with("[plugins.x]\nmaven = \"a:b:1\"\nhooks = [\"post-build\"]")).contains("command"));
    assert!(parse_err(&with("[plugins.x]\ncommand = \"c\"")).contains("hook"));
    assert!(parse_err(&with("[plugins.x]\ncommand = \"c\"\nhooks = [\"whenever\"]")).contains("unknown hook"));
    assert!(parse_err(&with("[plugins.x]\ncommand = \"c\"\nhooks = [\"pre-build\"]\nid = \"z\"")).contains("only apply"));
}

#[test]
fn task_errors() {
    let with = |t: &str| format!("{MINIMAL}\n[tasks]\n{t}\n");
    assert!(parse_err(&with("a = { depends-on = [\"missing\"] }")).contains("unknown task"));
    assert!(parse_err(&with("a = {}")).contains("nothing to do"));
    assert!(parse_err(&with("a = 1")).contains("command string"));
    assert!(parse_err(&with("a = { run = [1] }")).contains("strings"));
    assert!(parse_err(&with("\"a b\" = \"echo\"")).contains("task name"));
}

#[test]
fn version_validation() {
    assert!(parse_err("[package]\nname = \"a\"\nversion = \"\"\n").contains("version"));
    assert!(parse_err("[package]\nname = \"a\"\nversion = \"1\"\ngroup = \"com..x\"\n").contains("group"));
    assert!(parse_err(&format!("{MINIMAL}\n[kotlin]\nversion = \"(\"\n")).contains("Kotlin version"));
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[kotlin]
ksp = \"(\"
"
        ))
        .contains("KSP version")
    );
}

#[test]
fn load_reports_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MANIFEST_NAME);
    let err = format!("{:#}", Manifest::load(&path).unwrap_err());
    assert!(err.contains("failed to read"), "{err}");
    std::fs::write(&path, "not = [valid").unwrap();
    let err = format!("{:#}", Manifest::load(&path).unwrap_err());
    assert!(err.contains("failed to parse manifest"), "{err}");
    std::fs::write(&path, MINIMAL).unwrap();
    assert_eq!(Manifest::load(&path).unwrap().package.name, "hello");
}

#[test]
fn parses_enums() {
    assert_eq!("wasm-js".parse::<TargetKind>().unwrap(), TargetKind::Wasm);
    assert!("ios".parse::<TargetKind>().is_err());
    assert_eq!("compile-only".parse::<Scope>().unwrap(), Scope::Provided);
    assert_eq!("post-package".parse::<Hook>().unwrap(), Hook::PostPackage);
    assert_eq!(TargetKind::Native.source_dir_name(), "src-native");
}

#[test]
fn toolchain_jdk_requests() {
    assert_eq!(parse(MINIMAL).toolchain.jdk, JdkRequest::System);
    let with = |t: &str| {
        format!(
            "{MINIMAL}
[toolchain]
{t}
"
        )
    };
    assert_eq!(parse(&with("jdk = \"21\"")).toolchain.jdk, JdkRequest::Major(21));
    assert_eq!(parse(&with("jdk = 17")).toolchain.jdk, JdkRequest::Major(17));
    assert_eq!(parse(&with("jdk = \"system\"")).toolchain.jdk, JdkRequest::System);
    assert_eq!(parse(&with("jdk = \"21.0.5+11\"")).toolchain.jdk, JdkRequest::Release("jdk-21.0.5+11".into()));
    assert_eq!("jdk8u432-b06".parse::<JdkRequest>().unwrap().major(), Some(8));
    assert!(parse_err(&with("jdk = \"seven\"")).contains("JDK"));
    assert!(parse_err(&with("jdk = 6")).contains("not supported"));
    assert!(parse_err(&with("jdk = true")).contains("jdk"));
    assert!(parse_err(&with("jdk = \"21 0\"")).contains("invalid JDK"));
    assert!(parse_err(&with("vendor = \"x\"")).contains("vendor"));
    // jvm-target must not exceed the pinned JDK.
    let err = parse_err(&format!(
        "{MINIMAL}
[toolchain]
jdk = \"17\"
[target.jvm]
jvm-target = \"21\"
"
    ));
    assert!(err.contains("newer than"), "{err}");
}

#[test]
fn default_jvm_target_follows_older_jdks() {
    let jvm = |text: &str| parse(text).targets.jvm.unwrap();
    // A pinned JDK older than the default lowers the default target.
    assert_eq!(jvm(&format!("{MINIMAL}\n[toolchain]\njdk = \"17\"\n")).jvm_target, "17");
    assert_eq!(jvm(&format!("{MINIMAL}\n[toolchain]\njdk = \"21\"\n[target.jvm]\n")).jvm_target, "21");
    // So does an older system JDK at build time.
    let default = jvm(MINIMAL);
    assert_eq!(default.target_for(21).unwrap(), "21");
    assert_eq!(default.target_for(99).unwrap(), DEFAULT_JVM_TARGET);
    // An explicit target is kept, as long as the JDK can compile for it.
    let explicit = jvm(&format!("{MINIMAL}\n[target.jvm]\njvm-target = \"11\"\n"));
    assert_eq!(explicit.target_for(21).unwrap(), "11");
    assert_eq!(explicit.target_for(11).unwrap(), "11");
}

#[test]
fn jvm_target_is_normalized_for_kotlin() {
    let jvm = |target: &str| parse(&format!("{MINIMAL}\n[target.jvm]\njvm-target = \"{target}\"\n")).targets.jvm.unwrap();
    // Kotlin spells Java 8 `1.8` and rejects `8`.
    assert_eq!(jvm("8").jvm_target, "1.8");
    assert_eq!(jvm("1.8").jvm_target, "1.8");
    for target in ["9", "11", "17", "21", "25", "26"] {
        assert_eq!(jvm(target).jvm_target, target);
    }
    assert!(jvm("8").jvm_target_set);
    // Java 8 is also what the default lowers to on a JDK 8.
    assert_eq!(parse(MINIMAL).targets.jvm.unwrap().target_for(8).unwrap(), "1.8");
}

#[test]
fn jvm_target_is_validated_at_load() {
    for bad in ["abc", "", "7", "1.7", "1.9", "1.11", "0", "-1", "17.0", "017", "+17", "1.8.0"] {
        let err = parse_err(&format!("{MINIMAL}\n[target.jvm]\njvm-target = \"{bad}\"\n"));
        assert!(err.contains(&format!("jvm-target `{bad}`")), "{bad}: {err}");
        assert!(err.contains("`8` or `1.8`"), "{bad}: {err}");
    }
    // Surrounding whitespace is ignored, like in `[toolchain] jdk`.
    let padded = parse(&format!("{MINIMAL}\n[target.jvm]\njvm-target = \" 21 \"\n")).targets.jvm.unwrap();
    assert_eq!(padded.jvm_target, "21");
}

#[test]
fn jvm_target_must_not_exceed_the_jdk() {
    let jvm = |target: &str| parse(&format!("{MINIMAL}\n[target.jvm]\njvm-target = \"{target}\"\n")).targets.jvm.unwrap();
    let err = jvm("26").target_for(25).unwrap_err().to_string();
    assert!(err.contains("jvm-target 26 is newer than JDK 25"), "{err}");
    assert!(err.contains("[toolchain] jdk = \"26\""), "{err}");
    assert_eq!(jvm("25").target_for(25).unwrap(), "25");
    assert_eq!(jvm("1.8").target_for(11).unwrap(), "1.8");
    // The same check applies to a pinned JDK at load time, with the same hint.
    let err = parse_err(&format!("{MINIMAL}\n[toolchain]\njdk = \"21\"\n[target.jvm]\njvm-target = \"26\"\n"));
    assert!(err.contains("jvm-target 26 is newer than JDK 21"), "{err}");
    assert!(err.contains("[toolchain] jdk = \"26\""), "{err}");
}

#[test]
fn jdk_request_display_round_trips() {
    for s in ["system", "21", "jdk-21.0.5+11", "jdk8u432-b06"] {
        assert_eq!(s.parse::<JdkRequest>().unwrap().to_string(), s);
    }
    assert_eq!(release_major("jdk-25.0.1+8"), Some(25));
    assert_eq!(release_major("nonsense"), None);
}

#[test]
fn doc_section() {
    assert_eq!(parse(MINIMAL).doc, DocSettings::default());
    let m = parse(&format!(
        "{MINIMAL}
[doc]
dokka = \"2.1.0\"
includes = [\"docs/module.md\"]
source-link = \"https://github.com/me/lib/blob/main/\"
external-links = [\"https://kotlinlang.org/api/kotlinx.coroutines/\"]
"
    ));
    assert_eq!(m.doc.dokka_version.as_deref(), Some("2.1.0"));
    assert_eq!(m.doc.includes, [PathBuf::from("docs/module.md")]);
    assert_eq!(m.doc.source_link.as_deref(), Some("https://github.com/me/lib/blob/main"));
    assert_eq!(m.doc.external_links, ["https://kotlinlang.org/api/kotlinx.coroutines/"]);
    let with = |body: &str| {
        format!(
            "{MINIMAL}
[doc]
{body}
"
        )
    };
    assert!(parse_err(&with("dokka = \"[2.0,3.0)\"")).contains("exact Dokka version"));
    assert!(parse_err(&with("dokka = \"\"")).contains("exact Dokka version"));
    assert!(parse_err(&with("source-link = \"git@github.com:me/lib\"")).contains("source-link"));
    assert!(parse_err(&with("external-links = [\"file:///docs\"]")).contains("external-links"));
    assert!(parse_err(&with("format = \"html\"")).contains("format"));
}

#[test]
fn fmt_and_lint_sections() {
    let m = parse(MINIMAL);
    assert_eq!(m.fmt, FmtSettings::default());
    assert_eq!(m.lint, LintSettings::default());
    let m = parse(&format!(
        "{MINIMAL}
[fmt]
ktlint = \"1.7.1\"
exclude = [\"src/generated/**\"]

[lint]
detekt = \"1.23.7\"
config = [\"config/detekt.yml\"]
baseline = \"config/baseline.xml\"
"
    ));
    assert_eq!(m.fmt.ktlint_version.as_deref(), Some("1.7.1"));
    assert_eq!(m.fmt.exclude, ["src/generated/**"]);
    assert_eq!(m.lint.detekt_version.as_deref(), Some("1.23.7"));
    assert_eq!(m.lint.config, [PathBuf::from("config/detekt.yml")]);
    assert_eq!(m.lint.baseline, Some(PathBuf::from("config/baseline.xml")));
    let with = |section: &str, body: &str| format!("{MINIMAL}\n[{section}]\n{body}\n");
    assert!(parse_err(&with("fmt", "ktlint = \"[1.0,2.0)\"")).contains("exact ktlint version"));
    assert!(parse_err(&with("fmt", "exclude = [\"!src/**\"]")).contains("exclude"));
    assert!(parse_err(&with("fmt", "exclude = [\"/abs/**\"]")).contains("exclude"));
    assert!(parse_err(&with("fmt", "indent = 2")).contains("indent"));
    assert!(parse_err(&with("lint", "detekt = \"\"")).contains("exact detekt version"));
    assert!(parse_err(&with("lint", "max-issues = 3")).contains("max-issues"));
}

#[test]
fn path_dependencies() {
    let m = parse(&format!(
        "{MINIMAL}
[dependencies]
mathlib = {{ path = \"../mathlib\" }}
\"com.a:x\" = \"1.0\"

[dev-dependencies]
fixtures = {{ path = \"../fixtures\", scope = \"runtime\" }}

[target.jvm.dependencies]
jvmonly = {{ path = \"libs/jvmonly\" }}
"
    ));
    assert_eq!(m.dependencies.len(), 1);
    let names: Vec<_> = m.path_dependencies.iter().map(|d| (d.name.as_str(), d.dev, d.target)).collect();
    assert_eq!(names, [("jvmonly", false, Some(TargetKind::Jvm)), ("mathlib", false, None), ("fixtures", true, None)]);
    let mathlib = &m.path_dependencies[1];
    assert_eq!(mathlib.path, PathBuf::from("../mathlib"));
    assert_eq!(mathlib.scope, Scope::Compile);
    assert!(mathlib.applies_to(TargetKind::Wasm, false) && !mathlib.applies_to(TargetKind::Wasm, true));
    assert!(!m.path_dependencies[0].applies_to(TargetKind::Native, false));
    assert_eq!(m.path_dependencies[2].scope, Scope::Runtime);

    let with = |entry: &str| format!("{MINIMAL}\n[dependencies]\n{entry}\n");
    assert!(parse_err(&with("lib = { path = \"../lib\", version = \"1.0\" }")).contains("`version` does not apply to a `path` dependency"));
    assert!(parse_err(&with("\"g:a\" = { path = \"../lib\" }")).contains("package name"));
    assert!(parse_err(&with("lib = { path = \"\" }")).contains("must not be empty"));
    assert!(parse_err(&with("lib = { path = \"../lib\", scope = \"ksp\" }")).contains("KSP processor"));
    assert!(parse_err(&with("lib = { workspace = true }")).contains("needs a [workspace]"));
}

#[test]
fn workspace_manifest() {
    let root = Path::new("/ws");
    let ws = WorkspaceManifest::parse(
        "[workspace]
members = [\"app\", \"libs/core\"]

[workspace.kotlin]
version = \"2.3.0\"

[workspace.dependencies]
\"com.a:x\" = \"1.0\"
core = { path = \"libs/core\" }
",
        root,
    )
    .unwrap();
    assert_eq!(ws.members, [PathBuf::from("app"), PathBuf::from("libs/core")]);
    assert_eq!(ws.kotlin_version.as_deref(), Some("2.3.0"));
    assert!(ws.has_dependency("core") && !ws.has_dependency("app"));
    assert!(is_workspace("[workspace]\nmembers = [\"a\"]\n").unwrap());
    assert!(!is_workspace(MINIMAL).unwrap());
    assert!(
        format!("{:#}", is_workspace(&format!("{MINIMAL}[workspace]\nmembers = [\"a\"]\n")).unwrap_err())
            .contains("cannot also have [package]")
    );
    assert!(Manifest::parse("[workspace]\nmembers = [\"a\"]\n").unwrap_err().to_string().contains("workspace manifest"));

    let ws_err = |text: &str| format!("{:#}", WorkspaceManifest::parse(text, root).unwrap_err());
    assert!(ws_err("[workspace]\nmembers = []\n").contains("`members` is empty"));
    assert!(ws_err("[workspace]\nmembers = [\"libs/*\"]\n").contains("must be directories"));
    assert!(ws_err("[workspace]\nmembers = [\"a\", \"a\"]\n").contains("more than once"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\nexclude = [\"b\"]\n").contains("exclude"));
    assert!(
        ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.dependencies]\nx = { workspace = true }\n").contains("only valid in a member")
    );
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.dependencies]\nx = \"1.0\"\n").contains("`group:artifact`"));

    // Members inherit entries, the Kotlin version and (only) override the scope.
    let member = |deps: &str| Manifest::parse_in(&format!("{MINIMAL}\n[dependencies]\n{deps}\n"), Some(&ws));
    let m = member("\"com.a:x\" = { workspace = true, scope = \"runtime\" }\ncore = { workspace = true }").unwrap();
    assert_eq!(m.kotlin.version, "2.3.0");
    assert!(m.kotlin.version_set);
    assert_eq!(m.dependencies[0].version.as_deref(), Some("1.0"));
    assert_eq!(m.dependencies[0].scope, Scope::Runtime);
    assert_eq!(m.path_dependencies[0].path, root.join("libs/core"));
    assert!(format!("{:#}", member("\"com.a:x\" = { workspace = true, version = \"2.0\" }").unwrap_err()).contains("cannot be combined"));
    assert!(format!("{:#}", member("y = { workspace = true }").unwrap_err()).contains("not declared in [workspace.dependencies]"));
    assert!(format!("{:#}", member("\"com.a:x\" = { workspace = false }").unwrap_err()).contains("can only be `true`"));
    let own = Manifest::parse_in(&format!("{MINIMAL}\n[kotlin]\nversion = \"2.4.0\"\n"), Some(&ws)).unwrap_err();
    assert!(format!("{own:#}").contains("differs from the workspace's"));
    assert!(!parse(MINIMAL).kotlin.version_set);
}

#[test]
fn test_section() {
    assert!(parse(MINIMAL).test.fail_fast, "targets stop at the first failure by default");
    assert!(!parse(&format!("{MINIMAL}\n[test]\nfail-fast = false\n")).test.fail_fast);
    assert!(parse_err(&format!("{MINIMAL}\n[test]\nfail-fast = \"no\"\n")).contains("fail-fast"));
    assert!(parse_err(&format!("{MINIMAL}\n[test]\nthreads = 2\n")).contains("threads"));
}

#[test]
fn bin_sections() {
    assert!(parse(MINIMAL).bins.is_empty());
    let m = parse(&format!(
        "{MINIMAL}
[[bin]]
name = \"migrate\"
main = \"com.example.tools.MigrateKt\"
entry-point = \"com.example.tools.main\"

[[bin]]
name = \"seed\"
path = \"src/tools/Seed.kt\"
"
    ));
    assert_eq!(m.bins.len(), 2);
    let migrate = m.bin("migrate").unwrap();
    assert_eq!(migrate.main.as_deref(), Some("com.example.tools.MigrateKt"));
    assert_eq!(migrate.entry_point.as_deref(), Some("com.example.tools.main"));
    assert_eq!(migrate.path, None);
    assert_eq!(m.bin("seed").unwrap().path, Some(PathBuf::from("src/tools/Seed.kt")));
    assert!(m.bin("nope").is_none());

    let bin = |body: &str| format!("{MINIMAL}\n[[bin]]\n{body}\n");
    assert!(parse_err(&bin("name = \"x\"")).contains("needs an entry point"));
    assert!(parse_err(&bin("name = \"x\"\nmain = \"a.B\"\npath = \"src/B.kt\"")).contains("not both"));
    assert!(parse_err(&bin("name = \"1x\"\nmain = \"a.B\"")).contains("invalid [[bin]] name"));
    assert!(parse_err(&bin("name = \"x\"\nmain = \"a..B\"")).contains("invalid `main`"));
    assert!(parse_err(&bin("name = \"x\"\npath = \"src/B.java\"")).contains("Kotlin source file"));
    assert!(parse_err(&bin("name = \"x\"\nmain = \"a.B\"\nargs = []")).contains("unknown field"));
    let twice = format!("{}[[bin]]\nname = \"x\"\nmain = \"a.C\"\n", bin("name = \"x\"\nmain = \"a.B\""));
    assert!(parse_err(&twice).contains("declared more than once"));
}
