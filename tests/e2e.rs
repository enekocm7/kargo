//! End-to-end tests that drive the real Kotlin compilers. They need a JDK and
//! network access (or a warm `~/.kargo` cache), so they only run when
//! `KARGO_E2E=1` is set:
//!
//! ```text
//! KARGO_E2E=1 cargo test --test e2e
//! KARGO_E2E=1 KARGO_E2E_NATIVE=1 cargo test --test e2e   # also Kotlin/Native (slow)
//! ```
//!
//! They share the user's Kargo cache so the compiler is downloaded only once.

mod common;

use std::process::Command as StdCommand;

use assert_cmd::Command;
use common::{e2e_enabled, which};
use predicates::prelude::*;

macro_rules! require_e2e {
    () => {
        if !e2e_enabled() {
            eprintln!("skipping: set KARGO_E2E=1 to run end-to-end tests");
            return;
        }
    };
}

struct E2e {
    dir: tempfile::TempDir,
}

impl E2e {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    fn kargo(&self, project: &str) -> Command {
        let mut cmd = Command::cargo_bin("kargo").unwrap();
        cmd.current_dir(self.dir.path().join(project)).env_remove("KARGO_TARGET_DIR").env("NO_COLOR", "1");
        cmd
    }

    fn create(&self, args: &[&str]) {
        let mut cmd = Command::cargo_bin("kargo").unwrap();
        cmd.current_dir(self.dir.path()).arg("new").args(args).arg("--no-vcs").assert().success();
    }

    fn write(&self, rel: &str, contents: &str) {
        let p = self.dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }
}

#[test]
fn jvm_lifecycle() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);

    t.kargo("app").arg("build").assert().success().stderr(predicate::str::contains("Compiling app v0.1.0 (jvm)"));
    // Unchanged sources are not recompiled.
    t.kargo("app").arg("build").assert().success().stderr(predicate::str::contains("Compiling").not());
    t.kargo("app").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
    t.kargo("app").arg("test").assert().success();
    t.kargo("app").args(["test", "--report", "junit"]).assert().success().stderr(predicate::str::contains("Reported JUnit XML"));
    let report = std::fs::read_to_string(t.dir.path().join("app/target/debug/jvm/test-reports/TEST-junit-jupiter.xml")).unwrap();
    assert!(report.contains("<testcase") && report.contains("GreetingTest"), "{report}");

    // A failing test fails the command.
    t.write("app/tests/FailTest.kt", "import kotlin.test.*\nclass FailTest { @Test fun nope() = assertEquals(1, 2) }\n");
    t.kargo("app").arg("test").assert().failure().code(101);
    std::fs::remove_file(t.dir.path().join("app/tests/FailTest.kt")).unwrap();

    // Test classes run whatever their name, and filters match `Class.method`.
    t.write("app/tests/T.kt", "import kotlin.test.*\nclass T {\n    @Test fun ok() {}\n    @Test fun bad() = assertEquals(1, 2)\n}\n");
    t.kargo("app").arg("test").assert().code(101).stdout(predicate::str::contains("bad()"));
    t.kargo("app").args(["test", "T.ok"]).assert().success().stdout(predicate::str::contains("1 tests successful"));
    t.kargo("app").args(["test", "T.bad"]).assert().code(101);
    t.kargo("app").args(["test", "nothing_matches"]).assert().code(101).stderr(predicate::str::contains("no JVM tests match"));
    std::fs::remove_file(t.dir.path().join("app/tests/T.kt")).unwrap();

    // The thin jar runs with its lib/ directory, the fat jar on its own.
    let jar = t.dir.path().join("app/target/debug/jvm/app-0.1.0.jar");
    let out = StdCommand::new("java").arg("-jar").arg(&jar).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Hello, Kotlin!"));
    let fat = t.kargo("app").args(["package", "-q"]).assert().success().get_output().stdout.clone();
    let fat = String::from_utf8(fat).unwrap();
    let out = StdCommand::new("java").arg("-jar").arg(fat.trim()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Hello, Kotlin!"));

    // Program arguments and exit codes are forwarded.
    t.write("app/src/Main.kt", "fun main(args: Array<String>) { println(args.joinToString(\"|\")); kotlin.system.exitProcess(7) }\n");
    t.kargo("app").args(["run", "--", "a b", "c"]).assert().code(7).stdout(predicate::str::contains("a b|c"));
}

