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
    assert!(m.profiles["dev"].debug && !m.profiles["dev"].optimize);
    assert!(!m.profiles["release"].debug && m.profiles["release"].optimize);
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
gradle-version = "9.6.0"

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
    assert_eq!(android.config.gradle_version.as_deref(), Some("9.6.0"));
    assert_eq!(android.config.agp_version, None);

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

    assert_eq!(m.profiles["release"].compiler_args, ["-Xno-call-assertions"]);
    assert!(m.profiles["dev"].optimize && m.profiles["dev"].debug);

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
    assert_eq!(sqlite.options.compiler_options, ["-I/usr/include"]);
    assert_eq!(sqlite.options.linker_options, ["-lsqlite3"]);
    assert_eq!(sqlite.link, LinkMode::Dynamic);
    assert!(sqlite.def_properties().is_empty());
}

#[test]
fn native_platform_options() {
    let m = parse(&format!(
        r#"{MINIMAL}
[target.native]
platforms = ["linux_x64", "macos_arm64", "mingw_x64"]
linker-options = ["-lm"]

[target.native.platform.macosArm64]
frameworks = ["Foundation"]

[target.native.platform.mingw_x64]
library-paths = ["vendor/lib"]
static-libraries = ["libextra.a"]

[target.native.cinterop.sqlite]
headers = ["sqlite3.h"]
package = "sqlite3"
pkg-config = "sqlite3"
link = "static"
header-filter = ["sqlite3*.h"]
excluded-functions = ["sqlite3_trace"]
strict-enums = ["Color"]
language = "C"

[target.native.cinterop.sqlite.platform.mingw_x64]
compiler-options = ["-Ivendor/sqlite/include"]
linker-options = ["-Lvendor/sqlite/lib", "-lsqlite3"]
"#
    ));
    let native = m.targets.native.as_ref().unwrap();
    assert_eq!(native.options_for("linux_x64").linker_options, ["-lm"]);
    assert_eq!(native.options_for("macos_arm64").frameworks, ["Foundation"]);
    let mingw = native.options_for("mingw_x64");
    assert_eq!(mingw.linker_options, ["-lm"]);
    assert_eq!(mingw.library_paths, [PathBuf::from("vendor/lib")]);
    assert_eq!(mingw.static_libraries, ["libextra.a"]);

    let sqlite = &native.cinterop[0];
    assert_eq!(sqlite.pkg_config, ["sqlite3"]);
    assert_eq!(sqlite.link, LinkMode::Static);
    assert!(sqlite.describes_library());
    assert!(sqlite.options_for("linux_x64").is_empty());
    let windows = sqlite.options_for("mingw_x64");
    assert_eq!(windows.compiler_options, ["-Ivendor/sqlite/include"]);
    assert_eq!(windows.linker_options, ["-Lvendor/sqlite/lib", "-lsqlite3"]);
    assert_eq!(
        sqlite.def_properties(),
        [
            ("headerFilter", "sqlite3*.h".to_owned()),
            ("excludedFunctions", "sqlite3_trace".to_owned()),
            ("strictEnums", "Color".to_owned()),
            ("language", "C".to_owned())
        ]
    );
    let objc = parse(&format!(
        "{MINIMAL}\n[target.native.cinterop.kit]\nmodules = [\"AppKit\"]\nlanguage = \"Objective-C\"\npkg-config = [\"a\", \"b\"]\n"
    ));
    let kit = &objc.targets.native.as_ref().unwrap().cinterop[0];
    assert!(kit.describes_library());
    assert_eq!(kit.pkg_config, ["a", "b"]);
    assert_eq!(kit.def_properties(), [("modules", "AppKit".to_owned()), ("language", "Objective-C".to_owned())]);
}

#[test]
fn native_c_sources_and_frameworks() {
    let m = parse(&format!(
        r#"{MINIMAL}
[target.native.c-sources]
defines = ["NDEBUG", "LEVEL=2"]

[target.native.c-sources.platform.linux_x64]
defines = ["LINUX"]
compiler-options = ["-O3"]
"#
    ));
    let c = m.targets.native.as_ref().unwrap().c_sources.as_ref().unwrap();
    assert_eq!(c.sources, ["c"]);
    assert_eq!(c.include_dirs, [PathBuf::from("c")]);
    assert_eq!(c.flags_for("linux_x64").defines, ["NDEBUG", "LEVEL=2", "LINUX"]);
    assert_eq!(c.flags_for("linux_x64").compiler_options, ["-O3"]);
    assert_eq!(c.flags_for("mingw_x64").defines, ["NDEBUG", "LEVEL=2"]);
    let explicit = parse(&format!(
        "{MINIMAL}\n[target.native.c-sources]\nsources = [\"native/*.c\", \"vendor/lib.c\"]\ninclude-dirs = [\"native/include\"]\n"
    ));
    let c = explicit.targets.native.as_ref().unwrap().c_sources.as_ref().unwrap();
    assert_eq!(c.sources, ["native/*.c", "vendor/lib.c"]);
    assert_eq!(c.include_dirs, [PathBuf::from("native/include")]);

    let framework = parse(&format!(
        "{MINIMAL}\n[target.native]\nplatforms = [\"ios_arm64\", \"ios_simulator_arm64\"]\nkind = \"framework\"\nbundle-id = \"com.example.Shared\"\nexport = [\"com.example:core\"]\n"
    ));
    let native = framework.targets.native.as_ref().unwrap();
    assert_eq!(native.kind, NativeKind::Framework);
    assert_eq!(native.bundle_id.as_deref(), Some("com.example.Shared"));
    assert_eq!(native.export, ["com.example:core"]);
    assert!(is_apple_target("ios_simulator_arm64"));
    assert!(!is_apple_target("linux_x64"));
}

#[test]
fn native_interop_errors() {
    let with = |t: &str| format!("{MINIMAL}\n{t}\n");
    let err = parse_err(&with("[target.native.platform.host]\nlinker-options = []"));
    assert!(err.contains("platform.host] is not supported"), "{err}");
    let err = parse_err(&with("[target.native.platform.plan9]\nlinker-options = []"));
    assert!(err.contains("invalid table [target.native.platform.plan9]"), "{err}");
    // [target.native] has no C compiler; options for clang belong to a cinterop entry or c-sources.
    assert!(parse_err(&with("[target.native.platform.linux_x64]\ncompiler-options = []")).contains("unknown field"));
    assert!(parse_err(&with("[target.native]\nstatic-libraries = [\"lib/libx.a\"]")).contains("invalid static library"));
    assert!(parse_err(&with("[target.native]\nframeworks = [\"UIKit.framework\"]")).contains("invalid framework"));
    let cinterop = |t: &str| parse_err(&with(&format!("[target.native.cinterop.x]\n{t}")));
    assert!(cinterop("link = \"shared\"").contains("unknown link `shared`"));
    assert!(cinterop("language = \"Swift\"").contains("unknown language `Swift`"));
    assert!(cinterop("pkg-config = \"lib sqlite\"").contains("invalid pkg-config package"));
    assert!(cinterop("header-filter = [\"a b\"]").contains("separated by spaces"));
    assert!(cinterop("[target.native.cinterop.x.platform.linux_x64]\npkg-config = \"x\"").contains("unknown field"));
    let c = |t: &str| parse_err(&with(&format!("[target.native.c-sources]\n{t}")));
    assert!(c("sources = []").contains("`sources` is empty"));
    assert!(c("sources = [\"../outside.c\"]").contains("invalid source"));
    assert!(c("defines = [\"-DX\"]").contains("invalid define"));
    let native = |t: &str| parse_err(&with(&format!("[target.native]\n{t}")));
    assert!(native("platforms = [\"linux_x64\"]\nkind = \"framework\"").contains("builds Apple frameworks"));
    assert!(native("bundle-id = \"a.b\"").contains("only applies to kind = \"framework\""));
    assert!(native("kind = \"shared\"\nexport = [\"a\"]").contains("invalid entry `a`"));
    assert!(native("export = [\"a:b\"]").contains("`export` only applies"));
}

#[test]
fn wasm_target_options() {
    let with = |t: &str| format!("{MINIMAL}\n{t}\n");
    let wasm = |t: &str| parse(&with(t)).targets.wasm.unwrap();

    let default = wasm("[target.wasm]");
    assert_eq!(default.environment, WasmEnvironment::Js);
    assert_eq!(default.kind, WasmKind::Application);
    assert_eq!(default.wasm_opt, WasmOpt::Auto);
    assert_eq!(default.opt_level, WasmOptLevel::Balanced);
    assert!(!default.debug_info && !default.content_hash && default.compress.is_empty());
    assert!(default.npm_dependencies.is_empty());
    assert_eq!(default.module_name(&parse(MINIMAL).package), "hello");

    let browser = wasm("[target.wasm]\nenvironment = \"browser\"");
    assert_eq!(browser.environment, WasmEnvironment::Browser);
    assert_eq!(browser.environment.as_str(), "browser");
    // Browsers and Node.js run the same Kotlin/Wasm target and use the same libraries.
    assert_eq!(browser.environment.platform(), "js");
    assert_eq!(WasmEnvironment::Wasi.platform(), "wasi");

    let library = wasm(
        r#"
[target.wasm]
environment = "browser"
kind = "library"
module-name = "hello.core"
npm-name = "@acme/hello"
wasm-opt = "tools/wasm-opt"
opt-level = "size"
wasm-opt-flags = ["--metrics"]
debug-info = true
compress = ["gzip", "brotli"]
content-hash = true
max-size = "1 MiB"
max-gzip-size = "128 KiB"
wasm-enable-array-range-checks = true
wasm-debugger-custom-formatters = true

[target.wasm.npm-dependencies]
"@js-joda/core" = "^3.2.0"
"#,
    );
    assert_eq!(library.kind, WasmKind::Library);
    assert_eq!(library.module_name(&parse(MINIMAL).package), "hello.core");
    assert_eq!(library.npm_package_name(&parse(MINIMAL).package), "@acme/hello");
    assert_eq!(library.wasm_opt, WasmOpt::Path("tools/wasm-opt".into()));
    assert_eq!(library.opt_level, WasmOptLevel::Size);
    assert_eq!(library.wasm_opt_flags, ["--metrics"]);
    assert!(library.debug_info && library.content_hash && library.wasm_debugger_custom_formatters);
    assert_eq!(library.compress, [WasmCompression::Gzip, WasmCompression::Brotli]);
    assert_eq!(library.max_size, Some(1024 * 1024));
    assert_eq!(library.max_gzip_size, Some(128 * 1024));
    assert_eq!(library.wasm_enable_array_range_checks, Some(true));
    assert_eq!(library.npm_dependencies["@js-joda/core"], "^3.2.0");

    // The npm name defaults to the lowercase package name.
    let lowercase = Manifest::parse("[package]\nname = \"Hello_App\"\nversion = \"1\"\n[target.wasm]\nkind = \"lib\"\n").unwrap();
    assert_eq!(lowercase.targets.wasm.unwrap().npm_package_name(&lowercase.package), "hello_app");

    assert_eq!(wasm("[target.wasm]\nwasm-opt = false").wasm_opt, WasmOpt::Off);
    assert_eq!(wasm("[target.wasm]\nwasm-opt = true").wasm_opt, WasmOpt::Auto);

    assert_eq!(default.port, None);
    assert_eq!(wasm("[target.wasm]\nenvironment = \"browser\"\nport = 5173").port, Some(5173));
    assert_eq!(wasm("[target.wasm]\nenvironment = \"browser\"\nport = 0").port, Some(0));
}

#[test]
fn browser_apps_are_bundled_and_proxied() {
    let wasm = |t: &str| parse(&format!("{MINIMAL}\n{t}\n")).targets.wasm.unwrap();
    assert!(wasm("[target.wasm]\nenvironment = \"browser\"").bundling.is_none());
    let bundled = wasm("[target.wasm]\nenvironment = \"browser\"\nbundler = \"rollup\"\nexternal = [\"react\"]").bundling.unwrap();
    assert_eq!((bundled.bundler, bundled.bundler.as_str()), (Bundler::Rollup, "rollup"));
    assert_eq!(bundled.external, ["react"]);
    // Release builds bundle and write no source maps; the other builds are served unbundled with maps.
    assert!(bundled.applies(true) && !bundled.applies(false));
    assert!(!bundled.wants_source_maps(true) && bundled.wants_source_maps(false));
    let always = wasm("[target.wasm]\nenvironment = \"browser\"\nbundler = \"bun\"\nbundle = true\nsource-maps = true").bundling.unwrap();
    assert!(always.applies(false) && always.wants_source_maps(true));
    let never = wasm("[target.wasm]\nenvironment = \"browser\"\nbundler = \"bun\"\nbundle = false").bundling.unwrap();
    assert!(!never.applies(true));

    let served = wasm(
        "[target.wasm]\nenvironment = \"browser\"\n[target.wasm.dev-server]\nport = 3000\n[target.wasm.dev-server.proxy]\n\"/api\" = \"http://localhost:8080\"\n\"/api/v2\" = \"http://[::1]:9000/backend/\"\n",
    );
    assert_eq!(served.port, Some(3000));
    // The longest prefix comes first, so it wins.
    assert_eq!(served.proxy.iter().map(|r| r.prefix.as_str()).collect::<Vec<_>>(), ["/api/v2", "/api"]);
    assert_eq!((served.proxy[1].host.as_str(), served.proxy[1].port, served.proxy[1].base.as_deref()), ("localhost", 8080, None));
    assert_eq!((served.proxy[0].host.as_str(), served.proxy[0].port, served.proxy[0].base.as_deref()), ("[::1]", 9000, Some("/backend")));

    let js = parse(&format!("{MINIMAL}\n[target.js]\nenvironment = \"browser\"\nbundler = \"bun\"\n[target.js.dev-server]\nport = 4000\n"))
        .targets
        .js
        .unwrap();
    assert_eq!((js.bundling.unwrap().bundler, js.port), (Bundler::Bun, Some(4000)));
}

#[test]
fn libraries_are_bundled_in_several_formats() {
    let parse_target = |t: &str| parse(&format!("{MINIMAL}\n{t}\n")).targets;
    let js = parse_target(
        "[target.js]\nkind = \"library\"\nbundler = \"rollup\"\nformats = [\"umd\", \"esm\", \"cjs\"]\nglobal-name = \"acme.Greeter\"\nexternal = [\"lodash\"]\n[target.js.globals]\nlodash = \"_\"\n",
    )
    .js
    .unwrap();
    let bundling = js.bundling.unwrap();
    assert_eq!(bundling.formats, [LibraryFormat::Esm, LibraryFormat::Cjs, LibraryFormat::Umd], "sorted");
    assert_eq!((bundling.global_name.as_deref(), bundling.globals["lodash"].as_str()), (Some("acme.Greeter"), "_"));
    assert_eq!(LibraryFormat::Umd.extension(), "umd.js");
    let cjs = parse_target("[target.js]\nkind = \"library\"\nbundler = \"bun\"\nformats = [\"cjs\"]\n").js.unwrap().bundling.unwrap();
    assert_eq!(cjs.formats, [LibraryFormat::Cjs]);
    let wasm = parse_target("[target.wasm]\nkind = \"library\"\nbundler = \"bun\"\n").wasm.unwrap().bundling.unwrap();
    assert_eq!(wasm.formats, [LibraryFormat::Esm]);
    // A library for Node.js is bundled too.
    let node = parse_target("[target.wasm]\nenvironment = \"js\"\nkind = \"library\"\nbundler = \"bun\"\n").wasm.unwrap();
    assert!(node.bundling.is_some());
}

#[test]
fn proxy_targets_can_use_tls() {
    let wasm = |t: &str| {
        parse(&format!("{MINIMAL}\n[target.wasm]\nenvironment = \"browser\"\n[target.wasm.dev-server.proxy]\n{t}\n")).targets.wasm.unwrap()
    };
    let routes = wasm(
        "\"/a\" = \"https://api.example.com\"\n\"/b\" = { target = \"https://localhost:8443/v1\", insecure = true }\n\"/c\" = \"wss://sockets.example.com:9000\"\n\"/d\" = \"ws://localhost\"",
    )
    .proxy;
    let route = |prefix: &str| routes.iter().find(|r| r.prefix == prefix).unwrap();
    assert!(route("/a").tls && !route("/a").insecure && route("/a").port == 443);
    assert!(route("/b").tls && route("/b").insecure && route("/b").port == 8443 && route("/b").base.as_deref() == Some("/v1"));
    assert!(route("/c").tls && route("/c").port == 9000);
    assert!(!route("/d").tls && route("/d").port == 80);
}

