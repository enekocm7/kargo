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
fn compile_errors_are_reported() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["broken"]);
    t.write("broken/src/Main.kt", "fun main() { val x: Int = \"nope\" }\n");
    t.kargo("broken").arg("build").assert().failure().code(101).stderr(predicate::str::contains("could not compile `broken`"));
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
    t.kargo("nat").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
    t.kargo("nat").arg("test").assert().success().stdout(predicate::str::contains("1 passed"));
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
    t.kargo("droid").arg("build").assert().success();
    assert!(t.dir.path().join("droid/target/android/build/outputs/apk/debug/droid-debug.apk").is_file());
    t.kargo("droid").arg("test").assert().success();
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