#[test]
fn main_class_detection() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["mains"]);
    // `@JvmStatic fun main` in an object or companion object.
    t.write("mains/src/Main.kt", "package m\nobject Cli { @JvmStatic fun main(args: Array<String>) { println(\"object main\") } }\n");
    t.kargo("mains").arg("run").assert().success().stdout(predicate::str::contains("object main"));
    t.write(
        "mains/src/Main.kt",
        "package m\nclass App { companion object { @JvmStatic fun main(args: Array<String>) = println(\"companion main\") } }\n",
    );
    t.kargo("mains").arg("run").assert().success().stdout(predicate::str::contains("companion main"));

    // Several mains are reported by name.
    t.write("mains/src/Main.kt", "package m\nfun main() {}\n");
    t.write("mains/src/Other.kt", "package m.other\nfun main() {}\n");
    t.kargo("mains")
        .arg("run")
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("found multiple `main` functions"))
        .stderr(predicate::str::contains("m.MainKt"))
        .stderr(predicate::str::contains("m.other.OtherKt"));
}

#[test]
fn compile_errors_are_reported() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["broken"]);
    t.write("broken/src/Main.kt", "fun main() { val x: Int = \"nope\" }\n");
    t.kargo("broken").arg("build").assert().failure().code(101).stderr(predicate::str::contains("could not compile `broken`"));
}

#[test]
fn incremental_compilation() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["inc"]);
    std::fs::remove_file(t.dir.path().join("inc/tests/GreetingTest.kt")).unwrap();
    let run = |expected: &str| {
        t.kargo("inc").args(["run", "-q"]).assert().success().stdout(predicate::str::contains(expected));
    };
    t.write("inc/src/Main.kt", "package com.example.inc\nfun main() = println(\"v=\" + value() + \" k=\" + K + \" s=\" + area())\n");
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(): Int = 1\n");
    t.write("inc/src/Const.kt", "package com.example.inc\nconst val K = 10\n");
    t.write("inc/src/Shape.kt", "package com.example.inc\nfun area(): Int = 9\n");
    run("v=1 k=10 s=9");
    assert!(t.dir.path().join("inc/target/debug/jvm/.fingerprint/inc.ic/caches").is_dir(), "incremental caches are kept");

    // A body change, and a constant inlined into another file.
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(): Int = 2\n");
    t.write("inc/src/Const.kt", "package com.example.inc\nconst val K = 11\n");
    run("v=2 k=11 s=9");

    // A signature change breaks the caller, which is recompiled and reported.
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(x: Int): Int = x\n");
    t.kargo("inc").arg("build").assert().failure().code(101).stderr(predicate::str::contains("src/Main.kt:2:"));
    t.write("inc/src/Main.kt", "package com.example.inc\nfun main() = println(\"v=\" + value(5) + \" k=\" + K + \" s=\" + area())\n");
    run("v=5 k=11 s=9");

    // Deleting a file removes its classes.
    std::fs::remove_file(t.dir.path().join("inc/src/Shape.kt")).unwrap();
    t.write("inc/src/Main.kt", "package com.example.inc\nfun main() = println(\"v=\" + value(5) + \" k=\" + K)\n");
    run("v=5 k=11");
    assert!(!t.dir.path().join("inc/target/debug/jvm/classes/com/example/inc/ShapeKt.class").exists());

    // Tests see changes to `internal` main declarations.
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(x: Int): Int = x\ninternal fun secret() = 41\n");
    t.write(
        "inc/tests/SecretTest.kt",
        "package com.example.inc\nimport kotlin.test.*\nclass SecretTest { @Test fun s() = assertEquals(41, secret()) }\n",
    );
    t.kargo("inc").arg("test").assert().success();
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(x: Int): Int = x\ninternal fun secret() = 42\n");
    t.kargo("inc").arg("test").assert().failure().code(101);

    // KARGO_INCREMENTAL=false compiles from scratch and drops the caches.
    t.write("inc/src/Util.kt", "package com.example.inc\nfun value(x: Int): Int = x + 1\n");
    std::fs::remove_file(t.dir.path().join("inc/tests/SecretTest.kt")).unwrap();
    t.kargo("inc").args(["run", "-q"]).env("KARGO_INCREMENTAL", "false").assert().success().stdout(predicate::str::contains("v=6"));
    assert!(!t.dir.path().join("inc/target/debug/jvm/.fingerprint/inc.ic").exists());
    run("v=6 k=11");
}