#[test]
fn bundling_and_dev_server_errors() {
    let wasm = |t: &str| parse_err(&format!("{MINIMAL}\n[target.wasm]\n{t}\n"));
    let browser = "environment = \"browser\"\n";
    assert!(wasm(&format!("{browser}bundler = \"webpack\"")).contains("unknown bundler `webpack` (expected `bun` or `rollup`)"));
    assert!(wasm("bundler = \"bun\"").contains("`bundler` bundles a browser application or an npm library"));
    assert!(wasm(&format!("{browser}bundle = true")).contains("set `bundler = \"bun\"` or `\"rollup\"`"));
    assert!(wasm(&format!("{browser}source-maps = true")).contains("configure the bundler"));
    assert!(wasm(&format!("{browser}bundler = \"bun\"\nexternal = [\" \"]")).contains("`external` lists"));
    assert!(wasm("[target.wasm.dev-server]\nport = 80").contains("configures the server `kargo run` starts"));
    assert!(wasm(&format!("{browser}port = 80\n[target.wasm.dev-server]\nport = 81")).contains("both set the port"));
    let proxy = |route: &str| wasm(&format!("{browser}[target.wasm.dev-server.proxy]\n{route}"));
    assert!(proxy("\"api\" = \"http://localhost\"").contains("is not a path prefix"));
    assert!(proxy("\"/api\" = \"ftp://localhost\"").contains("an `http://` or `https://` URL"));
    assert!(proxy("\"/api\" = { target = \"http://localhost\", insecure = true }").contains("`insecure` is about the certificate"));
    assert!(proxy("\"/api\" = { target = \"https://localhost\", verify = false }").contains("did not match any variant"));
    assert!(proxy("\"/\" = \"http://localhost\"").contains("would hide the app's own files"));
    assert!(proxy("\"/api\" = \"http://localhost:http\"").contains("the port is not a number"));
    assert!(proxy("\"/api\" = \"http://\"").contains("no host"));
    assert!(proxy("\"/api\" = \"http://localhost/x?y\"").contains("no query"));
    assert!(proxy("\"/api\" = \"http://a\"\n\"/api/\" = \"http://b\"").contains("lists `/api` twice"));

    let js = |t: &str| parse_err(&format!("{MINIMAL}\n[target.js]\n{t}\n"));
    let umd = js("environment = \"browser\"\nmodule-kind = \"umd\"\nbundler = \"bun\"");
    assert!(umd.contains("`bundler` bundles ES modules"), "{umd}");
    let node = js("bundler = \"bun\"");
    assert!(node.contains("[target.js] `bundler` bundles a browser application"), "{node}");

    let library = |t: &str| js(&format!("kind = \"library\"\nbundler = \"rollup\"\n{t}"));
    assert!(library("formats = [\"amd\"]").contains("unknown format `amd`"));
    assert!(library("formats = [\"esm\", \"esm\"]").contains("lists `esm` twice"));
    assert!(library("globals = { react = \"React\" }").contains("for the `umd` format"));
    assert!(library("formats = [\"umd\"]\nglobal-name = \"my lib\"").contains("`global-name` is `my lib`"));
    assert!(library("formats = [\"umd\"]\nglobals = { react = \"1React\" }").contains("[target.js.globals] `react`"));
    let bun_umd = js("kind = \"library\"\nbundler = \"bun\"\nformats = [\"umd\"]");
    assert!(bun_umd.contains("the `umd` format needs `bundler = \"rollup\"`"), "{bun_umd}");
    let app_formats = js("environment = \"browser\"\nbundler = \"bun\"\nformats = [\"esm\"]");
    assert!(app_formats.contains("are about the files of a library"), "{app_formats}");
    let wasm_cjs = wasm("kind = \"library\"\nbundler = \"rollup\"\nformats = [\"esm\", \"cjs\"]");
    assert!(wasm_cjs.contains("can only be [\"esm\"] for Kotlin/Wasm"), "{wasm_cjs}");
}

#[test]
fn wasm_target_option_errors() {
    let with = |t: &str| format!("{MINIMAL}\n[target.wasm]\n{t}\n");
    assert!(parse_err(&with("kind = \"plugin\"")).contains("unknown wasm kind `plugin` (expected `application` or `library`)"));
    assert!(parse_err(&with("environment = \"wasi\"\nkind = \"library\"")).contains("needs environment = \"js\" or \"browser\""));
    assert!(parse_err(&with("module-name = \"a/b\"")).contains("invalid [target.wasm] module-name `a/b`"));
    assert!(parse_err(&with("module-name = \".hidden\"")).contains("invalid [target.wasm] module-name"));
    assert!(parse_err(&with("npm-name = \"hello\"")).contains("set kind = \"library\""));
    assert!(parse_err(&with("kind = \"library\"\nnpm-name = \"Hello\"")).contains("invalid [target.wasm] npm-name `Hello`"));
    assert!(parse_err(&with("kind = \"library\"\nnpm-name = \"@scope\"")).contains("invalid [target.wasm] npm-name"));
    assert!(parse_err(&with("wasm-opt = 3")).contains("`wasm-opt` must be true, false or the path"));
    assert!(parse_err(&with("opt-level = \"tiny\"")).contains("expected `size`, `balanced` or `speed`"));
    assert!(parse_err(&with("compress = [\"zip\"]")).contains("expected `gzip` or `brotli`"));
    assert!(parse_err(&with("max-size = \"0 MiB\"")).contains("must be a positive size"));
    assert!(parse_err(&with("npm-dependencies = { left-pad = 1 }")).contains("invalid type"));
    assert!(parse_err(&with("[target.wasm.npm-dependencies]\nleft-pad = \"\"")).contains("needs a package name and a version"));
    assert!(parse_err(&with("html = \"index.html\"")).contains("unknown field"));
    assert!(parse_err(&with("port = 5173")).contains("`port` is the port `kargo run` serves a browser app on"));
    assert!(parse_err(&with("environment = \"browser\"\nport = 70000")).contains("invalid value"));
}