#[test]
fn daemon_matches_standalone_compiler() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["both"]);
    let diagnostics = |daemon: &str| {
        let out = t.kargo("both").arg("build").env("KARGO_DAEMON", daemon).assert().failure().code(101).get_output().stderr.clone();
        // Compilers print paths with the platform's separator.
        String::from_utf8_lossy(&out).lines().filter(|l| l.contains("error")).map(|l| l.replace('\\', "/")).collect::<Vec<_>>()
    };
    t.write("both/src/Main.kt", "fun main() {\n    val x: Int = \"nope\"\n}\n");
    let standalone = diagnostics("false");
    assert!(standalone.iter().any(|l| l.starts_with("src/Main.kt:2:16: error:")), "{standalone:?}");
    assert_eq!(diagnostics("true"), standalone, "the daemon reports the same Kotlin diagnostics, with relative paths");

    // `--message-format json` turns them into JSON messages on stdout, with or without the daemon.
    let main = t.dir.path().join("both/src/Main.kt").canonicalize().unwrap();
    for daemon in ["false", "true"] {
        let out = t
            .kargo("both")
            .args(["build", "--message-format", "json"])
            .env("KARGO_DAEMON", daemon)
            .assert()
            .failure()
            .code(101)
            .get_output()
            .stdout
            .clone();
        let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&out).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let error = messages.iter().find(|m| m["reason"] == "compiler-message" && m["severity"] == "error").expect("an error message");
        assert_eq!((error["line"].as_u64(), error["column"].as_u64()), (Some(2), Some(16)), "{error}");
        assert_eq!(std::path::Path::new(error["file"].as_str().unwrap()).canonicalize().unwrap(), main);
        assert!(error["rendered"].as_str().unwrap().contains("val x: Int = \"nope\""), "{error}");
        assert_eq!(messages.last().unwrap()["reason"], "build-finished");
    }

    t.write("both/src/Main.kt", "fun main() = println(J.s())\n");
    t.write("both/src/J.java", "public class J { static String s() { return 1; } }\n");
    let from_daemon = diagnostics("true");
    assert!(from_daemon.iter().any(|l| l.starts_with("src/J.java:1: error:")), "{from_daemon:?}");

    // Kotlin and Java sources compile and run through the daemon.
    t.write("both/src/Main.kt", "fun main() = println(\"from \" + J.s())\n");
    t.write("both/src/J.java", "public class J { static String s() { return \"java\"; } }\n");
    t.kargo("both").arg("run").assert().success().stdout(predicate::str::contains("from java"));
    t.kargo("both").args(["daemon", "status"]).assert().success().stdout(predicate::str::contains("pid"));
}

#[test]
fn dependencies_and_hooks() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["deps", "--with", "serialization,coroutines"]);
    t.kargo("deps").args(["add", "org.jetbrains.kotlinx:kotlinx-datetime:0.7.1"]).assert().success();
    let hook = if cfg!(windows) { "echo %KARGO_HOOK% > hook.txt" } else { "echo $KARGO_HOOK > hook.txt" };
    let manifest = std::fs::read_to_string(t.dir.path().join("deps/kargo.toml")).unwrap();
    t.write("deps/kargo.toml", &format!("{manifest}\nafter = {{ run = '{hook}', hooks = [\"post-build\"] }}\n"));
    t.write(
        "deps/src/Main.kt",
        r#"package com.example.deps

import kotlinx.coroutines.runBlocking
import kotlinx.datetime.LocalDate
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class Event(val name: String, val day: String)

fun greeting(name: String) = "Hello, $name!"

fun main() = runBlocking {
    println(Json.encodeToString(Event.serializer(), Event("launch", LocalDate(2026, 1, 2).toString())))
}
"#,
    );
    t.kargo("deps").arg("run").assert().success().stdout(predicate::str::contains(r#"{"name":"launch","day":"2026-01-02"}"#));
    assert_eq!(std::fs::read_to_string(t.dir.path().join("deps/hook.txt")).unwrap().trim(), "post-build");
    assert!(t.dir.path().join("deps/kargo.lock").is_file());
    t.kargo("deps").args(["build", "--locked"]).assert().success();
}

#[test]
fn multiplatform_expect_actual_on_wasm() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["mpp", "--target", "jvm,wasm"]);
    t.write("mpp/src/Platform.kt", "package com.example.mpp\n\nexpect fun platform(): String\n");
    t.write("mpp/src-jvm/Platform.kt", "package com.example.mpp\n\nactual fun platform() = \"jvm\"\n");
    t.write("mpp/src-wasm/Platform.kt", "package com.example.mpp\n\nactual fun platform() = \"wasm\"\n");
    t.write(
        "mpp/src/Main.kt",
        "package com.example.mpp\n\nfun greeting(n: String) = \"Hello, $n!\"\n\nfun main() = println(greeting(platform()))\n",
    );
    t.kargo("mpp").arg("run").assert().success().stdout(predicate::str::contains("Hello, jvm!"));
    t.kargo("mpp").args(["run", "--target", "wasm"]).assert().success().stdout(predicate::str::contains("Hello, wasm!"));
    t.kargo("mpp")
        .args(["test", "--target", "wasm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GreetingTest.greetsByName ... ok"))
        .stdout(predicate::str::contains("Hello, wasm!").not());
    t.kargo("mpp").args(["test", "--target", "wasm", "--report", "junit"]).assert().success();
    let report = std::fs::read_to_string(t.dir.path().join("mpp/target/debug/wasm/test-reports/TEST-wasm.xml")).unwrap();
    assert!(report.contains(r#"<testcase name="greetsByName""#), "{report}");
}

#[test]
fn check_type_checks_without_packaging() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["chk", "--target", "jvm,wasm"]);
    t.kargo("chk")
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("Checking chk v0.1.0 (jvm)"))
        .stderr(predicate::str::contains("Checking chk v0.1.0 (wasm)"));
    let target = t.dir.path().join("chk/target/debug");
    assert!(!target.join("jvm/chk-0.1.0.jar").exists(), "check does not package a jar");
    assert!(!target.join("wasm/chk.mjs").exists() && !target.join("check/wasm/chk.mjs").exists(), "check does not link Wasm");
    assert!(target.join("check/wasm/klib/chk.klib").is_file());
    // Unchanged sources are not checked again, and the JVM classes are shared with `kargo build`.
    t.kargo("chk").arg("check").assert().success().stderr(predicate::str::contains("Checking").not());
    t.kargo("chk").args(["build", "-t", "jvm"]).assert().success().stderr(predicate::str::contains("Compiling").not());
    assert!(target.join("jvm/chk-0.1.0.jar").is_file());

    // Tests are only checked with --tests.
    t.write("chk/tests/Broken.kt", "class Broken { val x: Int = \"no\" }\n");
    t.kargo("chk").arg("check").assert().success();
    t.kargo("chk").args(["check", "--tests", "-t", "jvm"]).assert().failure().code(101).stderr(predicate::str::contains("Broken.kt"));
    t.kargo("chk").args(["check", "--tests", "-t", "wasm"]).assert().failure().code(101).stderr(predicate::str::contains("Broken.kt"));
    std::fs::remove_file(t.dir.path().join("chk/tests/Broken.kt")).unwrap();
    t.kargo("chk").args(["check", "--tests"]).assert().success().stderr(predicate::str::contains("(jvm, tests)"));

    // Type errors in the main sources fail the check.
    t.write("chk/src/Bad.kt", "val bad: String = 1\n");
    t.kargo("chk").args(["check", "-t", "wasm"]).assert().failure().code(101).stderr(predicate::str::contains("Bad.kt"));
}

#[test]
fn doc_generates_html_from_kdoc() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["lib", "--lib"]);
    t.kargo("lib")
        .arg("doc")
        .assert()
        .success()
        .stderr(predicate::str::contains("Documenting lib v0.1.0 (jvm)"))
        .stderr(predicate::str::contains("Generated"));
    let page = std::fs::read_to_string(t.dir.path().join("lib/target/debug/doc/lib/com.example.lib/greeting.html")).unwrap();
    assert!(page.contains("Returns a friendly greeting."));

    // Several targets: `src/` becomes a common source set, `src-<target>/` the platform ones.
    t.create(&["mpp-doc", "--lib", "--target", "jvm,wasm"]);
    t.write("mpp-doc/src/Platform.kt", "package com.example.mpp_doc\n\n/** The platform name. */\nexpect fun platform(): String\n");
    t.write("mpp-doc/src-jvm/Platform.kt", "package com.example.mpp_doc\n\nactual fun platform() = \"jvm\"\n");
    t.write("mpp-doc/src-wasm/Platform.kt", "package com.example.mpp_doc\n\nactual fun platform() = \"wasm\"\n");
    t.write("mpp-doc/docs/module.md", "# Module mpp-doc\n\nDocumented on every platform.\n");
    let manifest = t.dir.path().join("mpp-doc/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[doc]\nincludes = [\"docs/module.md\"]\n")).unwrap();
    t.kargo("mpp-doc").arg("doc").assert().success().stderr(predicate::str::contains("(common, jvm, wasm)"));
    let index = std::fs::read_to_string(t.dir.path().join("mpp-doc/target/debug/doc/index.html")).unwrap();
    assert!(index.contains("Documented on every platform."));
}