#[test]
fn js_target_options() {
    let js = |extra: &str| parse(&format!("{MINIMAL}\n{extra}\n")).targets.js.unwrap();
    let default = js("[target.js]");
    assert_eq!(default.environment, JsEnvironment::Node);
    assert_eq!(default.kind, JsKind::Application);
    assert_eq!(default.module_kind, JsModuleKind::Es);
    assert_eq!(default.module_kind.extension(), "mjs");
    assert_eq!(default.module_name(&parse(MINIMAL).package), "hello");
    assert_eq!(parse(&format!("{MINIMAL}\n[target.js]\n")).targets.enabled(), [TargetKind::Js]);

    let library = js(r#"
[target.js]
environment = "browser"
kind = "library"
module-kind = "umd"
module-name = "hello.core"
npm-name = "@acme/hello"
package-manager = "pnpm"

[target.js.npm-dependencies]
"left-pad" = "^1.3.0"

[target.js.dependencies]
"org.example:dom" = "1.0"
"#);
    assert_eq!((library.environment, library.kind, library.module_kind), (JsEnvironment::Browser, JsKind::Library, JsModuleKind::Umd));
    assert_eq!(library.module_kind.extension(), "js");
    assert_eq!(library.module_name(&parse(MINIMAL).package), "hello.core");
    assert_eq!(library.npm_package_name(&parse(MINIMAL).package), "@acme/hello");
    assert_eq!(library.package_manager, Some(PackageManager::Pnpm));
    assert_eq!(library.npm_dependencies["left-pad"], "^1.3.0");
    assert_eq!(library.dependencies[0].module.to_string(), "org.example:dom");
    assert_eq!(js("[target.js]\nmodule-kind = \"commonjs\"").module_kind, JsModuleKind::CommonJs);
    assert_eq!(default.port, None);
    assert_eq!(js("[target.js]\nenvironment = \"browser\"\nport = 5173").port, Some(5173));
}

#[test]
fn js_target_option_errors() {
    let with = |t: &str| format!("{MINIMAL}\n[target.js]\n{t}\n");
    assert!(parse_err(&with("environment = \"deno\"")).contains("unknown js environment `deno` (expected `node` or `browser`)"));
    assert!(parse_err(&with("kind = \"plugin\"")).contains("unknown js kind `plugin`"));
    assert!(parse_err(&with("module-kind = \"amd\"")).contains("unknown js module-kind `amd` (expected `es`, `commonjs` or `umd`)"));
    assert!(parse_err(&with("environment = \"browser\"\nmodule-kind = \"commonjs\"")).contains("cannot load a CommonJS module"));
    assert!(parse_err(&with("module-name = \"a/b\"")).contains("invalid [target.js] module-name `a/b`"));
    assert!(parse_err(&with("npm-name = \"hello\"")).contains("set kind = \"library\""));
    assert!(parse_err(&with("kind = \"library\"\nnpm-name = \"Hello\"")).contains("invalid [target.js] npm-name `Hello`"));
    assert!(parse_err(&with("package-manager = \"yarn\"")).contains("unknown package-manager `yarn`"));
    assert!(parse_err(&with("[target.js.npm-dependencies]\nleft-pad = \"\"")).contains("needs a package name and a version"));
    assert!(parse_err(&with("wasm-opt = false")).contains("unknown field"));
    assert!(parse_err(&with("port = 5173")).contains("`port` is the port `kargo run` serves a browser app on"));
}

#[test]
fn wasm_web_resources() {
    let with = |t: &str| format!("{MINIMAL}\n[target.wasm]\n{t}\n");
    let library = parse(&with(
        "environment = \"browser\"\nkind = \"library\"\nweb-resources = [\"web/raylib.mjs\", \"web/raylib.wasm\", \"web/assets\"]\nweb-init = \"web/init.mjs\"",
    ))
    .targets
    .wasm
    .unwrap();
    assert_eq!(library.web_resources, [PathBuf::from("web/raylib.mjs"), "web/raylib.wasm".into(), "web/assets".into()]);
    assert_eq!(library.web_init, Some(PathBuf::from("web/init.mjs")));
    assert!(parse(&with("kind = \"library\"")).targets.wasm.unwrap().web_resources.is_empty());
    // The init script may be listed among the resources too.
    parse(&with("kind = \"library\"\nweb-resources = [\"web/init.mjs\"]\nweb-init = \"web/init.mjs\""));

    assert!(
        parse_err(&with("web-resources = [\"a.mjs\"]"))
            .contains("set kind = \"library\", or put an application's own files in `resources/`")
    );
    assert!(parse_err(&with("web-init = \"init.mjs\"")).contains("set kind = \"library\""));
    assert!(parse_err(&with("kind = \"library\"\nenvironment = \"wasi\"\nweb-resources = [\"a.mjs\"]")).contains("needs environment"));
    assert!(parse_err(&with("kind = \"library\"\nweb-resources = [\"../a.mjs\"]")).contains("use a path inside the package"));
    assert!(parse_err(&with("kind = \"library\"\nweb-resources = [\"\"]")).contains("use a path inside the package"));
    assert!(
        parse_err(&with("kind = \"library\"\nweb-resources = [\"a/x.mjs\", \"b/x.mjs\"]")).contains("would both be shipped as `x.mjs`")
    );
    assert!(parse_err(&with("kind = \"library\"\nweb-resources = [\"a/init.mjs\"]\nweb-init = \"init.mjs\"")).contains("`init.mjs`"));
}

#[test]
fn target_errors() {
    let with = |t: &str| format!("{MINIMAL}\n{t}\n");
    assert!(parse_err(&with("[target]")).contains("no target is enabled"));
    assert!(parse_err(&with("[target.wasm]\nenvironment = \"node\"")).contains("expected `js`, `browser` or `wasi`"));
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
    let android = |t: &str| parse_err(&with(&format!("[target.android]\nnamespace = \"a\"\n{t}")));
    assert!(android("shrink-resources = true").contains("needs `minify = true`"));
    assert!(android("consumer-proguard-files = [\"x.pro\"]").contains("only applies to libraries"));
    assert!(android("build-config = { \"bad-name\" = 1 }").contains("not a valid Java field name"));
    assert!(android("build-config = { RATIO = 1.5 }").contains("build-config"));
    let signing = "store-file = \"k.jks\"\nstore-password-env = \"P\"\nkey-alias = \"a\"";
    assert!(android(&format!("[target.android.signing.staging]\n{signing}")).contains("use `debug` or `release`"));
    assert!(android(&format!("kind = \"library\"\n[target.android.signing.release]\n{signing}")).contains("only applies to applications"));
    assert!(android(&format!("[target.android.signing.release]\n{signing}\nstore-type = \"zip\"")).contains("store-type"));
    assert!(android("[target.android.signing.release]\nstore-file = \"k.jks\"\nkey-alias = \"a\"").contains("store-password-env"));
    assert!(
        android("[target.android.flavors.free]\ndimension = \"tier\"\n[target.android.flavors.dev]\ndimension = \"env\"")
            .contains("list their order in `flavor-dimensions`")
    );
    assert!(android("flavor-dimensions = [\"env\"]\n[target.android.flavors.free]\ndimension = \"tier\"").contains("has no flavors"));
    assert!(android("[target.android.flavors.release]\ndimension = \"tier\"").contains("cannot be a flavor name"));
    assert!(android("[target.android.flavors.\"a-b\"]\ndimension = \"tier\"").contains("invalid flavor name"));
    let native = parse(&with("[target.native]"));
    assert_eq!(native.targets.native.unwrap().platforms, ["host"]);
    assert!(native.targets.jvm.is_none(), "explicit targets replace the JVM default");
}

#[test]
fn android_namespace_defaults_to_group_and_name() {
    let namespace = |package: &str, android: &str| {
        let m = parse(&format!("[package]\n{package}version = \"1.0.0\"\n[target.android]\n{android}"));
        m.targets.android.unwrap().config.namespace
    };
    assert_eq!(namespace("name = \"core\"\ngroup = \"com.example\"\n", ""), "com.example.core");
    assert_eq!(namespace("name = \"my-lib\"\ngroup = \"com.Example\"\n", "kind = \"library\""), "com.example.my_lib");
    assert_eq!(namespace("name = \"core\"\ngroup = \"org.2x\"\n", ""), "org._2x.core");
    assert_eq!(namespace("name = \"class\"\ngroup = \"org.x\"\n", ""), "org.x._class");
    assert_eq!(namespace("name = \"core\"\ngroup = \"com.example\"\n", "namespace = \"com.other.ui\""), "com.other.ui");
    let err = parse_err(&format!("{MINIMAL}\n[target.android]\n"));
    assert!(err.contains("needs a `namespace`") && err.contains("`group`"), "{err}");
    assert!(parse_err(&format!("{MINIMAL}\n[target.android]\nnamespace = \"com.my-app\"\n")).contains("not a Java package name"));
}

#[test]
fn android_release_settings() {
    let m = parse(&format!(
        r#"{MINIMAL}
[target.android]
namespace = "com.example.app"
minify = true
proguard-files = ["android/proguard-rules.pro"]
build-config = {{ API_URL = "https://example.com", LOGGING = false, RETRIES = 3, BIG = 10000000000 }}

[target.android.signing.release]
store-file = "release.jks"
store-password-env = "KS_PASS"
key-alias = "upload"

[target.android.flavors.free]
dimension = "tier"
application-id-suffix = ".free"
build-config = {{ ADS = true }}

[target.android.flavors.paid]
dimension = "tier"
"#
    ));
    let cfg = &m.targets.android.as_ref().unwrap().config;
    assert!(cfg.minify);
    assert_eq!(cfg.shrink_resources, None);
    assert_eq!(cfg.proguard_files, [Path::new("android/proguard-rules.pro")]);
    assert_eq!(cfg.build_config["API_URL"].java(), ("String", "\"https://example.com\"".to_owned()));
    assert_eq!(cfg.build_config["LOGGING"].java(), ("boolean", "false".to_owned()));
    assert_eq!(cfg.build_config["RETRIES"].java(), ("int", "3".to_owned()));
    assert_eq!(cfg.build_config["BIG"].java(), ("long", "10000000000L".to_owned()));
    assert_eq!(BuildConfigValue::String("a\"b\\".into()).java().1, r#""a\"b\\""#);
    let signing = &cfg.signing["release"];
    assert_eq!(signing.key_alias, "upload");
    assert_eq!(signing.key_password_env(), "KS_PASS", "the key password defaults to the store password");
    assert_eq!(cfg.dimensions(), ["tier"]);
    assert_eq!(cfg.flavors_of("tier").map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["free", "paid"]);
    assert_eq!(cfg.flavors["free"].application_id_suffix.as_deref(), Some(".free"));
}

#[test]
fn android_lint_ndk_and_resources() {
    let m = parse(&format!(
        r#"{MINIMAL}
[target.android]
namespace = "com.example.app"
build-tools = "36.0.0"
locales = ["en", "pt-rBR"]
manifest-placeholders = {{ MAPS_KEY = "abc", DEBUGGABLE = true, PORT = 8080 }}

[target.android.ndk]
version = "27.2.12479018"
abi-filters = ["arm64-v8a", "x86_64"]
cmake = "src-android/cpp/CMakeLists.txt"
cmake-version = "3.22.1"
cmake-arguments = ["-DANDROID_STL=c++_shared"]
cpp-flags = ["-std=c++17"]

[target.android.lint]
baseline = "config/lint-baseline.xml"
warnings-as-errors = true
disable = ["MissingTranslation"]
error = ["HardcodedText"]
reports = ["html", "xml"]

[target.android.flavors.free]
dimension = "tier"
manifest-placeholders = {{ MAPS_KEY = "free-key" }}
"#
    ));
    let cfg = &m.targets.android.as_ref().unwrap().config;
    assert_eq!(cfg.build_tools.as_deref(), Some("36.0.0"));
    assert_eq!(cfg.locales, ["en", "pt-rBR"]);
    assert_eq!(cfg.manifest_placeholders["DEBUGGABLE"].text(), "true");
    assert_eq!(cfg.manifest_placeholders["PORT"].text(), "8080");
    assert_eq!(cfg.flavors["free"].manifest_placeholders["MAPS_KEY"].text(), "free-key");
    let ndk = cfg.ndk.as_ref().unwrap();
    assert_eq!(ndk.abi_filters, ["arm64-v8a", "x86_64"]);
    assert_eq!(ndk.cmake.as_deref(), Some(Path::new("src-android/cpp/CMakeLists.txt")));
    assert_eq!(ndk.cmake_version.as_deref(), Some("3.22.1"));
    assert_eq!(cfg.lint.baseline.as_deref(), Some(Path::new("config/lint-baseline.xml")));
    assert!(cfg.lint.warnings_as_errors);
    assert_eq!(cfg.lint.reports(), ["html", "xml", "sarif"], "SARIF is always written: `--message-format json` reads it");
    assert_eq!(AndroidLint::default().reports(), ["html", "xml", "sarif"]);
}

#[test]
fn android_lint_ndk_and_resources_are_validated() {
    let android = |t: &str| parse_err(&format!("{MINIMAL}\n[target.android]\nnamespace = \"a\"\n{t}"));
    assert!(android("build-tools = \"latest\"").contains("`build-tools` must be a version"));
    assert!(android("locales = [\"en US\"]").contains("`locales` entry"));
    assert!(android("manifest-placeholders = { \"a b\" = 1 }").contains("not a placeholder name"));
    assert!(android("manifest-placeholders = { RATIO = 1.5 }").contains("manifest-placeholders"));
    assert!(android("[target.android.ndk]\nabi-filters = [\"mips\"]").contains("not an Android ABI"));
    assert!(android("[target.android.ndk]\ncmake = \"a\"\nndk-build = \"b\"").contains("`cmake` or `ndk-build`, not both"));
    assert!(android("[target.android.ndk]\ncmake-arguments = [\"-DX=1\"]").contains("need `cmake"));
    assert!(android("[target.android.ndk]\nversion = \"r27\"").contains("`version` must be a version"));
    assert!(android("[target.android.ndk]\nunknown = 1").contains("unknown field"));
    assert!(android("[target.android.lint]\ndisable = [\"Missing Translation\"]").contains("not a lint issue id"));
    assert!(android("[target.android.lint]\nreports = [\"pdf\"]").contains("not a report format"));
    assert!(
        android("[target.android.flavors.free]\ndimension = \"t\"\nmanifest-placeholders = { \"\" = 1 }")
            .contains("not a placeholder name")
    );
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
    assert!(parse_err(&with("x = \"ftp://nope\"")).contains("maven-local"));
    assert!(parse_err(&with("x = { url = \"https://a\", username-env = \"U\" }")).contains("both"));
    assert!(parse_err(&with("x = 3")).contains("expected a URL"));
    // Duplicate of Maven Central is ignored.
    let m = parse(&with(&format!("central = \"{}/\"", kargo_maven::repo::MAVEN_CENTRAL)));
    assert_eq!(m.repositories.len(), 1);
    assert!(parse_err(&with("corp = { username-env = \"U\", password-env = \"P\" }")).contains("needs a `url`"));
    assert!(parse_err(&with("defaults = \"no\"")).contains("true or false"));
    assert!(parse_err(&with("corp = { url = \"https://a\", include-groups = [\"com.*.corp\"] }")).contains("not a group pattern"));
}

#[test]
fn repository_content_and_defaults() {
    let m = parse(&format!(
        r#"{MINIMAL}
[repositories.corp]
url = "https://nexus.corp/maven"
include-groups = ["com.corp.*"]

[repositories.maven-central]
exclude-groups = ["com.corp.*"]
"#
    ));
    let names: Vec<_> = m.repositories.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["maven-central", "corp"], "the built-in repository keeps its place");
    assert_eq!(m.repositories[0].content.exclude_groups, ["com.corp.*"]);
    assert_eq!(m.repositories[1].content.include_groups, ["com.corp.*"]);

    // Without the defaults, only the declared repositories are used, even for Compose and Android.
    let m = parse(&format!(
        "{MINIMAL}\n[frameworks]\ncompose = true\n[repositories]\ndefaults = false\nmirror = \"https://mirror.corp/maven\"\n"
    ));
    let urls: Vec<_> = m.repositories.iter().map(|r| r.url.as_str()).collect();
    assert_eq!(urls, ["https://mirror.corp/maven"]);
    // A built-in one can be added back by name.
    let m = parse(&format!("{MINIMAL}\n[repositories]\ndefaults = false\ngoogle = {{}}\n"));
    assert_eq!(m.repositories, [kargo_maven::Repository::google()]);
}

#[test]
fn publishing_settings() {
    let m = parse(MINIMAL);
    assert_eq!(m.publish, PublishSettings::default());
    assert!(m.package.publish && m.package.homepage.is_none());

    let m = parse(
        r#"
[package]
name = "lib"
version = "1.0.0"
homepage = "https://example.com/lib"
repository = "https://github.com/me/lib"
publish = false

[repositories]
internal = { url = "https://repo.example.com/releases", username-env = "U", password-env = "P" }

[publish]
repository = "internal"
sign = true
sources = false
javadoc = "empty"
central-publishing = "automatic"
kind = "bom"
relocations = ["org.old:lib"]
"#,
    );
    assert_eq!(m.package.homepage.as_deref(), Some("https://example.com/lib"));
    assert_eq!(m.package.repository.as_deref(), Some("https://github.com/me/lib"));
    assert!(!m.package.publish);
    assert_eq!(
        m.publish,
        PublishSettings {
            repository: Some("internal".into()),
            sign: Some(true),
            sources: false,
            javadoc: Javadoc::Empty,
            central_publishing: CentralPublishing::Automatic,
            kind: PublishKind::Bom,
            relocations: vec![ModuleId::new("org.old", "lib")],
            npm: None,
        }
    );
    for destination in ["maven-central", "maven-local"] {
        assert_eq!(
            parse(&format!("{MINIMAL}\n[publish]\nrepository = \"{destination}\"\n")).publish.repository.as_deref(),
            Some(destination)
        );
    }
    assert_eq!(parse(&format!("{MINIMAL}\n[publish]\njavadoc = false\n")).publish.javadoc, Javadoc::Off);

    let with = |p: &str| format!("{MINIMAL}\n[publish]\n{p}\n");
    assert!(parse_err(&with("repository = \"nowhere\"")).contains("maven-central, maven-local"));
    assert!(parse_err(&with("javadoc = \"html\"")).contains("javadoc"));
    assert!(parse_err(&with("central-publishing = \"later\"")).contains("manual"));
    assert!(parse_err(&with("kind = \"platform\"")).contains("\"library\" or \"bom\""));
    assert!(parse_err(&with("relocations = [\"a:b\", \"a:b\"]")).contains("twice"));
    assert!(parse_err(&with("checksums = []")).contains("unknown field"));
    let bad_url = MINIMAL.replace("version = \"0.1.0\"", "version = \"0.1.0\"\nrepository = \"github.com/me/lib\"");
    assert!(parse_err(&bad_url).contains("`repository` must be an http(s):// URL"));
}

#[test]
fn npm_publishing_settings() {
    assert_eq!(parse(MINIMAL).publish.npm, None);
    assert_eq!(parse(&format!("{MINIMAL}\n[publish.npm]\n")).publish.npm, Some(NpmPublishSettings::default()));

    let m = parse(&format!(
        r#"{MINIMAL}
[publish.npm]
registry = "https://npm.example.com/"
scope = "@acme"
access = "restricted"
tag = "next"
provenance = true
keywords = ["kotlin", "wasm"]
bugs = "https://example.com/bugs"
side-effects = ["./polyfill.mjs"]
engines = {{ node = ">=20" }}
"#
    ));
    assert_eq!(
        m.publish.npm,
        Some(NpmPublishSettings {
            registry: Some("https://npm.example.com".into()),
            scope: Some("acme".into()),
            access: Some(NpmAccess::Restricted),
            tag: Some("next".into()),
            provenance: true,
            keywords: vec!["kotlin".into(), "wasm".into()],
            bugs: Some("https://example.com/bugs".into()),
            side_effects: Some(NpmSideEffects::Files(vec!["./polyfill.mjs".into()])),
            engines: BTreeMap::from([("node".to_owned(), ">=20".to_owned())]),
        })
    );
    let flag = parse(&format!("{MINIMAL}\n[publish.npm]\nside-effects = false\n"));
    assert_eq!(flag.publish.npm.unwrap().side_effects, Some(NpmSideEffects::Flag(false)));

    let with = |p: &str| format!("{MINIMAL}\n[publish.npm]\n{p}\n");
    assert!(parse_err(&with("registry = \"npm.example.com\"")).contains("`registry` must be an http(s):// URL"));
    assert!(parse_err(&with("scope = \"Acme Corp\"")).contains("`scope`"));
    assert!(parse_err(&with("access = \"private\"")).contains("\"public\" or \"restricted\""));
    assert!(parse_err(&with("tag = \"my tag\"")).contains("dist-tag"));
    assert!(parse_err(&with("side-effects = \"yes\"")).contains("side-effects"));
    assert!(parse_err(&with("engines = { node = \"\" }")).contains("`engines`"));
    assert!(parse_err(&with("token = \"abc\"")).contains("unknown field"));
}

#[test]
fn maven_local_repository() {
    let m = parse(&format!("{MINIMAL}\n[repositories]\nlocal = \"maven-local\"\n"));
    let local = m.repositories.iter().find(|r| r.name == "local").expect("the local repository is added");
    assert!(local.url.replace('\\', "/").ends_with(".m2/repository") || std::env::var_os(kargo_maven::repo::MAVEN_LOCAL_ENV).is_some());
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
    assert!(parse_err(&with("compose = { colour = 1 }")).contains("unknown field `colour`"));
    assert!(parse_err(&with("compose = 3")).contains("expected true, false or a table"));
    let client = parse_err(&with("ktor = { client = true }"));
    assert!(
        client.contains("`client` is the name of an engine") && client.contains("\"okhttp\"") && client.contains("boolean"),
        "{client}"
    );
    assert!(parse_err(&with("ktor = { server = false }")).contains("`server` is the name of an engine"));
    let engines = parse(&with("ktor = { client = \"cio\", client-engines = { native = \"darwin\", wasm = \"none\" } }"));
    assert_eq!(engines.frameworks.ktor.unwrap().client_engines["native"], "darwin");
    assert!(parse_err(&with("ktor = { server = \"netty\", client-engines = { jvm = \"cio\" } }")).contains("set `client"));
    assert!(parse_err(&with("ktor = { client = \"cio\", client-engines = { ios = \"darwin\" } }")).contains("unknown target `ios`"));
    assert!(parse_err(&with("ktor = { client = \"cio\", client-engines = { jvm = \"\" } }")).contains("`client-engines.jvm` is empty"));
    let options = parse(&with(
        "compose = { stability-config = [\"a.conf\", \"b.conf\"], strong-skipping = false, feature-flags = [\"-IntrinsicRemember\"], metrics = true }",
    ));
    let compose = options.frameworks.compose.as_ref().unwrap();
    assert_eq!(compose.stability_config, ["a.conf", "b.conf"]);
    assert_eq!((compose.strong_skipping, compose.metrics, compose.reports), (Some(false), Some(true), None));
    assert_eq!(compose.feature_flags, ["-IntrinsicRemember"]);
    let one = parse(&with("compose = { stability-config = \"stability.conf\" }"));
    assert_eq!(one.frameworks.compose.as_ref().unwrap().stability_config, ["stability.conf"]);
    assert!(parse_err(&with("compose = { feature-flags = [\"Turbo\"] }")).contains("unknown flag `Turbo`"));
    assert!(
        parse_err(&with("compose = { strong-skipping = true, feature-flags = [\"-StrongSkipping\"] }"))
            .contains("keep only `strong-skipping`")
    );
    let html = parse(&with("compose = { html = true, svg = true }"));
    let compose = html.frameworks.compose.as_ref().unwrap();
    assert_eq!((compose.html, compose.svg), (Some(true), Some(true)));
    assert!(parse_err(&with("compose = { svg = true }")).contains("set `html = true` too"));
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
fn tasks_have_their_own_env_and_env_files() {
    let var = |value: &str| EnvVar { value: Some(value.to_owned()), ..Default::default() };
    let m = parse(&format!(
        "{MINIMAL}\n[tasks.frontend]\nrun = \"npm run dev\"\nenv-files = [\".env.frontend\"]\n[tasks.frontend.env]\nAPI_URL = \"https://staging.example.com\"\nPORT = 8080\nFLAG = true\nDATABASE_URL = {{ required = true }}\nCONFIG_DIR = {{ value = \"config\", relative = true }}\nFORCED = {{ value = \"kargo\", force = true }}\n"
    ));
    let frontend = &m.tasks["frontend"];
    assert_eq!(frontend.env.files, [".env.frontend"]);
    assert_eq!(frontend.env.vars["API_URL"], var("https://staging.example.com"));
    assert_eq!(frontend.env.vars["PORT"], var("8080"));
    assert_eq!(frontend.env.vars["FLAG"], var("true"));
    assert_eq!(frontend.env.vars["DATABASE_URL"], EnvVar { required: true, ..Default::default() });
    assert_eq!(frontend.env.vars["CONFIG_DIR"], EnvVar { relative: true, ..var("config") });
    assert_eq!(frontend.env.vars["FORCED"], EnvVar { force: true, ..var("kargo") });
    assert!(frontend.env.expose.is_empty());

    // The inline form holds the same keys.
    let m = parse(&format!(
        "{MINIMAL}\n[tasks]\nbackend = {{ run = \"echo\", env-files = [\".env.backend\"], env = {{ API_URL = \"https://api.example.com\" }} }}\n"
    ));
    assert_eq!(m.tasks["backend"].env.files, [".env.backend"]);
    assert_eq!(m.tasks["backend"].env.vars["API_URL"], var("https://api.example.com"));

    // A variable named `run` inside `env` is fine: it is a variable, not the task's command.
    let m = parse(&format!("{MINIMAL}\n[tasks]\na = {{ run = \"echo\", env = {{ run = \"x\" }} }}\n"));
    assert_eq!(m.tasks["a"].env.vars["run"], var("x"));

    let with = |t: &str| format!("{MINIMAL}\n[tasks]\n{t}\n");
    assert!(parse_err(&with("a = { run = \"x\", env = { \"1BAD\" = \"y\" } }")).contains("invalid variable name"));
    assert!(parse_err(&with("a = { run = \"x\", env = { B = {} } }")).contains("needs a `value`"));
    assert!(
        parse_err(&with("a = { run = \"x\", env = { B = { value = \"v\", required = true } } }")).contains("both `value` and `required`")
    );
    assert!(parse_err(&with("a = { run = \"x\", env-files = [\"\"] }")).contains("`env-files` must be an array"));
    assert!(parse_err(&with("a = { run = \"x\", env-files = \"x\" }")).contains("invalid task `a`"));
    assert!(
        parse_err(&format!("{MINIMAL}\n[tasks.a]\nrun = \"echo\"\n[tasks.a.env]\nexpose = [\"A\"]\n"))
            .contains("`expose` belongs in [env]")
    );
    assert!(parse_err(&format!("{MINIMAL}\n[tasks.a]\nrun = \"echo\"\n[tasks.a.env]\nenv-files = [\"x\"]\n")).contains("not in its `env`"));
}

#[test]
fn tasks_and_plugins_can_run_kotlin_scripts() {
    let m = parse(&format!(
        "{MINIMAL}
[tasks]
gen = {{ script = \"scripts/gen.kts\", hooks = [\"pre-build\"], env = {{ CHANNEL = \"releases\" }} }}
notes = {{ script = \"scripts/notes.kt\", depends-on = [\"gen\"] }}
ci = {{ depends-on = [\"gen\"] }}

[plugins.notify]
script = \"scripts/notify.kts\"
args = [\"--quiet\"]
hooks = [\"post-build\"]
"
    ));
    assert_eq!(m.tasks["gen"].script.as_deref(), Some(Path::new("scripts/gen.kts")));
    assert!(m.tasks["gen"].run.is_empty());
    assert_eq!(m.tasks["gen"].summary(), "kargo script scripts/gen.kts");
    assert_eq!(m.tasks["notes"].script.as_deref(), Some(Path::new("scripts/notes.kt")));
    assert_eq!(m.tasks["ci"].script, None);
    let notify = &m.process_plugins[0];
    assert_eq!((notify.script.as_deref(), notify.command.as_str()), (Some(Path::new("scripts/notify.kts")), ""));
    assert_eq!(notify.platform_args(), ["--quiet"]);
    assert_eq!(notify.program(), Path::new("scripts/notify.kts").display().to_string());
}

#[test]
fn script_errors() {
    let task = |t: &str| parse_err(&format!("{MINIMAL}\n[tasks]\n{t}\n"));
    let plugin = |p: &str| parse_err(&format!("{MINIMAL}\n[plugins.x]\nhooks = [\"post-build\"]\n{p}\n"));
    assert!(task("a = { script = \"a.kts\", run = \"echo\" }").contains("`script` runs a Kotlin script, so it cannot go with `run`"));
    assert!(task("a = { script = \"a.kts\", windows-run = \"echo\" }").contains("`windows-run`"));
    assert!(task("a = { script = \"a.kts\", shell = \"sh\" }").contains("`shell`"));
    assert!(task("a = { script = \"a.sh\" }").contains("must be the path of a .kts script or a .kt file, not `a.sh`"));
    assert!(task("a = { script = \"\" }").contains("`script` must be"));
    assert!(task("a = { script = 1 }").contains("invalid task `a`"));

    assert!(plugin("script = \"a.kts\"\ncommand = \"c\"").contains("either `command` or `script`"));
    assert!(plugin("script = \"a.kts\"\nwindows-command = \"c\"").contains("`windows-command` goes with `command`"));
    assert!(plugin("script = \"a.kts\"\nmaven = \"a:b:1\"").contains("not both"));
    assert!(plugin("script = \"a.sh\"").contains("must be the path of a .kts script"));
    assert!(plugin("script = \"a.kts\"\nid = \"z\"").contains("only apply to `maven`"));
    let no_hook = parse_err(&format!("{MINIMAL}\n[plugins.x]\nscript = \"a.kts\"\n"));
    assert!(no_hook.contains("a `script` plugin needs at least one hook"), "{no_hook}");
}

#[test]
fn an_inherited_script_stays_the_workspaces_file() {
    let ws = WorkspaceManifest::parse(
        "[workspace]
members = [\"app\"]

[workspace.plugins]
notify = { script = \"tools/notify.kts\", hooks = [\"post-build\"] }

[workspace.tasks]
codegen = { script = \"tools/gen.kts\", depends-on = [\"prepare\"] }
prepare = { script = \"tools/prepare.kts\" }
",
        Path::new("/ws"),
    )
    .unwrap();
    let m = Manifest::parse_in(
        "[package]
name = \"app\"
version = \"1.0.0\"

[plugins]
notify.workspace = true

[tasks]
codegen.workspace = true
",
        Some(&ws),
    )
    .unwrap();
    assert_eq!(m.process_plugins[0].script.as_deref(), Some(Path::new("/ws").join("tools/notify.kts").as_path()));
    assert_eq!(m.tasks["codegen"].script.as_deref(), Some(Path::new("/ws").join("tools/gen.kts").as_path()));
    assert_eq!(m.tasks["prepare"].script.as_deref(), Some(Path::new("/ws").join("tools/prepare.kts").as_path()), "a dependency too");
}

#[test]
fn version_validation() {
    assert!(
        parse_err(
            "[package]
name = \"a\"
version = \"\"
"
        )
        .contains("[package] `version`")
    );
    assert!(
        parse_err(
            "[package]
name = \"a\"
version = \"1\"
group = \"com..x\"
"
        )
        .contains("group")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[kotlin]
version = \"(\"
"
        ))
        .contains("[kotlin] `version`")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[kotlin]
ksp = \"(\"
"
        ))
        .contains("[kotlin] `ksp`")
    );
}

#[test]
fn versions_of_the_project_must_be_exact() {
    for bad in ["[1.0,2.0)", "2.+", "1.0 ", "", "(1.0,)", "1.*"] {
        let package = parse_err(&format!(
            "[package]
name = \"a\"
version = \"{bad}\"
"
        ));
        assert!(package.contains("[package] `version` must be one exact version"), "{bad}: {package}");
        let kotlin = parse_err(&format!(
            "{MINIMAL}
[kotlin]
version = \"{bad}\"
"
        ));
        assert!(kotlin.contains("[kotlin] `version` must be one exact version"), "{bad}: {kotlin}");
        let ksp = parse_err(&format!(
            "{MINIMAL}
[kotlin]
ksp = \"{bad}\"
"
        ));
        assert!(ksp.contains("[kotlin] `ksp` must be one exact version"), "{bad}: {ksp}");
        let coroutines = parse_err(&format!(
            "{MINIMAL}
[frameworks]
coroutines = \"{bad}\"
"
        ));
        assert!(coroutines.contains("[frameworks] `coroutines` must be one exact version"), "{bad}: {coroutines}");
        let ktor = parse_err(&format!(
            "{MINIMAL}
[frameworks.ktor]
version = \"{bad}\"
"
        ));
        assert!(ktor.contains("[frameworks.ktor] `version` must be one exact version"), "{bad}: {ktor}");
    }
    let build = parse(
        "[package]
name = \"a\"
version = \"1.0.0+build.5\"
",
    );
    assert_eq!(build.package.version, "1.0.0+build.5");
    let err = parse_err(&format!(
        "{MINIMAL}
[kotlin]
version = \"[2.0,3.0)\"
"
    ));
    assert!(err.contains("such as \"") && err.contains("got `[2.0,3.0)`"), "{err}");
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
    assert_eq!("js".parse::<TargetKind>().unwrap(), TargetKind::Js);
    assert_eq!(TargetKind::Js.source_dir_name(), "src-js");
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
fn toolchain_android_sdk_requests() {
    let with =
        |toolchain: &str, android: &str| format!("{MINIMAL}\n[toolchain]\n{toolchain}\n[target.android]\nnamespace = \"a.b\"\n{android}\n");
    let sdk = |toolchain: &str, android: &str| {
        let m = parse(&with(toolchain, android));
        let cfg = m.targets.android.unwrap().config;
        (m.toolchain.android_sdk, cfg.compile_sdk, cfg.target_sdk)
    };
    assert_eq!(parse(MINIMAL).toolchain.android_sdk, AndroidSdkRequest::System);
    assert_eq!(sdk("", ""), (AndroidSdkRequest::System, 37, 36));
    assert_eq!(sdk("android-sdk = \"system\"", "compile-sdk = 35\ntarget-sdk = 34"), (AndroidSdkRequest::System, 35, 34));
    // compile-sdk follows the toolchain, and the default target-sdk never exceeds it.
    assert_eq!(sdk("android-sdk = \"36\"", ""), (AndroidSdkRequest::Platform(36), 36, 36));
    assert_eq!(sdk("android-sdk = 35", ""), (AndroidSdkRequest::Platform(35), 35, 35));
    assert_eq!(sdk("android-sdk = \"37\"", "compile-sdk = 37"), (AndroidSdkRequest::Platform(37), 37, 36));
    let err = parse_err(&with("android-sdk = \"36\"", "compile-sdk = 37"));
    assert!(err.contains("`compile-sdk = 37` differs from `android-sdk = \"36\"`"), "{err}");
    let err = parse_err(&with("android-sdk = \"latest\"", ""));
    assert!(err.contains("expected `system` or a platform API level"), "{err}");
    assert!(parse_err(&with("android-sdk = 19", "")).contains("not supported; use 21 or newer"));
    assert!(parse_err(&with("android-sdk = true", "")).contains("android-sdk"));
    // Without the Android target it is only a preference.
    assert_eq!(parse(&format!("{MINIMAL}\n[toolchain]\nandroid-sdk = \"36\"\n")).toolchain.android_sdk, AndroidSdkRequest::Platform(36));
    assert_eq!(AndroidSdkRequest::Platform(36).to_string(), "36");
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
    assert_eq!(ws.members, ["app", "libs/core"]);
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
    // A new workspace has no members yet.
    assert!(WorkspaceManifest::parse("[workspace]\nmembers = []\n", root).unwrap().member_dirs().unwrap().is_empty());
    assert!(ws_err("[workspace]\nmembers = [\"libs/**\"]\n").contains("globs such as \"libs/*\""));
    assert!(ws_err("[workspace]\nmembers = [\"/abs\"]\n").contains("relative to the workspace root"));
    assert!(ws_err("[workspace]\nmembers = [\"a\", \"a\"]\n").contains("more than once"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\nexclude = [\"b/*\"]\n").contains("`exclude` entries must be directories"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\ndefault-members = [\"*\"]\n").contains("`default-members` entries"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\nresolver = \"2\"\n").contains("resolver"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.package]\nname = \"x\"\n").contains("[workspace.package]"));
    assert!(
        ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.package]\nversion = { workspace = true }\n").contains("only valid in a member")
    );
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.frameworks]\nspring = true\n").contains("[workspace.frameworks]"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.tasks]\nx = { depends-on = [\"y\"] }\n").contains("unknown task `y`"));
    assert!(ws_err("[workspace]\nmembers = [\"a\"]\n[workspace.kotlin]\njvm-target = \"banana\"\n").contains("jvm-target"));
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
    // `exported` and `test-fixtures` may be added to an inherited entry.
    let m = member("core = { workspace = true, exported = true }").unwrap();
    assert!(m.path_dependencies[0].exported);
}

#[test]
fn workspace_members_globs_and_exclude() {
    let dir = tempfile::tempdir().unwrap();
    for member in ["apps/web", "apps/cli", "libs/core", "libs/old", "libs/.hidden", "libs/nomanifest"] {
        std::fs::create_dir_all(dir.path().join(member)).unwrap();
        if member != "libs/nomanifest" {
            std::fs::write(dir.path().join(member).join(MANIFEST_NAME), "").unwrap();
        }
    }
    let ws = WorkspaceManifest::parse(
        "[workspace]\nmembers = [\"libs/*\", \"apps/*\", \"./libs/core\"]\nexclude = [\"libs/old\"]\ndefault-members = [\"apps/web\"]\n",
        dir.path(),
    )
    .unwrap();
    let dirs: Vec<String> = ws.member_dirs().unwrap().iter().map(|d| d.to_string_lossy().replace('\\', "/")).collect();
    // Each glob's matches are sorted; `./libs/core` is already listed; hidden, excluded and
    // manifest-less directories are left out.
    assert_eq!(dirs, ["libs/core", "apps/cli", "apps/web"]);
    assert_eq!(ws.default_members, [PathBuf::from("apps/web")]);
    assert!(ws.lists_member(Path::new("apps/new")));
    assert!(ws.lists_member(Path::new("libs/core")));
    assert!(!ws.lists_member(Path::new("libs/old")));
    assert!(!ws.lists_member(Path::new("libs/core/nested")));

    let none = WorkspaceManifest::parse("[workspace]\nmembers = [\"nothing/*\"]\n", dir.path()).unwrap();
    assert!(format!("{:#}", none.member_dirs().unwrap_err()).contains("matches no package"));
}

#[test]
fn members_inherit_from_the_workspace() {
    let ws = WorkspaceManifest::parse(
        "[workspace]
members = [\"app\"]

[workspace.package]
version = \"2.1.0\"
group = \"com.example\"
authors = [\"Ada\"]
license = \"MIT\"

[workspace.kotlin]
opt-in = [\"kotlin.RequiresOptIn\"]
compiler-args = [\"-Xcontext-parameters\"]
warnings-as-errors = true
jvm-target = \"17\"
language-version = \"2.2\"

[workspace.frameworks]
serialization = { formats = [\"json\", \"cbor\"] }
coroutines = \"1.10.2\"

[workspace.plugins]
notify = { command = \"tools/notify.sh\", hooks = [\"post-build\"] }

[workspace.tasks]
codegen = { run = \"gen\", depends-on = [\"prepare\"] }
prepare = \"mkdir -p gen\"
release = \"echo release\"
",
        Path::new("/ws"),
    )
    .unwrap();
    assert_eq!(ws.tasks.keys().collect::<Vec<_>>(), ["codegen", "prepare", "release"]);
    let member = |text: &str| Manifest::parse_in(text, Some(&ws));
    let m = member(
        "[package]
name = \"app\"
version.workspace = true
group = { workspace = true }
authors.workspace = true
description = \"Own description\"

[kotlin]
opt-in = [\"kotlinx.coroutines.ExperimentalCoroutinesApi\", \"kotlin.RequiresOptIn\"]
compiler-args = [\"-Xjsr305=strict\"]

[frameworks]
serialization.workspace = true
coroutines = { workspace = true }

[plugins]
notify.workspace = true

[tasks]
codegen.workspace = true
own = \"echo own\"
",
    )
    .unwrap();
    assert_eq!(m.package.version, "2.1.0");
    assert_eq!(m.package.group.as_deref(), Some("com.example"));
    assert_eq!(m.package.authors, ["Ada"]);
    assert_eq!(m.package.license, None, "only the keys a member inherits");
    assert_eq!(m.package.description.as_deref(), Some("Own description"));
    assert_eq!(m.kotlin.opt_in, ["kotlin.RequiresOptIn", "kotlinx.coroutines.ExperimentalCoroutinesApi"]);
    assert_eq!(m.kotlin.compiler_args, ["-Xcontext-parameters", "-Xjsr305=strict"]);
    assert!(m.kotlin.warnings_as_errors);
    assert_eq!(m.kotlin.language_version.as_deref(), Some("2.2"));
    let jvm = m.targets.jvm.as_ref().unwrap();
    assert_eq!(jvm.jvm_target, "17");
    assert!(jvm.jvm_target_set);
    assert_eq!(m.frameworks.serialization.as_ref().unwrap().formats.as_deref(), Some(&["json".to_owned(), "cbor".to_owned()][..]));
    assert_eq!(m.frameworks.coroutines.as_ref().unwrap().version.as_deref(), Some("1.10.2"));
    // A workspace plugin's command stays relative to the workspace.
    assert_eq!(PathBuf::from(&m.process_plugins[0].command), Path::new("/ws").join("tools/notify.sh"));
    // An inherited task brings the workspace tasks it depends on.
    assert_eq!(m.tasks.keys().collect::<Vec<_>>(), ["codegen", "own", "prepare"]);

    // A member's own `[target.jvm] jvm-target` wins.
    let own = member("[package]\nname = \"app\"\nversion = \"1.0.0\"\n[target.jvm]\njvm-target = \"21\"\n").unwrap();
    assert_eq!(own.targets.jvm.unwrap().jvm_target, "21");

    let err = |text: &str| format!("{:#}", member(&format!("[package]\nname = \"app\"\nversion = \"1.0.0\"\n{text}")).unwrap_err());
    assert!(err("[frameworks]\nktor.workspace = true\n").contains("[workspace.frameworks] has no `ktor`"));
    assert!(err("[tasks]\nx = { workspace = true, run = \"y\" }\n").contains("and nothing else"));
    let outside = Manifest::parse("[package]\nname = \"app\"\nversion.workspace = true\n").unwrap_err();
    assert!(format!("{outside:#}").contains("needs a [workspace]"));
    let licensed = member("[package]\nname = \"app\"\nversion = \"1.0.0\"\nlicense.workspace = true\n").unwrap();
    assert_eq!(licensed.package.license.as_deref(), Some("MIT"));
    let homepage = member("[package]\nname = \"app\"\nversion = \"1.0.0\"\nhomepage.workspace = true\n").unwrap_err();
    assert!(format!("{homepage:#}").contains("[workspace.package] has no `homepage`"));
}

#[test]
fn test_section() {
    assert!(parse(MINIMAL).test.fail_fast, "targets stop at the first failure by default");
    assert!(!parse(&format!("{MINIMAL}\n[test]\nfail-fast = false\n")).test.fail_fast);
    assert!(parse_err(&format!("{MINIMAL}\n[test]\nfail-fast = \"no\"\n")).contains("fail-fast"));
    assert!(parse_err(&format!("{MINIMAL}\n[test]\nthreads = 2\n")).contains("threads"));
    let limits = parse(&format!("{MINIMAL}\n[test]\ntimeout = \"5m\"\ntest-timeout = \"30 s\"\njvm-args = [\"-Xmx512m\"]\n")).test;
    assert_eq!(limits.timeout, Some(Duration::from_secs(300)));
    assert_eq!(limits.test_timeout, Some(Duration::from_secs(30)));
    assert_eq!(limits.jvm_args, ["-Xmx512m"]);
    assert_eq!(parse(MINIMAL).test, TestSettings::default(), "no limits are set by default");
    assert_eq!(parse(&format!("{MINIMAL}\n[test]\ntimeout = \"500ms\"\n")).test.timeout, Some(Duration::from_millis(500)));
    assert_eq!(parse(&format!("{MINIMAL}\n[test]\ntimeout = \"1h\"\n")).test.timeout, Some(Duration::from_secs(3600)));
    for bad in ["0s", "5", "1.5s", "10 parsecs", "", "-1m"] {
        let err = parse_err(&format!("{MINIMAL}\n[test]\ntimeout = \"{bad}\"\n"));
        assert!(err.contains("[test] `timeout` must be a positive duration"), "{bad}: {err}");
    }
    assert!(parse_err(&format!("{MINIMAL}\n[test]\ntest-timeout = \"soon\"\n")).contains("`test-timeout`"));
}

#[test]
fn test_coverage_section() {
    assert_eq!(parse(MINIMAL).test.coverage, CoverageSettings::default(), "no coverage thresholds by default");
    let coverage = parse(&format!(
        "{MINIMAL}\n[test.coverage]\njacoco = \"0.8.12\"\nmin-line = 80\nmin-branch = 70\nfail-under = 75.5\nexclude = [\"**/generated/**\"]\n"
    ))
    .test
    .coverage;
    assert_eq!(coverage.jacoco_version.as_deref(), Some("0.8.12"));
    assert_eq!(coverage.min_line, Some(80.0));
    assert_eq!(coverage.min_branch, Some(70.0));
    assert_eq!(coverage.fail_under, Some(75.5));
    assert_eq!(coverage.exclude, ["**/generated/**"]);
    assert_eq!(coverage.line_threshold(None), Some(75.5), "fail-under wins over min-line");
    assert_eq!(coverage.line_threshold(Some(90.0)), Some(90.0), "--fail-under wins over the manifest");
    let bare = parse(&format!("{MINIMAL}\n[test.coverage]\nmin-line = 80\n")).test.coverage;
    assert_eq!(bare.line_threshold(None), Some(80.0));
    assert_eq!(CoverageSettings::default().line_threshold(None), None);
    for bad in ["min-line = 101", "min-line = -1", "min-branch = 200", "fail-under = -0.5"] {
        let err = parse_err(&format!("{MINIMAL}\n[test.coverage]\n{bad}\n"));
        assert!(err.contains("is not a percentage"), "{bad}: {err}");
    }
    assert!(parse_err(&format!("{MINIMAL}\n[test.coverage]\nexclude = [\"\"]\n")).contains("empty pattern"));
    assert!(parse_err(&format!("{MINIMAL}\n[test.coverage]\nthreads = 2\n")).contains("threads"));
}

#[test]
fn test_suites() {
    assert!(parse(MINIMAL).test.suites.is_empty(), "no suites by default");
    let manifest = format!(
        "{MINIMAL}
[test.suites.integration]
sources = \"integration-tests\"
resources = \"integration-resources\"
jvm-args = [\"-Xmx2g\"]
cwd = \"integration\"
timeout = \"5m\"
test-timeout = \"30s\"
include-tags = [\"integration\"]
exclude-tags = [\"slow\"]
env = {{ DATABASE_URL = \"jdbc:h2:mem:test\" }}

[test.suites.integration.dependencies]
\"org.testcontainers:testcontainers\" = \"1.20.0\"
"
    );
    let suite = &parse(&manifest).test.suites["integration"];
    assert_eq!(suite.sources, PathBuf::from("integration-tests"));
    assert_eq!(suite.resources, PathBuf::from("integration-resources"));
    assert_eq!(suite.jvm_args, ["-Xmx2g"]);
    assert_eq!(suite.cwd.as_deref(), Some(Path::new("integration")));
    assert_eq!(suite.timeout, Some(Duration::from_secs(300)));
    assert_eq!(suite.test_timeout, Some(Duration::from_secs(30)));
    assert_eq!(suite.include_tags, ["integration"]);
    assert_eq!(suite.exclude_tags, ["slow"]);
    assert_eq!(suite.env["DATABASE_URL"], "jdbc:h2:mem:test");
    assert_eq!(suite.dependencies.len(), 1);
    assert_eq!(suite.dependencies[0].module.to_string(), "org.testcontainers:testcontainers");

    // Defaults: `<name>-tests` and `<name>-resources`.
    let suite = &parse(&format!("{MINIMAL}\n[test.suites.integration]\n")).test.suites["integration"];
    assert_eq!(suite.sources, PathBuf::from("integration-tests"));
    assert_eq!(suite.resources, PathBuf::from("integration-resources"));
    assert!(suite.jvm_args.is_empty() && suite.cwd.is_none() && suite.timeout.is_none());

    // Reserved names, clashes and bad directories are rejected.
    for (body, what) in [
        ("[test.suites.unit]\n", "`unit`"),
        ("[test.suites.all]\n", "`all`"),
        ("[test.suites.jvm]\n", "clashes with the source directories"),
        ("[test.suites.integration]\nsources = \"tests\"\n", "`tests`"),
        ("[test.suites.integration]\nsources = \"tests-jvm\"\n", "`tests-jvm`"),
        ("[test.suites.integration]\nsources = \"/abs\"\n", "inside the package"),
        ("[test.suites.integration]\nresources = \"test-resources\"\n", "`test-resources`"),
        ("[test.suites.integration]\ncwd = \"/abs\"\n", "`cwd`"),
        ("[test.suites.integration]\ninclude-tags = [\"\"]\n", "empty tag"),
        ("[test.suites.integration]\nenv = { \"1BAD\" = \"x\" }\n", "not a valid environment variable"),
        ("[test.suites.a]\nsources = \"same\"\n[test.suites.b]\nsources = \"same\"\n", "also used by"),
    ] {
        let err = parse_err(&format!("{MINIMAL}\n{body}"));
        assert!(err.contains(what), "{body}: {err}");
    }
    // Unknown keys are still errors.
    assert!(parse_err(&format!("{MINIMAL}\n[test.suites.integration]\nthreads = 2\n")).contains("threads"));
}

#[test]
fn audit_section() {
    assert_eq!(parse(MINIMAL).audit, AuditSettings::default(), "nothing is set by default");
    let audit = parse(&format!(
        "{MINIMAL}
[audit]
deny = \"medium\"
ignore = [
    {{ id = \"GHSA-aaaa-bbbb-cccc\", reason = \"we never parse untrusted YAML\" }},
    {{ id = \"CVE-2024-1234\", reason = \"fixed upstream soon\", until = 2026-12-31 }},
    {{ id = \"CVE-2024-5678\", reason = \"r\", until = \"2027-01-15\" }},
]
"
    ))
    .audit;
    assert_eq!(audit.deny, Some(DenyLevel::Medium));
    assert_eq!(audit.ignore.len(), 3);
    assert_eq!(audit.ignore[0].reason, "we never parse untrusted YAML");
    assert_eq!(audit.ignore[1].until.as_deref(), Some("2026-12-31"));
    assert_eq!(audit.ignore[2].until.as_deref(), Some("2027-01-15"));

    let audit = |body: &str| {
        parse_err(&format!(
            "{MINIMAL}
[audit]
{body}
"
        ))
    };
    assert!(audit("deny = \"severe\"").contains("unknown severity `severe`"));
    assert!(audit("ignore = [{ id = \"GHSA-x\" }]").contains("`GHSA-x` needs a `reason`"));
    assert!(audit("ignore = [{ id = \"GHSA-x\", reason = \"  \" }]").contains("needs a `reason`"));
    assert!(audit("ignore = [{ id = \"GHSA-x\", reason = \"r\", until = \"next year\" }]").contains("use a date"));
    assert!(audit("ignore = [{ id = \"GHSA-x\", reason = \"r\", until = 2026-12-31T10:00:00Z }]").contains("use a date"));
    assert!(audit("ignore = [\"GHSA-x\"]").contains("ignore"));
    assert!(audit("fix = true").contains("unknown field"));
}

#[test]
fn deny_levels() {
    assert!(DenyLevel::High.denies(Some(Severity::Critical)));
    assert!(DenyLevel::High.denies(Some(Severity::High)));
    assert!(!DenyLevel::High.denies(Some(Severity::Medium)));
    assert!(DenyLevel::Critical.denies(None), "an advisory without a severity is not taken as harmless");
    assert!(!DenyLevel::None.denies(None));
    assert!(!DenyLevel::None.denies(Some(Severity::Critical)));
    assert_eq!(Severity::from_score(9.8), Some(Severity::Critical));
    assert_eq!(Severity::from_score(7.0), Some(Severity::High));
    assert_eq!(Severity::from_score(6.9), Some(Severity::Medium));
    assert_eq!(Severity::from_score(0.1), Some(Severity::Low));
    assert_eq!(Severity::from_score(0.0), None);
}

#[test]
fn bench_section() {
    assert_eq!(parse(MINIMAL).bench, BenchSettings::default(), "nothing is set by default");
    let bench = parse(&format!(
        "{MINIMAL}
[bench]
warmup = \"3x500ms\"
iterations = \"5x2s\"
forks = 2
mode = \"avgt\"
time-unit = \"us\"
jvm-args = [\"-Xmx1g\"]
"
    ))
    .bench;
    assert_eq!(bench.warmup, Some(BenchRounds { count: 3, time: Duration::from_millis(500) }));
    assert_eq!(bench.iterations, Some(BenchRounds { count: 5, time: Duration::from_secs(2) }));
    assert_eq!(bench.forks, Some(2));
    assert_eq!(bench.mode, Some(BenchMode::AverageTime));
    assert_eq!(bench.time_unit.as_deref(), Some("us"));
    assert_eq!(bench.jvm_args, ["-Xmx1g"]);
    assert_eq!(
        parse(&format!(
            "{MINIMAL}
[bench]
forks = 0
"
        ))
        .bench
        .forks,
        Some(0),
        "no forks runs in the launcher's JVM"
    );

    for bad in ["5", "x1s", "0x1s", "5x", "5x0s", "five x 1s", "5x1 parsec", ""] {
        let err = parse_err(&format!(
            "{MINIMAL}
[bench]
warmup = \"{bad}\"
"
        ));
        assert!(err.contains("[bench] `warmup` must be a count and a duration"), "{bad}: {err}");
    }
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[bench]
iterations = \"soon\"
"
        ))
        .contains("`iterations`")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[bench]
mode = \"fast\"
"
        ))
        .contains("unknown benchmark mode `fast`")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[bench]
time-unit = \"years\"
"
        ))
        .contains("[bench] `time-unit` must be one of")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[bench]
forks = -1
"
        ))
        .contains("forks")
    );
    assert!(
        parse_err(&format!(
            "{MINIMAL}
[bench]
threads = 2
"
        ))
        .contains("threads")
    );

    for mode in BenchMode::ALL {
        assert_eq!(mode.as_str().parse::<BenchMode>().unwrap(), mode);
    }
    assert_eq!(BenchRounds { count: 5, time: Duration::from_secs(60) }.to_string(), "5x1m");
    assert_eq!(BenchRounds { count: 2, time: Duration::from_millis(1500) }.to_string(), "2x1500ms");
}

#[test]
fn benchmark_framework() {
    assert!(parse(MINIMAL).frameworks.benchmark.is_none());
    assert_eq!(
        parse(&format!(
            "{MINIMAL}
[frameworks]
benchmark = true
"
        ))
        .frameworks
        .benchmark,
        Some(BenchmarkConfig::default())
    );
    let pinned = parse(&format!(
        "{MINIMAL}
[frameworks]
benchmark = {{ version = \"0.4.14\" }}
"
    ))
    .frameworks
    .benchmark
    .unwrap();
    assert_eq!(pinned.version.as_deref(), Some("0.4.14"));
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

#[test]
fn a_module_can_be_declared_once_per_classifier_and_extension() {
    let deps = |body: &str| parse(&format!("{MINIMAL}\n[dependencies]\n{body}\n")).dependencies;

    // Native libraries are declared as the module plus its classifier jar.
    let found = deps(
        "\"org.lwjgl:lwjgl\" = \"3.3.6\"\n\
         lwjgl-natives = { coords = \"org.lwjgl:lwjgl:3.3.6\", classifier = \"natives-windows\", scope = \"runtime\" }\n\
         lwjgl-linux = { coords = \"org.lwjgl:lwjgl:3.3.6\", classifier = \"natives-linux\", scope = \"runtime\" }\n\
         lwjgl-zip = { coords = \"org.lwjgl:lwjgl:3.3.6\", classifier = \"natives-linux\", extension = \"zip\" }",
    );
    assert_eq!(found.len(), 4);
    assert!(!found[0].same_artifact(&found[1]) && !found[1].same_artifact(&found[2]) && !found[2].same_artifact(&found[3]));
    assert!(found[1].same_artifact(&found[1]));

    let twice = parse_err(&format!(
        "{MINIMAL}\n[dependencies]\n\"g:a\" = {{ version = \"1\", classifier = \"x\", extension = \"zip\" }}\n\
         other = {{ coords = \"g:a:1\", classifier = \"x\", extension = \"zip\" }}\n"
    ));
    assert!(twice.contains("declared more than once"), "{twice}");
}

#[test]
fn env_table_accepts_strings_and_detailed_variables() {
    let m = parse(&format!(
        "{MINIMAL}\n[env]\nAPP_NAME = \"demo\"\nPORT = 8080\nDEBUG = true\n\
         LOG_LEVEL = {{ value = \"debug\", force = true }}\nCONFIG_DIR = {{ value = \"config\", relative = true }}\n\
         DATABASE_URL = {{ required = true }}\n\n[profile.release.env]\nLOG_LEVEL = \"info\"\n"
    ));
    let var = |value: &str| EnvVar { value: Some(value.to_owned()), ..Default::default() };
    assert_eq!(m.env.vars["APP_NAME"], var("demo"));
    assert_eq!(m.env.vars["PORT"], var("8080"));
    assert_eq!(m.env.vars["DEBUG"], var("true"));
    assert_eq!(m.env.vars["LOG_LEVEL"], EnvVar { force: true, ..var("debug") });
    assert_eq!(m.env.vars["CONFIG_DIR"], EnvVar { relative: true, ..var("config") });
    assert_eq!(m.env.vars["DATABASE_URL"], EnvVar { required: true, ..Default::default() });
    assert_eq!(m.profiles["release"].env.vars["LOG_LEVEL"], var("info"));
    assert!(m.profiles["dev"].env.vars.is_empty());
}

#[test]
fn env_rejects_invalid_variables() {
    let base = |env: &str| format!("{MINIMAL}\n[env]\n{env}\n");
    assert!(parse_err(&base("\"bad-name\" = \"x\"")).contains("invalid variable name `bad-name`"));
    assert!(parse_err(&base("A = []")).contains("`A` must be a string or a table"));
    assert!(parse_err(&base("A = {}")).contains("`A` needs a `value`, or `required = true`"));
    assert!(parse_err(&base("A = { value = \"x\", required = true }")).contains("both `value` and `required`"));
    assert!(parse_err(&base("A = { required = true, relative = true }")).contains("`relative` without a `value`"));
    assert!(parse_err(&base("A = { value = \"x\", typo = 1 }")).contains("unknown field"));
    let profile = format!("{MINIMAL}\n[profile.dev.env]\nB = {{}}\n");
    assert!(parse_err(&profile).contains("[profile.dev.env]: `B` needs a `value`"));
}

#[test]
fn env_table_reads_env_files_and_expose() {
    let m = parse(&format!(
        "{MINIMAL}\n[env]\nexpose = [\"APP_NAME\", \"PORT\"]\nenv-files = [\".env.shared\"]\nAPP_NAME = \"demo\"\n\n\
         [profile.release.env]\nenv-files = [\"config/release.env\"]\n"
    ));
    assert_eq!(m.env.expose, ["APP_NAME", "PORT"]);
    assert_eq!(m.env.files, [".env.shared"]);
    assert_eq!(m.profiles["release"].env.files, ["config/release.env"]);
    assert!(m.env.vars.contains_key("APP_NAME") && !m.env.vars.contains_key("expose"));
}

#[test]
fn custom_profiles_inherit_and_override() {
    let m = parse(&format!(
        "{MINIMAL}\n[profile.release]\noptimize = true\ndebug = false\n\n\
         [profile.staging]\ninherits = \"release\"\ndebug = true\njvm-args = [\"-Xmx1g\", \"-Dapp.env=staging\"]\n\
         compiler-args = [\"-Xno-param-assertions\"]\n\n\
         [profile.bench]\ninherits = \"staging\"\n\n[profile.quick]\noptimize = true\n\n\
         [profile.dev]\njvm-args = [\"-ea\"]\n"
    ));
    let staging = &m.profiles["staging"];
    assert!(staging.debug && staging.optimize);
    assert_eq!(staging.jvm_args, ["-Xmx1g", "-Dapp.env=staging"]);
    assert_eq!(staging.compiler_args, ["-Xno-param-assertions"]);
    // Keys the profile does not set come from the profile it inherits from, through any number of steps.
    let bench = &m.profiles["bench"];
    assert!(bench.debug && bench.optimize);
    assert_eq!((&bench.jvm_args, &bench.compiler_args), (&staging.jvm_args, &staging.compiler_args));
    // Custom profiles inherit from `dev` unless they say otherwise.
    assert!(m.profiles["quick"].debug && m.profiles["quick"].optimize);
    assert_eq!(m.profiles["dev"].jvm_args, ["-ea"]);
    assert!(m.profiles["dev"].debug && !m.profiles["dev"].optimize);
    assert!(m.profile("missing").is_none());
}

#[test]
fn profile_env_is_inherited_and_extended() {
    let m = parse(&format!(
        "{MINIMAL}\n[profile.dev.env]\nA = \"dev\"\nB = \"dev\"\nenv-files = [\"dev.env\"]\n\n\
         [profile.staging.env]\nB = \"staging\"\nenv-files = [\"staging.env\"]\n"
    ));
    let staging = &m.profiles["staging"].env;
    assert_eq!(staging.vars["A"], EnvVar { value: Some("dev".to_owned()), ..Default::default() });
    assert_eq!(staging.vars["B"], EnvVar { value: Some("staging".to_owned()), ..Default::default() });
    assert_eq!(staging.files, ["dev.env", "staging.env"]);
    assert_eq!(m.profiles["dev"].env.files, ["dev.env"]);
}

#[test]
fn profile_dependencies_are_inherited_and_replaced() {
    let m = parse(&format!(
        "{MINIMAL}\n[profile.dev.dependencies]\n\"com.example:leak\" = \"2.0\"\n\n\
         [profile.release.dependencies]\n\"com.example:mock\" = \"1.0\"\n\n\
         [profile.staging]\ninherits = \"dev\"\n\n[profile.staging.dependencies]\n\"com.example:leak\" = \"3.0\"\n"
    ));
    assert_eq!(m.profiles["release"].dependencies[0].name, "com.example:mock");
    assert_eq!(m.profiles["dev"].dependencies.len(), 1);
    let staging = &m.profiles["staging"].dependencies;
    assert_eq!(staging.len(), 1, "a profile's dependency replaces the inherited one with the same artifact");
    assert_eq!(staging[0].version.as_deref(), Some("3.0"));
}

#[test]
fn profile_names_and_inheritance_are_checked() {
    let err = |profile: &str| parse_err(&format!("{MINIMAL}\n{profile}\n"));
    assert!(err("[profile.dev]\ninherits = \"release\"").contains("`inherits` is for custom profiles"));
    assert!(err("[profile.staging]\ninherits = \"missing\"").contains("inherits from `missing`, which is not a profile"));
    assert!(err("[profile.a]\ninherits = \"b\"\n[profile.b]\ninherits = \"a\"").contains("loops: a -> b -> a"));
    assert!(err("[profile.debug]").contains("profile name `debug` is reserved"));
    assert!(err("[profile.\"my profile\"]").contains("invalid profile name `my profile`"));
}

#[test]
fn env_table_keeps_a_string_named_expose_a_variable() {
    let m = parse(&format!("{MINIMAL}\n[env]\nexpose = \"yes\"\n"));
    assert_eq!(m.env.vars["expose"], EnvVar { value: Some("yes".to_owned()), ..Default::default() });
    assert!(m.env.expose.is_empty());
}

#[test]
fn env_table_rejects_bad_files_and_expose() {
    let base = |env: &str| format!("{MINIMAL}\n[env]\n{env}\n");
    assert!(parse_err(&base("expose = [\"bad-name\"]")).contains("`expose` lists `bad-name`"));
    assert!(parse_err(&base("expose = [\"A\", \"A\"]")).contains("more than once"));
    assert!(parse_err(&base("env-files = [\"\"]")).contains("`env-files` must be an array of non-empty strings"));
    assert!(parse_err(&base("env-files = \".env\"")).contains("`env-files` must be an array of non-empty strings"));
    let profile = format!("{MINIMAL}\n[profile.dev.env]\nexpose = [\"A\"]\n");
    assert!(parse_err(&profile).contains("`expose` belongs in [env]"));
}

mod script {
    use crate::JdkRequest;
    use crate::script::*;

    fn kts(text: &str) -> KtsScript {
        parse_kts(text).unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn settings(text: &str) -> Settings {
        settings_from_annotations(text, &kts(text).annotations).unwrap_or_else(|e| panic!("{e:#}"))
    }

    #[test]
    fn reads_the_header_annotations_and_blanks_them() {
        let text = "#!/usr/bin/env -S kargo script\n@file:DependsOn(\"a:b:1.0\")\n@file:Kotlin(\"2.3.0\")\n\nprintln(1)\n";
        let script = kts(text);
        assert_eq!(script.annotations.len(), 2);
        assert_eq!(script.annotations[0].name, "DependsOn");
        assert_eq!(script.annotations[0].args, ["a:b:1.0"]);
        assert_eq!(script.code.lines().count(), text.lines().count());
        assert!(script.code.starts_with("#!/usr/bin/env -S kargo script\n"));
        assert!(script.code.ends_with("\n\nprintln(1)\n"), "{:?}", script.code);
        assert!(!script.code.contains("@file"));
    }

    #[test]
    fn skips_comments_before_annotations() {
        let text = "// first\n/* block /* nested */ still */\n@file:Jdk(\"21\")\nfun f() = 1\n";
        let script = kts(text);
        assert_eq!(settings(text).jdk, Some(JdkRequest::Major(21)));
        assert!(script.code.contains("fun f() = 1"));
    }

    #[test]
    fn stops_at_the_first_declaration() {
        let text = "val x = 1\n@file:DependsOn(\"a:b:1\")\n";
        assert!(kts(text).annotations.is_empty());
        assert!(kts(text).code.contains("@file:DependsOn"));
    }

    #[test]
    fn keeps_other_annotations_for_the_compiler() {
        let text = "@file:Suppress(\"UNUSED\")\n@file:DependsOn(\"a:b:1\")\nprintln(1)\n";
        let script = kts(text);
        assert_eq!(script.annotations.len(), 1);
        assert!(script.code.contains("@file:Suppress(\"UNUSED\")"));
        assert!(!script.code.contains("DependsOn"));
    }

    #[test]
    fn reads_array_and_multi_argument_forms() {
        let text = "@file:DependsOn([\"a:b:1\", \"c:d:2\"])\n@file:Repository(\"https://example.com/m2/\", \"mavenCentral\")\n";
        let s = settings(text);
        assert_eq!(s.dependencies.iter().map(ToString::to_string).collect::<Vec<_>>(), ["a:b:1", "c:d:2"]);
        assert_eq!(s.repositories, ["https://example.com/m2", "mavenCentral"]);
    }

    #[test]
    fn reads_string_escapes_and_raw_strings() {
        let s = settings("@file:Framework(\"a\\\"b\", \"\"\"c\"d\"\"\")\n");
        assert_eq!(s.frameworks, ["a\"b", "c\"d"]);
    }

    #[test]
    fn rejects_malformed_known_annotations_with_their_line() {
        let err = format!(
            "{:#}",
            parse_kts("\n@file:DependsOn(\"a:b\")\n")
                .and_then(|s| settings_from_annotations("\n@file:DependsOn(\"a:b\")\n", &s.annotations))
                .unwrap_err()
        );
        assert!(err.starts_with("line 2: invalid @file:DependsOn"), "{err}");
        assert!(err.contains("version pinned"), "{err}");
        let err = format!("{:#}", parse_kts("\n@file:Kotlin(\"unterminated)\n").unwrap_err());
        assert!(err.starts_with("line 2: invalid @file:Kotlin"), "{err}");
    }

    #[test]
    fn rejects_import_until_it_is_supported() {
        let err = format!("{:#}", parse_kts("@file:Import(\"other.kts\")\n").unwrap_err());
        assert!(err.contains("@file:Import is not supported yet"), "{err}");
    }

    #[test]
    fn rejects_repositories_that_are_not_urls() {
        let err = format!(
            "{:#}",
            parse_kts("@file:Repository(\"ftp://x\")\n")
                .and_then(|s| settings_from_annotations("@file:Repository(\"ftp://x\")\n", &s.annotations))
                .unwrap_err()
        );
        assert!(err.contains("invalid repository"), "{err}");
    }

    #[test]
    fn recognises_kargo_scripts() {
        assert!(is_kargo_script("println(1)", false), "outside a project every .kts is a script");
        assert!(!is_kargo_script("println(1)", true), "inside a project only a kargo shebang makes one");
        assert!(is_kargo_script("#!/usr/bin/env -S kargo script\nprintln(1)", true));
        assert!(is_kargo_script("#!/usr/local/bin/kargo script\n", true));
        assert!(!is_kargo_script("#!/usr/bin/env kotlin\n", true));
    }

    #[test]
    fn lists_the_scripts_of_tasks_and_plugins() {
        let text = "[package]
name = \"a\"
version = \"1\"
[tasks]
gen = { script = \"scripts/gen.kts\" }
build = \"echo\"
[plugins.n]
script = \"n.kt\"
hooks = [\"post-build\"]
[workspace.tasks]
rel = { script = \"rel.kts\" }
";
        assert_eq!(
            manifest_scripts(text),
            [std::path::Path::new("scripts/gen.kts"), std::path::Path::new("n.kt"), std::path::Path::new("rel.kts")]
        );
        assert!(
            manifest_scripts(
                "[tasks
"
            )
            .is_empty()
        );
    }

    #[test]
    fn reads_kt_front_matter() {
        let text = "#!/usr/bin/env -S kargo script\n/* kargo\nkotlin = \"2.3.0\"\njdk = \"21\"\nrepositories = [\"https://jitpack.io\"]\nframeworks = [\"serialization\"]\n[dependencies]\n\"com.github.ajalt.clikt:clikt\" = \"5.0.0\"\n*/\nfun main() {}\n";
        let s = parse_kt_front_matter(text).unwrap().expect("front matter");
        assert_eq!(s.kotlin.as_deref(), Some("2.3.0"));
        assert_eq!(s.jdk, Some(JdkRequest::Major(21)));
        assert_eq!(s.repositories, ["https://jitpack.io"]);
        assert_eq!(s.frameworks, ["serialization"]);
        assert_eq!(s.dependencies[0].to_string(), "com.github.ajalt.clikt:clikt:5.0.0");
    }

    #[test]
    fn ignores_other_block_comments_and_rejects_unknown_front_matter_keys() {
        assert!(parse_kt_front_matter("/** KDoc */\nfun main() {}\n").unwrap().is_none());
        assert!(parse_kt_front_matter("fun main() {}\n").unwrap().is_none());
        let err = format!("{:#}", parse_kt_front_matter("/* kargo\nflavour = \"x\"\n*/\n").unwrap_err());
        assert!(err.contains("unknown key `flavour`"), "{err}");
    }

    #[test]
    fn reads_script_targets() {
        use crate::script::ScriptTarget;
        assert_eq!(settings("@file:Target(\"native\")\n").target, Some(ScriptTarget::Native));
        assert_eq!(settings("@file:Target(\"jvm\")\n").target, Some(ScriptTarget::Jvm));
        let err = format!(
            "{:#}",
            parse_kts("@file:Target(\"wasm\")\n")
                .and_then(|s| settings_from_annotations("@file:Target(\"wasm\")\n", &s.annotations))
                .unwrap_err()
        );
        assert!(err.contains("invalid target"), "{err}");
        let err = format!(
            "{:#}",
            parse_kts("@file:Target(\"jvm\", \"native\")\n")
                .and_then(|s| settings_from_annotations("@file:Target(\"jvm\", \"native\")\n", &s.annotations))
                .unwrap_err()
        );
        assert!(err.contains("takes exactly one"), "{err}");

        let s = parse_kt_front_matter("/* kargo\ntarget = \"native\"\n*/\nfun main() {}\n").unwrap().unwrap();
        assert_eq!(s.target, Some(ScriptTarget::Native));
        let s = parse_kt_front_matter("/* kargo\ntarget = \"jvm\"\n*/\nfun main() {}\n").unwrap().unwrap();
        assert_eq!(s.target, Some(ScriptTarget::Jvm));
        let err = format!("{:#}", parse_kt_front_matter("/* kargo\ntarget = \"wasm\"\n*/\n").unwrap_err());
        assert!(err.contains("invalid target"), "{err}");
    }
}

#[test]
fn toolchain_graalvm_requests() {
    assert_eq!(parse(MINIMAL).toolchain.graalvm, GraalvmRequest::System);
    let with = |line: &str| format!("{MINIMAL}\n[toolchain]\n{line}\n");
    let community = |major| GraalvmRequest::Major { distribution: GraalvmDistribution::Community, major };
    assert_eq!(parse(&with("graalvm = \"25\"")).toolchain.graalvm, community(25));
    assert_eq!(parse(&with("graalvm = 21")).toolchain.graalvm, community(21));
    assert_eq!(parse(&with("graalvm = \"community-25\"")).toolchain.graalvm, community(25));
    let oracle = parse(&with("graalvm = \"oracle-25\"")).toolchain.graalvm;
    assert_eq!(oracle, GraalvmRequest::Major { distribution: GraalvmDistribution::Oracle, major: 25 });
    assert_eq!(oracle.to_string(), "oracle-25");
    assert_eq!(community(25).to_string(), "25");
    assert_eq!(parse(&with("graalvm = \"system\"")).toolchain.graalvm, GraalvmRequest::System);
    assert!(parse_err(&with("graalvm = \"11\"")).contains("17 or newer"));
    assert!(parse_err(&with("graalvm = \"mandrel-25\"")).contains("invalid GraalVM"));
    assert!(parse_err(&with("graalvm = true")).contains("must be a version"));
}

#[test]
fn packaging_tables() {
    let m = parse(MINIMAL);
    assert_eq!(m.packaging, PackagingSettings::default());
    assert!(m.packaging.jlink.jdeps && m.packaging.native_image.metadata_repository);

    let m = parse(&format!(
        r#"{MINIMAL}
[package.jlink]
modules = ["java.naming", "jdk.crypto.ec"]

[package.optimize]
shrink = false
optimize = true
obfuscate = false
rules = "config/keep.pro"

[package.native-image]
args = ["--enable-url-protocols=https"]
static = true
initialize-at-build-time = ["kotlinx.serialization"]
metadata-repository = false

[package.installer]
name = "Hello App"
vendor = "Example"
app-version = "1.2.3"
type = "app-image"
icon = {{ windows = "icons/app.ico", linux = "icons/app.png" }}
license-file = "LICENSE"
menu = true
shortcut = true
console = true
upgrade-uuid = "0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90"
bundle-id = "com.example.hello"
category = "Development"
"#
    ));
    assert_eq!(m.packaging.jlink.modules, ["java.naming", "jdk.crypto.ec"]);
    assert_eq!(
        m.packaging.optimize,
        OptimizeSettings { shrink: false, optimize: true, obfuscate: false, rules: PathBuf::from("config/keep.pro") }
    );
    let native = &m.packaging.native_image;
    assert!(native.static_link && !native.metadata_repository);
    assert_eq!(native.initialize_at_build_time, ["kotlinx.serialization"]);
    let installer = &m.packaging.installer;
    assert_eq!(installer.kinds, [InstallerType::AppImage]);
    assert_eq!(installer.kinds_for("windows"), [InstallerType::AppImage]);
    assert_eq!(installer.icon.for_os("windows"), Some(Path::new("icons/app.ico")));
    assert_eq!(installer.icon.for_os("macos"), None);
    assert_eq!(installer.icon.for_os("linux"), Some(Path::new("icons/app.png")));
    assert!(installer.menu && installer.shortcut && installer.console);
    assert_eq!(installer.app_version.as_deref(), Some("1.2.3"));

    let one_icon = parse(&format!("{MINIMAL}\n[package.installer]\nicon = \"app.png\"\n"));
    assert_eq!(one_icon.packaging.installer.icon.for_os("macos"), Some(Path::new("app.png")));

    let with = |text: &str| format!("{MINIMAL}\n{text}\n");
    assert!(parse_err(&with("[package.installer]\ntype = \"zip\"")).contains("unknown installer type `zip`"));
    assert!(parse_err(&with("[package.installer]\napp-version = \"1.0-SNAPSHOT\"")).contains("app-version"));
    assert!(parse_err(&with("[package.installer]\nupgrade-uuid = \"nope\"")).contains("UUID"));
    assert!(parse_err(&with("[package.installer]\nicon = { android = \"a.png\" }")).contains("icon"));
    assert!(parse_err(&with("[package.installer]\nname = \"a/b\"")).contains("file name"));
    assert!(parse_err(&with("[package.installer]\nbogus = 1")).contains("bogus"));
    assert!(parse_err(&with("[package.jlink]\nmodules = [\"java naming\"]")).contains("module names"));
    assert!(parse_err(&with("[package.jlink]\njdeps = false")).contains("needs `modules`"));
    assert_eq!(parse(&with("[package.dist]\ninclude = [\"conf\", \"docs/NOTICE\"]")).packaging.dist.include.len(), 2);
    assert!(parse_err(&with("[package.dist]\ninclude = [\"../secret\"]")).contains("inside the project"));
    assert!(parse_err(&with("[package.native-image]\nstatic = \"yes\"")).contains("static"));
}

#[test]
fn shade_table() {
    assert_eq!(parse(MINIMAL).packaging.shade, None);
    let m = parse(&format!(
        r#"{MINIMAL}
[package.shade]
relocate = {{ "com.google" = "shaded.google", "com.google.common" = "com.example.shaded.guava" }}
exclude = ["org.slf4j.*"]
minimize = true
keep = ["com.example.Plugin"]
"#
    ));
    let shade = m.packaging.shade.unwrap();
    assert_eq!(shade.relocate[0], ("com.google.common".to_owned(), "com.example.shaded.guava".to_owned()), "longest prefix first");
    assert_eq!(shade.relocate[1].0, "com.google");
    assert_eq!(shade.exclude, ["org.slf4j.*"]);
    assert!(shade.minimize && shade.merge_services && !shade.relocate_strings);
    let empty = parse(&format!("{MINIMAL}\n[package.shade]\n")).packaging.shade.unwrap();
    assert!(empty.relocate.is_empty() && !empty.minimize && empty.merge_services);

    let with = |body: &str| format!("{MINIMAL}\n[package.shade]\n{body}");
    assert!(parse_err(&with("relocate = { \"com/x\" = \"y\" }")).contains("package names"));
    assert!(parse_err(&with("relocate = { \"a.b\" = \"a.b\" }")).contains("to itself"));
    assert!(parse_err(&with("exclude = [\"a/b\"]")).contains("exclude"));
    assert!(parse_err(&with("keep = [\"a.B\"]")).contains("minimize"));
    assert!(parse_err(&with("bogus = 1")).contains("bogus"));
}

#[test]
fn desktop_installers() {
    let m = parse(&format!(
        r#"{MINIMAL}
[package.installer]
name = "Notes"
type = ["msi", "dmg", "deb", "app-image"]
file-associations = [
    {{ extension = ".note", mime-type = "application/x-note", description = "Note", icon = "icons/note.ico" }},
    {{ extension = "notes", mime-type = "application/x-notes" }},
]
signing-identity = "Developer ID Application: Example (ABCDE12345)"
notarize = {{ apple-id-env = "APPLE_ID", password-env = "APPLE_APP_PASSWORD", team-id = "ABCDE12345" }}

[frameworks]
compose = {{ resources-package = "com.example.notes.res", public-resources = true }}
"#
    ));
    let installer = &m.packaging.installer;
    assert_eq!(installer.kinds_for("windows"), [InstallerType::Msi, InstallerType::AppImage]);
    assert_eq!(installer.kinds_for("macos"), [InstallerType::Dmg, InstallerType::AppImage]);
    assert_eq!(installer.kinds_for("linux"), [InstallerType::Deb, InstallerType::AppImage]);
    assert_eq!(installer.app_name("notes"), "Notes");
    let first = &installer.file_associations[0];
    assert_eq!((first.extension.as_str(), first.mime_type.as_str()), ("note", "application/x-note"));
    assert_eq!(first.icon.as_deref(), Some(Path::new("icons/note.ico")));
    assert!(installer.file_associations[1].description.is_none());
    assert_eq!(
        installer.notarize,
        Some(Notarization::AppleId {
            apple_id_env: "APPLE_ID".into(),
            password_env: "APPLE_APP_PASSWORD".into(),
            team_id: "ABCDE12345".into()
        })
    );
    let compose = m.frameworks.compose.as_ref().unwrap();
    assert_eq!(compose.resources_package.as_deref(), Some("com.example.notes.res"));
    assert_eq!(compose.public_resources, Some(true));

    let only_mac = parse(&format!("{MINIMAL}\n[package.installer]\ntype = [\"dmg\"]\n"));
    assert!(only_mac.packaging.installer.kinds_for("windows").is_empty());
    let default = parse(MINIMAL);
    assert_eq!(default.packaging.installer.kinds_for("macos"), [InstallerType::Dmg]);
    let profile = parse(&format!(
        "{MINIMAL}\n[package.installer]\nsigning-identity = \"Developer ID\"\nnotarize = {{ keychain-profile = \"kargo\" }}\n"
    ));
    assert_eq!(profile.packaging.installer.notarize, Some(Notarization::KeychainProfile("kargo".into())));

    let with = |text: &str| format!("{MINIMAL}\n{text}\n");
    assert!(parse_err(&with("[package.installer]\ntype = []")).contains("list"));
    assert!(parse_err(&with("[package.installer]\ntype = [\"msi\", \"zip\"]")).contains("unknown installer type `zip`"));
    assert!(
        parse_err(&with("[package.installer]\nfile-associations = [{ extension = \"a b\", mime-type = \"x/y\" }]")).contains("extensions")
    );
    assert!(parse_err(&with("[package.installer]\nfile-associations = [{ extension = \"a\", mime-type = \"text\" }]")).contains("MIME"));
    assert!(parse_err(&with("[package.installer]\nfile-associations = [{ extension = \"a\" }]")).contains("mime-type"));
    let twice = "[package.installer]\nfile-associations = [{ extension = \"a\", mime-type = \"x/a\" }, { extension = \".a\", mime-type = \"x/b\" }]";
    assert!(parse_err(&with(twice)).contains("more than once"));
    assert!(parse_err(&with("[package.installer]\nnotarize = { keychain-profile = \"p\" }")).contains("signing-identity"));
    let partial = "[package.installer]\nsigning-identity = \"x\"\nnotarize = { apple-id-env = \"APPLE_ID\" }";
    assert!(parse_err(&with(partial)).contains("keychain-profile"));
    let bad_env =
        "[package.installer]\nsigning-identity = \"x\"\nnotarize = { apple-id-env = \"$ID\", password-env = \"P\", team-id = \"T1\" }";
    assert!(parse_err(&with(bad_env)).contains("environment variable"));
    assert!(parse_err(&with("[frameworks]\ncompose = { resources-package = \"1res\" }")).contains("resources-package"));
}

#[test]
fn installer_type_hosts() {
    assert_eq!(InstallerType::default_for("windows"), InstallerType::Msi);
    assert_eq!(InstallerType::default_for("macos"), InstallerType::Dmg);
    assert_eq!(InstallerType::default_for("linux"), InstallerType::Deb);
    assert_eq!(InstallerType::Pkg.os(), Some("macos"));
    assert_eq!(InstallerType::AppImage.os(), None);
    for t in InstallerType::ALL {
        assert_eq!(t.as_str().parse::<InstallerType>().unwrap(), t);
    }
}

#[test]
fn toolchain_kargo_requirement() {
    let with = |line: &str| format!("{MINIMAL}\n[toolchain]\n{line}\n");
    assert_eq!(parse(MINIMAL).toolchain.kargo, None);
    assert!(parse(&with("kargo = \">=0.1\"")).toolchain.kargo.is_some());
    assert!(parse(&with("kargo = \"^0.1.0\"")).toolchain.kargo.is_some());
    let err = parse_err(&with("kargo = \">=99.0\""));
    assert!(err.contains("needs kargo >=99.0") && err.contains("kargo self update"), "{err}");
    assert!(parse_err(&with("kargo = \"newest\"")).contains("invalid [toolchain] `kargo` requirement"));
}

#[test]
fn toolchain_typescript_request() {
    let with = |line: &str| format!("{MINIMAL}\n[toolchain]\n{line}\n");
    assert_eq!(parse(MINIMAL).toolchain.typescript, None);
    assert_eq!(parse(&with("typescript = \"5.9.3\"")).toolchain.typescript.as_deref(), Some("5.9.3"));
    assert_eq!(parse(&with("typescript = \" ^6 \"")).toolchain.typescript.as_deref(), Some("^6"));
    assert_eq!(parse(&with("typescript = \"latest\"")).toolchain.typescript.as_deref(), Some("latest"));
    assert!(parse_err(&with("typescript = \"\"")).contains("`typescript` is empty"));
    assert!(parse_err(&with("typescript = \">=5 <7\"")).contains("invalid [toolchain] `typescript`"));
    assert!(parse_err(&with("typescript = 5")).contains("typescript"));
}

#[test]
fn kargo_version_requirements_are_checked_against_the_running_kargo() {
    let req = |text: &str| semver::VersionReq::parse(text).unwrap();
    assert!(check_kargo_version(&req(">=0.3"), "0.3.0").is_ok());
    assert!(check_kargo_version(&req(">=0.3"), "1.0.0").is_ok());
    assert!(check_kargo_version(&req(">=0.3"), "0.2.9").is_err());
    assert!(check_kargo_version(&req(">=0.2"), "0.3.0-rc.1").is_ok(), "a release candidate counts as its release");
    assert!(check_kargo_version(&req(">=0.3"), "0.3.0-rc.1").is_ok());
    assert!(check_kargo_version(&req(">=0.3"), "0.2.9-rc.1").is_err());
    assert!(check_kargo_version(&req("^0.3"), "0.4.0").is_err());
    assert!(check_kargo_version(&req(">=0.3"), "not a version").is_err());
}

#[test]
fn generate_steps_run_commands_tools_and_builtins() {
    assert!(parse(MINIMAL).generators.is_empty());
    let m = parse(&format!(
        r#"{MINIMAL}
[[generate]]
name = "protos"
inputs = ["proto/**/*.proto"]
run = "protoc --kotlin_out=$KARGO_GEN_DIR proto/*.proto"
windows-run = ["protoc.exe --kotlin_out=%KARGO_GEN_DIR% proto/a.proto"]
targets = ["jvm", "android"]
kind = "test-sources"
env = {{ A = "b" }}

[[generate]]
name = "api-client"
inputs = ["openapi.yaml"]
tool = "org.openapitools:openapi-generator-cli:7.16.0"
args = ["generate", "-o", "$KARGO_GEN_DIR"]
after = ["protos"]

[[generate]]
name = "config"
builtin = "build-config"
fields = {{ API_URL = "https://example.com", RETRIES = 3, DEBUG = true }}
expose = ["TOKEN"]
build-time = true
roots = ["src\\main/kotlin/", "gen", "gen"]

[[generate]]
name = "grpc"
builtin = "protobuf"
grpc = true
"#
    ));
    assert_eq!(m.generators.len(), 4);
    let protos = m.generator("protos").unwrap();
    assert_eq!(protos.inputs, ["proto/**/*.proto"]);
    assert_eq!(protos.targets, [TargetKind::Jvm, TargetKind::Android]);
    assert_eq!(protos.kind, GeneratedKind::TestSources);
    assert_eq!(protos.env["A"], "b");
    assert!(protos.applies_to(TargetKind::Android) && !protos.applies_to(TargetKind::Wasm));
    let GeneratorAction::Run { run, windows_run, shell } = &protos.action else { panic!("{:?}", protos.action) };
    assert_eq!((run.len(), windows_run.as_ref().map(Vec::len), *shell), (1, Some(1), None));

    let api = m.generator("api-client").unwrap();
    assert!(api.roots.is_empty(), "the whole output directory by default");
    assert_eq!(m.generator("config").unwrap().roots, ["src/main/kotlin", "gen"], "separators and duplicates are tidied");
    assert_eq!(api.targets, TargetKind::ALL, "every target by default");
    assert_eq!(api.kind, GeneratedKind::Sources);
    assert_eq!(api.after, ["protos"]);
    let GeneratorAction::Tool { module, version, main_class, args } = &api.action else { panic!("{:?}", api.action) };
    assert_eq!(
        (module.to_string().as_str(), version.as_str(), main_class.as_deref(), args.len()),
        ("org.openapitools:openapi-generator-cli", "7.16.0", None, 3)
    );

    let config = m.generator("config").unwrap();
    assert_eq!(config.builtin(), Some("build-config"));
    assert!(config.inputs.is_empty());
    let GeneratorAction::BuildConfig(c) = &config.action else { panic!("{:?}", config.action) };
    assert_eq!((c.object.as_str(), c.package.as_deref(), c.build_time), ("BuildConfig", None, true));
    assert_eq!(c.fields["RETRIES"], BuildConfigValue::Int(3));
    assert_eq!(c.expose, ["TOKEN"]);

    let grpc = m.generator("grpc").unwrap();
    assert_eq!(grpc.targets, [TargetKind::Jvm, TargetKind::Android], "protobuf's classes are JVM ones");
    assert_eq!(grpc.inputs, ["proto/**/*.proto"]);
    let GeneratorAction::Protobuf(p) = &grpc.action else { panic!("{:?}", grpc.action) };
    assert_eq!((p.proto_path.as_slice(), p.java, p.kotlin, p.grpc), (["proto".to_owned()].as_slice(), true, true, true));
}

#[test]
fn generate_steps_are_validated() {
    let generate = |body: &str| format!("{MINIMAL}\n[[generate]]\nname = \"g\"\n{body}\n");
    let err = |body: &str| parse_err(&generate(body));
    assert!(err("").contains("set exactly one of `run`"));
    assert!(err("run = \"x\"\ntool = \"a:b:1\"").contains("set exactly one of `run`"));
    assert!(err("run = []").contains("`run` has no command"));
    assert!(err("run = 3").contains("`run` must be a string or an array of strings"));
    assert!(err("tool = \"a:b\"").contains("`group:artifact:version`"));
    assert!(err("tool = \"a:b:1\"\nwindows-run = \"x\"").contains("goes with `run`"));
    assert!(err("tool = \"a:b:1\"\nshell = \"sh\"").contains("goes with `run`"));
    assert!(err("run = \"x\"\nmain-class = \"a.B\"").contains("`main-class` goes with `tool`"));
    assert!(err("tool = \"a:b:1\"\nmain-class = \"a..B\"").contains("invalid `main-class`"));
    assert!(err("run = \"x\"\nargs = [\"y\"]").contains("`args` goes with `tool`"));
    assert!(err("run = \"x\"\nshell = \"fish\"").contains("unknown shell `fish`"));
    assert!(err("run = \"x\"\ntargets = [\"ios\"]").contains("invalid `targets`"));
    assert!(err("run = \"x\"\ntargets = []").contains("`targets` is empty"));
    assert!(err("run = \"x\"\nkind = \"docs\"").contains("unknown kind `docs`"));
    assert!(err("run = \"x\"\nkind = \"resources\"\ntargets = [\"wasm\", \"native\"]").contains("resources only exist"));
    assert!(err("run = \"x\"\nroots = [\"../up\"]").contains("`roots` entry `../up` must be a directory inside the output"));
    assert!(err("run = \"x\"\nroots = [\"/abs\"]").contains("`roots` entry"));
    assert!(err("run = \"x\"\nroots = [\"a//b\"]").contains("`roots` entry"));
    assert!(err("run = \"x\"\nafter = [\"nope\"]").contains("`nope`, which no [[generate]] declares"));
    assert!(err("run = \"x\"\nafter = [\"g\"]").contains("runs after itself"));
    assert!(err("run = \"x\"\nbogus = 1").contains("unknown field"));
    assert!(parse_err(&format!("{MINIMAL}\n[[generate]]\nname = \"1g\"\nrun = \"x\"\n")).contains("invalid [[generate]] name"));
    let twice = format!("{}[[generate]]\nname = \"g\"\nrun = \"y\"\n", generate("run = \"x\""));
    assert!(parse_err(&twice).contains("declared more than once"));
    let cycle = format!(
        "{MINIMAL}\n[[generate]]\nname = \"a\"\nrun = \"x\"\nafter = [\"b\"]\n[[generate]]\nname = \"b\"\nrun = \"y\"\nafter = [\"a\"]\n"
    );
    assert!(parse_err(&cycle).contains("a -> b -> a"));

    assert!(err("builtin = \"gradle\"").contains("unknown builtin `gradle`"));
    assert!(err("run = \"x\"\npackage = \"a.b\"").contains("`package` goes with `builtin = \"build-config\"`"));
    assert!(err("builtin = \"build-config\"\ngrpc = true").contains("`grpc` goes with `builtin = \"protobuf\"`"));
    assert!(err("builtin = \"build-config\"\ninputs = [\"a\"]").contains("`inputs` does not apply"));
    assert!(err("builtin = \"build-config\"\npackage = \"a..b\"").contains("invalid `package`"));
    assert!(err("builtin = \"build-config\"\nobject = \"Build Config\"").contains("`object` must be a Kotlin name"));
    assert!(err("builtin = \"build-config\"\nfields = { VERSION = \"1\" }").contains("one of the constants Kargo writes"));
    assert!(err("builtin = \"build-config\"\nfields = { \"A-B\" = 1 }").contains("not a valid constant name"));
    assert!(err("builtin = \"protobuf\"\njava = false\nkotlin = false").contains("nothing to generate"));
    assert!(err("builtin = \"protobuf\"\njava = false").contains("wraps the Java classes"));
    assert!(err("builtin = \"protobuf\"\nversion = \"[1,2)\"").contains("one exact version"));
    assert!(err("builtin = \"protobuf\"\ngrpc-java-version = \"1.0\"").contains("go with `grpc = true`"));
}

#[test]
fn build_jobs() {
    use kargo_util::jobs::Jobs;
    assert_eq!(parse(MINIMAL).build.jobs, None);
    assert_eq!(parse(&format!("{MINIMAL}[build]\njobs = 2\n")).build.jobs, Some(Jobs::Count(2)));
    assert_eq!(parse(&format!("{MINIMAL}[build]\njobs = \"auto\"\n")).build.jobs, Some(Jobs::Auto));
    assert_eq!(parse(&format!("{MINIMAL}[build]\njobs = 0\n")).build.jobs, Some(Jobs::Auto));
    assert!(parse_err(&format!("{MINIMAL}[build]\njobs = -1\n")).contains("[build] `jobs` must be a number of jobs or \"auto\""));
    assert!(parse_err(&format!("{MINIMAL}[build]\njobs = \"lots\"\n")).contains("expected a number of jobs or `auto`"));
    assert!(parse_err(&format!("{MINIMAL}[build]\nworkers = 2\n")).contains("unknown field"));

    let ws = WorkspaceManifest::parse("[workspace]\nmembers = []\n[workspace.build]\njobs = 3\n", Path::new("/ws")).unwrap();
    assert_eq!(ws.build.jobs, Some(Jobs::Count(3)));
}

#[test]
fn build_default_targets() {
    assert_eq!(parse(MINIMAL).build.default_targets, None);
    let m = parse(&format!("{MINIMAL}[target.jvm]\n[target.wasm]\n[build]\ndefault-targets = [\"wasm\", \"jvm\", \"wasm\"]\n"));
    assert_eq!(m.build.default_targets, Some(vec![TargetKind::Wasm, TargetKind::Jvm]));
    assert!(parse_err(&format!("{MINIMAL}[build]\ndefault-targets = []\n")).contains("`default-targets` is empty"));
    assert!(parse_err(&format!("{MINIMAL}[build]\ndefault-targets = [\"ios\"]\n")).contains("unknown target `ios`"));
    assert!(parse_err(&format!("{MINIMAL}[build]\ndefault-targets = [\"wasm\"]\n")).contains("names `wasm`, which is not enabled"));

    let ws =
        WorkspaceManifest::parse("[workspace]\nmembers = []\n[workspace.build]\ndefault-targets = [\"jvm\"]\n", Path::new("/ws")).unwrap();
    assert_eq!(ws.build.default_targets, Some(vec![TargetKind::Jvm]));
}

#[test]
fn target_platforms() {
    use std::str::FromStr;
    assert_eq!(TargetPlatform::from_str("linux-arm64").unwrap().as_str(), "linux-arm64");
    assert_eq!(TargetPlatform::from_str("linux-amd64").unwrap().as_str(), "linux-x64");
    assert_eq!(TargetPlatform::from_str("linux-aarch64").unwrap().as_str(), "linux-arm64");
    assert_eq!(TargetPlatform::from_str("MACOS-ARM64").unwrap().as_str(), "macos-arm64");
    assert_eq!(TargetPlatform::from_str("windows_x64").unwrap().as_str(), "windows-x64");
    assert!(TargetPlatform::from_str("freebsd-x64").is_err());
    assert!(TargetPlatform::from_str("linux").is_err());
    let host = TargetPlatform::host().unwrap();
    assert!(host.is_host());
    assert_ne!(TargetPlatform::ALL.len(), 0);
}

#[test]
fn build_platforms() {
    assert_eq!(parse(MINIMAL).build.platforms, None);
    let m = parse(&format!("{MINIMAL}[build]\nplatforms = [\"linux-arm64\", \"linux-amd64\", \"linux-arm64\"]\n"));
    assert_eq!(m.build.platforms.unwrap().iter().map(|p| p.as_str()).collect::<Vec<_>>(), ["linux-arm64", "linux-x64"]);
    assert!(parse_err(&format!("{MINIMAL}[build]\nplatforms = []\n")).contains("`platforms` is empty"));
    assert!(parse_err(&format!("{MINIMAL}[build]\nplatforms = [\"freebsd-x64\"]\n")).contains("unknown platform"));

    let ws = WorkspaceManifest::parse("[workspace]\nmembers = []\n[workspace.build]\nplatforms = [\"windows-x64\"]\n", Path::new("/ws"))
        .unwrap();
    assert_eq!(ws.build.platforms.unwrap()[0].as_str(), "windows-x64");
}

#[test]
fn source_sets() {
    assert!(parse(MINIMAL).source_sets.is_empty());
    assert_eq!(parse(MINIMAL).layout.src, [PathBuf::from("src")]);
    assert_eq!(parse(MINIMAL).layout.target_sources(TargetKind::Jvm), [PathBuf::from("src-jvm")]);
    let m = parse(&format!(
        "{MINIMAL}[layout]\nsrc = [\"src/main/kotlin\", \"src/main/java\"]\ntests = \"src/test/kotlin\"\n\
         android-dir = \"src/main\"\n[layout.jvm]\nsrc = \"src/jvmMain/kotlin\"\n"
    ));
    assert_eq!(m.layout.src, [PathBuf::from("src/main/kotlin"), PathBuf::from("src/main/java")]);
    assert_eq!(m.layout.tests, [PathBuf::from("src/test/kotlin")]);
    assert_eq!(m.layout.android_dir, PathBuf::from("src/main"));
    assert_eq!(m.layout.target_sources(TargetKind::Jvm), [PathBuf::from("src/jvmMain/kotlin")]);
    assert_eq!(m.layout.target_sources(TargetKind::Wasm), [PathBuf::from("src-wasm")]);
    assert!(parse(MINIMAL).layout.source_sets.is_empty());
    let m = parse(&format!(
        "{MINIMAL}[target.jvm]\n[target.android]\nnamespace = \"com.example\"\n[target.native]\n\
         [source-sets.jvmAndAndroid]\ntargets = [\"jvm\", \"android\", \"jvm\"]\n\
         [source-sets.nonWeb]\ntargets = [\"jvm\", \"android\", \"native\"]\n"
    ));
    assert_eq!(m.source_sets.len(), 2);
    assert_eq!(m.source_sets[0].name, "jvmAndAndroid");
    assert_eq!(m.source_sets[0].targets, [TargetKind::Jvm, TargetKind::Android]);
    assert_eq!(m.source_sets[0].source_dir_name(), "src-jvmAndAndroid");

    // The wider set comes first, and the narrower one refines it; the target refines the narrowest.
    let jvm: Vec<_> = m.source_groups_for(TargetKind::Jvm).into_iter().map(|(g, parents)| (g.name.as_str(), parents)).collect();
    assert_eq!(jvm, [("nonWeb", vec![]), ("jvmAndAndroid", vec!["nonWeb"])]);
    let native: Vec<_> = m.source_groups_for(TargetKind::Native).into_iter().map(|(g, parents)| (g.name.as_str(), parents)).collect();
    assert_eq!(native, [("nonWeb", vec![])]);
    assert!(m.source_groups_for(TargetKind::Wasm).is_empty());
}

#[test]
fn source_sets_skip_the_levels_between() {
    let m = parse(&format!(
        "{MINIMAL}[target.jvm]\n[source-sets.a]\ntargets = [\"jvm\", \"android\"]\n\
         [source-sets.b]\ntargets = [\"jvm\", \"android\", \"native\"]\n\
         [source-sets.c]\ntargets = [\"jvm\", \"android\", \"native\", \"js\"]\n"
    ));
    let jvm: Vec<_> = m.source_groups_for(TargetKind::Jvm).into_iter().map(|(g, parents)| (g.name.as_str(), parents)).collect();
    assert_eq!(jvm, [("c", vec![]), ("b", vec!["c"]), ("a", vec!["b"])]);
}

#[test]
fn invalid_source_sets_are_rejected() {
    let with = |extra: &str| parse_err(&format!("{MINIMAL}[target.jvm]\n{extra}"));
    assert!(with("[source-sets.jvm]\ntargets = [\"jvm\", \"android\"]\n").contains("clashes with the source directories of a target"));
    assert!(with("[source-sets.common]\ntargets = [\"jvm\", \"android\"]\n").contains("clashes"));
    assert!(with("[source-sets.\"1x\"]\ntargets = [\"jvm\", \"android\"]\n").contains("invalid [source-sets] name"));
    assert!(with("[source-sets.x]\ntargets = [\"jvm\"]\n").contains("at least two different targets"));
    assert!(with("[source-sets.x]\ntargets = [\"jvm\", \"ios\"]\n").contains("unknown target `ios`"));
    assert!(with("[source-sets.x]\n").contains("at least two different targets"));
    assert!(
        with("[source-sets.x]\ntargets = [\"jvm\", \"js\"]\n[source-sets.y]\ntargets = [\"js\", \"jvm\"]\n")
            .contains("[source-sets.y] has the same targets as [source-sets.x]")
    );
    assert!(with("[source-sets.x]\ntargets = [\"jvm\", \"js\"]\nfoo = 1\n").contains("unknown field"));
}

#[test]
fn layout_is_validated() {
    let with = |extra: &str| parse_err(&format!("{MINIMAL}{extra}"));
    assert!(with("[layout]\nsrc = []\n").contains("`src` is empty"));
    assert!(with("[layout]\nsrc = \"/abs\"\n").contains("use a directory inside the package"));
    assert!(with("[layout]\nsrc = \"../out\"\n").contains("use a directory inside the package"));
    assert!(with("[layout]\nsrc = \"target/gen\"\n").contains("`target/` holds build output"));
    assert!(with("[layout.jvm]\nbenches = []\n").contains("`benches` is empty"));
    let m = parse(&format!(
        "{MINIMAL}[target.jvm]\n[target.wasm]\n[source-sets.web]\ntargets = [\"jvm\", \"wasm\"]\n[layout.source-sets.web]\nsrc = \"src/web\"\n"
    ));
    assert_eq!(m.layout.group_sources("web"), [PathBuf::from("src/web")]);
    assert!(with("[layout.source-sets.web]\nsrc = \"src/web\"\n").contains("names no [source-sets.web]"));
    assert!(with("[layout.source-sets.jvm]\nsrc = \"x\"\n").contains("clashes with a target"));
    assert!(
        with("[target.jvm]\n[target.wasm]\n[source-sets.web]\ntargets = [\"jvm\", \"wasm\"]\n[layout.source-sets.web]\nbenches = \"b\"\n")
            .contains("`benches` only exists")
    );
    assert!(with("[layout]\nfoo = \"x\"\n").contains("unknown field"));
}

#[test]
fn source_sets_of_native_platforms() {
    let m = parse(&format!(
        "{MINIMAL}[target.jvm]\n[target.native]\nplatforms = [\"host\"]\n\
         [source-sets.apple]\nplatforms = [\"apple\"]\n\
         [source-sets.ios]\nplatforms = [\"ios\"]\n\
         [source-sets.iosDevice]\nplatforms = [\"iosArm64\"]\n\
         [source-sets.linux]\ntargets = [\"native\"]\nplatforms = [\"linux-x64\", \"linux_arm64\"]\n\
         [source-sets.nonJs]\ntargets = [\"jvm\", \"native\"]\n"
    ));
    let apple = m.source_sets.iter().find(|g| g.name == "apple").unwrap();
    assert_eq!(apple.targets, [TargetKind::Native]);
    assert_eq!(
        apple.platforms,
        [
            "ios_arm64",
            "ios_simulator_arm64",
            "ios_x64",
            "macos_arm64",
            "macos_x64",
            "tvos_arm64",
            "tvos_simulator_arm64",
            "watchos_arm64",
            "watchos_simulator_arm64"
        ]
    );
    let names = |groups: Vec<(&SourceGroup, Vec<&str>)>| -> Vec<(String, Vec<String>)> {
        groups.into_iter().map(|(g, parents)| (g.name.clone(), parents.into_iter().map(str::to_owned).collect())).collect()
    };
    // A platform's sets nest by the platforms they hold; what is below `src-native/` refines no other set of its kind.
    assert_eq!(
        names(m.native_platform_groups(Some("ios_arm64"))),
        [("apple".into(), vec![]), ("ios".into(), vec!["apple".into()]), ("iosDevice".into(), vec!["ios".into()])]
    );
    assert_eq!(names(m.native_platform_groups(Some("linux_x64"))), [("linux".into(), vec![])]);
    assert!(m.native_platform_groups(Some("mingw_x64")).is_empty());
    assert_eq!(m.native_platform_groups(None).len(), 4);
    // The sets of whole targets leave the platform sets out, and `all_groups_for` has both.
    assert_eq!(names(m.source_groups_for(TargetKind::Native)), [("nonJs".into(), vec![])]);
    let all: Vec<&str> = m.all_groups_for(TargetKind::Native).iter().map(|g| g.name.as_str()).collect();
    assert_eq!(all, ["nonJs", "apple", "ios", "linux", "iosDevice"]);
    assert_eq!(names(m.source_groups_for(TargetKind::Jvm)), [("nonJs".into(), vec![])]);
}

#[test]
fn invalid_source_sets_of_native_platforms_are_rejected() {
    let with = |extra: &str| parse_err(&format!("{MINIMAL}[target.native]\n{extra}"));
    assert!(with("[source-sets.x]\nplatforms = [\"playstation\"]\n").contains("unknown Kotlin/Native platform `playstation`"));
    assert!(with("[source-sets.x]\nplatforms = [\"host\"]\n").contains("`host` is the machine you build on"));
    assert!(with("[source-sets.x]\ntargets = [\"jvm\"]\nplatforms = [\"apple\"]\n").contains("`targets` can only be [\"native\"]"));
    assert!(
        with("[source-sets.x]\nplatforms = [\"apple\"]\n[source-sets.y]\nplatforms = [\"macos\", \"ios\", \"tvos\", \"watchos\"]\n")
            .contains("has the same targets as")
    );
    assert!(with("[source-sets.x]\ntargets = [\"native\"]\n").contains("code for some Native platforms needs `platforms`"));
}

#[test]
fn android_plugins_keep_their_order() {
    let manifest = parse(&format!(
        "{MINIMAL}
[target.android]
namespace = \"a\"
[target.android.plugins]
zeta = {{ id = \"z.plugin\", version = \"1\" }}
alpha = {{ id = \"a.plugin\", apply = false }}
"
    ));
    let plugins = &manifest.targets.android.unwrap().config.plugins;
    assert_eq!(plugins.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["zeta", "alpha"]);
    assert_eq!(plugins[0].1.version.as_deref(), Some("1"));
    assert!(plugins[0].1.apply && !plugins[1].1.apply);
}