#[test]
fn fmt_and_lint_with_ktlint_and_detekt() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["style"]);
    // A new project is already formatted and lint-free.
    t.kargo("style").args(["fmt", "--check"]).assert().success().stderr(predicate::str::contains("Checking formatting of style v0.1.0"));
    t.kargo("style").arg("lint").assert().success().stderr(predicate::str::contains("Linting style v0.1.0"));

    t.write("style/src/Messy.kt", "package com.example.style\n\nfun   messy( ) =  listOf(1,2)\n");
    t.write("style/src/gen/Generated.kt", "package com.example.style\n\nfun   generated( ) =  1\n");
    let manifest = t.dir.path().join("style/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[fmt]\nexclude = [\"src/gen/**\"]\n")).unwrap();
    t.kargo("style")
        .args(["fmt", "--check"])
        .assert()
        .code(101)
        .stdout(predicate::str::contains("src/Messy.kt:3"))
        .stdout(predicate::str::contains("Generated.kt").not())
        .stderr(predicate::str::contains("formatting check failed"));
    t.kargo("style").arg("fmt").assert().success();
    let messy = std::fs::read_to_string(t.dir.path().join("style/src/Messy.kt")).unwrap();
    assert!(messy.contains("fun messy() = listOf(1, 2)"), "{messy}");
    let generated = std::fs::read_to_string(t.dir.path().join("style/src/gen/Generated.kt")).unwrap();
    assert!(generated.contains("fun   generated( )"), "excluded files are left alone: {generated}");
    t.kargo("style").args(["fmt", "--check"]).assert().success();

    // detekt issues fail `kargo lint`; detekt.yml and a baseline silence them.
    t.write("style/src/gen/Generated.kt", "package com.example.style\n\nfun generated() = 1\n");
    t.kargo("style").arg("lint").assert().code(101).stdout(predicate::str::contains("FunctionOnlyReturningConstant"));
    t.write("style/detekt.yml", "style:\n  FunctionOnlyReturningConstant:\n    active: false\n");
    t.kargo("style").arg("lint").assert().success();
    std::fs::remove_file(t.dir.path().join("style/detekt.yml")).unwrap();
    t.kargo("style").args(["lint", "--create-baseline"]).assert().success().stderr(predicate::str::contains("detekt-baseline.xml"));
    assert!(t.dir.path().join("style/detekt-baseline.xml").is_file());
    t.kargo("style").arg("lint").assert().success();
}

#[test]
fn doc_android_library() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs Gradle and the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid-doc", "--lib", "--target", "android"]);
    t.write(
        "droid-doc/src-android/Toasts.kt",
        "package com.example.droid_doc\n\nimport android.content.Context\n\n/** Shows [text] in a toast. */\nfun toast(context: Context, text: String) =\n    android.widget.Toast.makeText(context, text, android.widget.Toast.LENGTH_SHORT).show()\n",
    );
    t.kargo("droid-doc").arg("doc").assert().success().stderr(predicate::str::contains("Documenting droid-doc v0.1.0 (android)"));
    let page = std::fs::read_to_string(t.dir.path().join("droid-doc/target/debug/doc/droid-doc/com.example.droid_doc/toast.html")).unwrap();
    assert!(page.contains("Shows"));
    // `android.jar` is on the classpath, so `Context` resolves and links to the Android reference.
    assert!(page.contains("developer.android.com/reference/kotlin/android/content/Context"), "{page}");
}

#[test]
fn wasm_installs_npm_packages_of_dependencies() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["dt", "--target", "wasm"]);
    // kotlinx-datetime imports the npm package @js-joda/core on Wasm.
    t.kargo("dt").args(["add", "org.jetbrains.kotlinx:kotlinx-datetime:0.7.1"]).assert().success();
    t.kargo("dt").args(["add", "org.jetbrains.kotlinx:kotlinx-serialization-core:1.9.0"]).assert().success();
    t.write(
        "dt/src/Main.kt",
        "package com.example.dt\n\nimport kotlinx.datetime.*\n\nfun greeting(n: String) = \"Hello, $n!\"\n\n\
         fun main() = println(LocalDate(2024, 1, 31).plus(1, DateTimeUnit.MONTH))\n",
    );
    t.kargo("dt")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Installing npm packages @js-joda/core@"))
        .stdout(predicate::str::contains("2024-02-29"));
    assert!(t.dir.path().join("dt/target/debug/node_modules/@js-joda/core").is_dir());
    // Already installed: no second `npm install`.
    t.kargo("dt").arg("run").assert().success().stderr(predicate::str::contains("Installing").not());
}

#[test]
fn spring_boot_with_lombok() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["sb", "--with", "spring-boot,lombok"]);
    t.write(
        "sb/tests/PersonTest.kt",
        "package com.example.sb\n\nimport kotlin.test.*\n\nclass PersonTest {\n    @Test fun accessors() {\n        val p = Person()\n        p.name = \"Ada\"\n        assertEquals(\"Ada\", p.name)\n    }\n}\n",
    );
    t.kargo("sb").arg("test").assert().success();
}

#[test]
fn java_annotation_processor() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["apt"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("apt/kargo.toml")).unwrap();
    t.write(
        "apt/kargo.toml",
        &format!(
            "{manifest}
[target.jvm]
javac-args = [\"-Xlint:-processing\"]

[target.jvm.dependencies]
             \"com.google.auto.value:auto-value-annotations\" = {{ version = \"1.11.0\", scope = \"provided\" }}
             \"com.google.auto.value:auto-value\" = {{ version = \"1.11.0\", scope = \"processor\" }}
"
        ),
    );
    t.write(
        "apt/src/Person.java",
        "package com.example.apt;
import com.google.auto.value.AutoValue;
@AutoValue public abstract class Person {
    public abstract String name();
    public static Person create(String n) { return new AutoValue_Person(n); }
}
",
    );
    t.write(
        "apt/src/Main.kt",
        "package com.example.apt

fun main() = println(Person.create(\"Ada\"))
",
    );
    t.kargo("apt").arg("run").assert().success().stdout(predicate::str::contains("Person{name=Ada}"));
    // The processor is not packaged.
    let lib = t.dir.path().join("apt/target/debug/jvm/lib");
    assert!(!std::fs::read_dir(lib).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("auto-value")));
}

#[test]
fn ksp_processor() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["ksp"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("ksp/kargo.toml")).unwrap();
    t.write(
        "ksp/kargo.toml",
        &format!(
            "{manifest}
[dependencies]
\"com.squareup.moshi:moshi\" = \"1.15.2\"
\"com.squareup.moshi:moshi-kotlin-codegen\" = {{ version = \"1.15.2\", scope = \"ksp\" }}
"
        ),
    );
    t.write(
        "ksp/src/Main.kt",
        "package com.example.ksp

import com.squareup.moshi.JsonClass
import com.squareup.moshi.Moshi

@JsonClass(generateAdapter = true)
data class Person(val name: String, val age: Int)

fun greeting(name: String) = \"Hello, $name!\"

fun main() = println(Moshi.Builder().build().adapter(Person::class.java).toJson(Person(\"Ada\", 36)))
",
    );
    t.write(
        "ksp/tests/PersonTest.kt",
        "package com.example.ksp

import com.squareup.moshi.JsonClass
import kotlin.test.Test
import kotlin.test.assertEquals

@JsonClass(generateAdapter = true)
data class Pet(val name: String)

class PersonTest {
    @Test fun generatedAdapters() {
        assertEquals(\"PersonJsonAdapter\", Class.forName(\"com.example.ksp.PersonJsonAdapter\").simpleName)
        assertEquals(\"PetJsonAdapter\", Class.forName(\"com.example.ksp.PetJsonAdapter\").simpleName)
    }
}
",
    );
    t.kargo("ksp")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Processing ksp v0.1.0 (jvm, ksp)"))
        .stdout(predicate::str::contains(r#"{"name":"Ada","age":36}"#));
    assert!(t.dir.path().join("ksp/target/debug/jvm/generated/ksp/main/kotlin/com/example/ksp/PersonJsonAdapter.kt").is_file());
    // Processors are not packaged.
    let lib = t.dir.path().join("ksp/target/debug/jvm/lib");
    assert!(!std::fs::read_dir(lib).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("codegen")));
    // Unchanged sources are not processed again.
    t.kargo("ksp").arg("build").assert().success().stderr(predicate::str::contains("Processing").not());
    t.kargo("ksp").arg("test").assert().success().stderr(predicate::str::contains("Processing ksp v0.1.0 (jvm, tests, ksp)"));
    assert!(std::fs::read_to_string(t.dir.path().join("ksp/kargo.lock")).unwrap().contains("[resolution.\"jvm/ksp\"]"));
}

#[test]
fn ktor_and_compose_compile() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["kt", "--with", "ktor,serialization"]);
    t.kargo("kt").arg("test").assert().success();
    t.create(&["ui", "--with", "compose"]);
    t.kargo("ui").arg("build").assert().success();
}

#[test]
fn native_executable_and_tests() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_NATIVE").is_none() {
        eprintln!("skipping: set KARGO_E2E_NATIVE=1 to run Kotlin/Native tests");
        return;
    }
    let t = E2e::new();
    t.create(&["nat", "--target", "native", "--with", "coroutines"]);
    t.kargo("nat").args(["check", "--tests"]).assert().success().stderr(predicate::str::contains("Checking nat v0.1.0 (native, "));
    assert!(!t.dir.path().join("nat/target/debug/native").exists(), "check does not link an executable");
    t.kargo("nat").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
    t.kargo("nat").args(["test", "--report", "junit"]).assert().success().stdout(predicate::str::contains("1 passed"));
    let report = std::fs::read_to_string(t.dir.path().join("nat/target/debug/native/test-reports/TEST-native.xml")).unwrap();
    assert!(report.contains("<testcase") && report.contains(r#"failures="0""#), "{report}");
}

#[test]
fn native_cinterop() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_NATIVE").is_none() {
        eprintln!("skipping: set KARGO_E2E_NATIVE=1 to run Kotlin/Native tests");
        return;
    }
    let t = E2e::new();
    t.create(&["cin", "--target", "native"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("cin/kargo.toml")).unwrap();
    t.write(
        "cin/kargo.toml",
        &format!("{manifest}\n[target.native.cinterop.mathx]\npackage = \"mathx\"\ncompiler-options = [\"-Iinclude\"]\n"),
    );
    t.write("cin/include/twice.h", "static inline int twice(int x) { return 2 * x; }\n");
    t.write("cin/interop/mathx.def", "headers = twice.h\n---\nstatic int add(int a, int b) { return a + b; }\n");
    t.write(
        "cin/src/Main.kt",
        "import kotlinx.cinterop.ExperimentalForeignApi\n\n@OptIn(ExperimentalForeignApi::class)\nfun main() = println(\"answer \" + mathx.twice(mathx.add(20, 1)))\n",
    );
    t.write(
        "cin/tests/GreetingTest.kt",
        "import kotlinx.cinterop.ExperimentalForeignApi\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\n\
         @OptIn(ExperimentalForeignApi::class)\nclass InteropTest {\n    @Test\n    fun callsC() = assertEquals(4, mathx.add(2, 2))\n}\n",
    );
    t.kargo("cin")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating mathx C bindings"))
        .stdout(predicate::str::contains("answer 42"));
    // Up to date: the bindings are not regenerated.
    t.kargo("cin").arg("build").assert().success().stderr(predicate::str::contains("Generating").not());
    t.kargo("cin").arg("test").assert().success().stdout(predicate::str::contains("1 passed"));
}

#[test]
fn android_app_with_compose() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs Gradle and the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid", "--target", "android", "--with", "compose"]);
    t.kargo("droid")
        .args(["check", "--tests"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Checking droid v0.1.0 (android, gradle, tests)"));
    assert!(!t.dir.path().join("droid/target/android/build/outputs/apk").exists(), "check does not package an APK");
    t.kargo("droid").arg("build").assert().success();
    let apk = t.dir.path().join("droid/target/android/build/outputs/apk/debug/droid-debug.apk");
    assert!(apk.is_file());
    // The app's own code must be dexed into the APK, not only the libraries.
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&apk).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).filter(|n| n.ends_with(".dex")).collect();
    let dexed = names.iter().any(|name| {
        let mut dex = Vec::new();
        std::io::Read::read_to_end(&mut zip.by_name(name).unwrap(), &mut dex).unwrap();
        dex.windows(b"MainActivity".len()).any(|w| w == b"MainActivity")
    });
    assert!(dexed, "MainActivity is missing from {}", apk.display());

    t.kargo("droid").args(["test", "--report", "junit"]).assert().success().stdout(predicate::str::contains("1 passed"));
    let reports = std::fs::read_dir(t.dir.path().join("droid/target/debug/android/test-reports")).unwrap().count();
    assert!(reports > 0, "Gradle's JUnit XML reports are copied");
    // A failing test must fail `kargo test`.
    let test = t.dir.path().join("droid/tests/GreetingTest.kt");
    let source = std::fs::read_to_string(&test).unwrap();
    std::fs::write(&test, source.replace("\"Hello, Kargo!\"", "\"WRONG\"")).unwrap();
    t.kargo("droid").arg("test").assert().code(101).stdout(predicate::str::contains("1 failed"));

    // Type errors fail `kargo check`.
    t.write("droid/src/Bad.kt", "val bad: String = 1\n");
    t.kargo("droid").arg("check").assert().failure().code(101);
}

#[test]
fn pinned_jdk_is_downloaded_and_used() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["pinned"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("pinned/kargo.toml")).unwrap();
    t.write("pinned/kargo.toml", &format!("{manifest}\n[toolchain]\njdk = \"17\"\n"));
    t.write("pinned/src/Main.kt", "fun main() = println(\"java \" + System.getProperty(\"java.specification.version\"))\n");
    t.kargo("pinned").arg("run").assert().success().stdout(predicate::str::contains("java 17"));
    t.kargo("pinned").args(["toolchain", "which"]).assert().success().stdout(predicate::str::contains("JDK 17"));
}

#[test]
fn path_dependencies_and_workspaces() {
    require_e2e!();
    let t = E2e::new();
    let package = |name: &str| format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[target.jvm]\n\n[target.wasm]\n");
    t.write("mathlib/kargo.toml", &format!("{}\n[frameworks]\ncoroutines = true\n", package("mathlib")));
    t.write(
        "mathlib/src/Lib.kt",
        "package mathlib\n\nimport kotlinx.coroutines.Dispatchers\n\nfun add(a: Int, b: Int): Int = a + b\n\nfun dispatcher(): String = Dispatchers.Default.toString()\n",
    );
    t.write("calc/kargo.toml", &package("calc"));
    t.write(
        "calc/src/Main.kt",
        "import mathlib.add\nimport mathlib.dispatcher\n\nfun main() {\n    dispatcher()\n    println(\"2 + 3 = ${add(2, 3)}\")\n}\n",
    );
    t.write(
        "calc/tests/CalcTest.kt",
        "import kotlin.test.*\n\nclass CalcTest {\n    @Test fun adds() = assertEquals(5, mathlib.add(2, 3))\n}\n",
    );

    // A single package with a path dependency: the dependency builds first, under target/<profile>/deps/.
    t.kargo("calc").args(["add", "--path", "../mathlib"]).assert().success();
    t.kargo("calc")
        .args(["run", "-t", "jvm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 + 3 = 5"))
        .stderr(predicate::str::contains("Compiling mathlib v0.1.0 (jvm)").and(predicate::str::contains("Compiling calc v0.1.0 (jvm)")));
    assert!(t.dir.path().join("calc/target/debug/deps/mathlib/jvm/mathlib-0.1.0.jar").is_file());
    let lock = std::fs::read_to_string(t.dir.path().join("calc/kargo.lock")).unwrap();
    assert!(lock.contains("[resolution.\"mathlib/jvm/compile\"]") && lock.contains("kotlinx-coroutines-core"), "{lock}");
    t.kargo("calc").args(["build", "-t", "jvm"]).assert().success().stderr(predicate::str::contains("Compiling").not());

    // Changing the dependency rebuilds it and its consumer.
    t.write("mathlib/src/Lib.kt", &std::fs::read_to_string(t.dir.path().join("mathlib/src/Lib.kt")).unwrap().replace("a + b", "b + a"));
    t.kargo("calc")
        .args(["build", "-t", "jvm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Compiling mathlib v0.1.0 (jvm)").and(predicate::str::contains("Compiling calc v0.1.0 (jvm)")));

    // Wasm links the dependency's klib; tests and the fat jar see it too.
    t.kargo("calc").args(["run", "-t", "wasm"]).assert().success().stdout(predicate::str::contains("2 + 3 = 5"));
    t.kargo("calc").arg("test").assert().success();
    let fat = t.kargo("calc").args(["package", "-q"]).assert().success().get_output().stdout.clone();
    let out = StdCommand::new("java").arg("-jar").arg(String::from_utf8(fat).unwrap().trim()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 + 3 = 5"));

    // The same packages as a workspace: one kargo.lock and one target/ at the root.
    t.write("kargo.toml", "[workspace]\nmembers = [\"mathlib\", \"calc\"]\n");
    t.kargo("").args(["build", "-t", "jvm"]).assert().success();
    assert!(t.dir.path().join("target/debug/mathlib/jvm/mathlib-0.1.0.jar").is_file());
    assert!(t.dir.path().join("target/debug/calc/jvm/calc-0.1.0.jar").is_file());
    assert!(std::fs::read_to_string(t.dir.path().join("kargo.lock")).unwrap().contains("[resolution.\"calc/jvm/runtime\"]"));
    t.kargo("").args(["run", "-p", "calc", "-t", "jvm"]).assert().success().stdout(predicate::str::contains("2 + 3 = 5"));
    t.kargo("").args(["test", "--workspace", "-t", "jvm"]).assert().success();
}

#[test]
fn native_path_dependency() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_NATIVE").is_none() {
        eprintln!("skipping: set KARGO_E2E_NATIVE=1 to run Kotlin/Native tests");
        return;
    }
    let t = E2e::new();
    // The dependency's own kind is an executable; consumers still link a klib of it.
    t.write("nlib/kargo.toml", "[package]\nname = \"nlib\"\nversion = \"0.1.0\"\n\n[target.native]\n");
    t.write("nlib/src/Lib.kt", "package nlib\n\nfun greet(name: String) = \"Hello, $name!\"\n");
    t.write(
        "ncalc/kargo.toml",
        "[package]\nname = \"ncalc\"\nversion = \"0.1.0\"\n\n[target.native]\n\n[dependencies]\nnlib = { path = \"../nlib\" }\n",
    );
    t.write("ncalc/src/Main.kt", "fun main() = println(nlib.greet(\"native\"))\n");
    t.kargo("ncalc")
        .arg("run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello, native!"))
        .stderr(predicate::str::contains("Compiling nlib v0.1.0 (native, "));
}
