//! End-to-end tests that drive the real Kotlin compilers. They need a JDK and
//! network access (or a warm `~/.kargo` cache), so they only run when
//! `KARGO_E2E=1` is set:
//!
//! ```text
//! KARGO_E2E=1 cargo test --test e2e
//! KARGO_E2E=1 KARGO_E2E_NATIVE=1 cargo test --test e2e   # also Kotlin/Native (slow)
//! KARGO_E2E=1 KARGO_E2E_GRAALVM=1 cargo test --test e2e   # also GraalVM native images (downloads GraalVM)
//! ```
//!
//! They share the user's Kargo cache so the compiler is downloaded only once.

mod common;

use std::path::PathBuf;
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

macro_rules! require_native {
    () => {
        if std::env::var_os("KARGO_E2E_NATIVE").is_none() {
            eprintln!("skipping: set KARGO_E2E_NATIVE=1 to run Kotlin/Native tests");
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
fn jvm_target_spellings_and_limits() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    t.write("app/src/Main.kt", "fun main() { println(Legacy.greeting()) }\n");
    t.write("app/src/Legacy.java", "public class Legacy { public static String greeting() { return \"hi\"; } }\n");
    let with_target = |target: &str| {
        t.write("app/kargo.toml", &format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.jvm]\njvm-target = \"{target}\"\n"));
    };
    let class_version = |name: &str| {
        let bytes = std::fs::read(t.dir.path().join("app/target/debug/jvm/classes").join(name)).unwrap();
        u16::from_be_bytes([bytes[6], bytes[7]])
    };

    // Kotlin only knows Java 8 as `1.8` while javac wants `8`; Kargo takes either spelling.
    for (target, version) in [("8", 52), ("1.8", 52), ("17", 61)] {
        with_target(target);
        t.kargo("app").arg("build").assert().success();
        assert_eq!(class_version("MainKt.class"), version, "Kotlin classes for jvm-target {target}");
        assert_eq!(class_version("Legacy.class"), version, "Java classes for jvm-target {target}");
    }

    // A bad value, or one the JDK can't compile for, fails before the compilers start.
    with_target("abc");
    t.kargo("app").arg("build").assert().code(101).stderr(predicate::str::contains("invalid [target.jvm] jvm-target `abc`"));
    with_target("99");
    t.kargo("app")
        .arg("build")
        .assert()
        .code(101)
        .stderr(predicate::str::contains("jvm-target 99 is newer than JDK"))
        .stderr(predicate::str::contains("[toolchain] jdk = \"99\""))
        .stderr(predicate::str::contains("Compiling").not());
}

#[test]
fn jvm_benchmarks() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app", "--bench"]);
    // Quick settings: this checks the plumbing, not the numbers.
    let quick = ["--warmup", "1x200ms", "--iterations", "2x200ms", "--forks", "1"];

    t.kargo("app")
        .arg("bench")
        .args(quick)
        .args(["--save", "first"])
        .assert()
        .success()
        .stdout(predicate::str::contains("com.example.app.SampleBench.sum").and(predicate::str::contains("avgt")))
        .stderr(predicate::str::contains("Compiling app v0.1.0 (jvm, benches)").and(predicate::str::contains("Saved baseline `first`")))
        // Benchmarks are measured optimized.
        .stderr(predicate::str::contains("Finished `release` profile"));
    assert!(t.dir.path().join("app/target/bench/first.json").is_file());

    // A second run compares with the first, and the unchanged benchmarks are not rebuilt.
    t.kargo("app")
        .arg("bench")
        .args(quick)
        .args(["sumOfSquares", "--baseline", "first"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("SampleBench.sumOfSquares")
                .and(predicate::str::contains(" -> "))
                .and(predicate::str::contains("SampleBench.sum ").not()),
        )
        .stderr(predicate::str::contains("Compiling").not());

    let json = t.kargo("app").arg("bench").args(quick).args(["--format", "json", "sum"]).assert().success().get_output().stdout.clone();
    let results: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert!(results.as_array().unwrap().iter().all(|r| r["benchmark"].as_str().unwrap().contains("SampleBench")));

    t.kargo("app")
        .arg("bench")
        .args(quick)
        .arg("nothing-like-this")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no benchmark matches"));
    t.kargo("app").args(["bench", "--no-run"]).assert().success();

    // Benchmarks are not part of the program or its tests.
    t.kargo("app").arg("package").assert().success();
    let jar = std::fs::read(t.dir.path().join("app/target/release/jvm/app-0.1.0.jar")).unwrap();
    assert!(!String::from_utf8_lossy(&jar).contains("SampleBench"));
    t.kargo("app").arg("test").assert().success().stderr(predicate::str::contains("SampleBench").not());
}

#[test]
fn jvm_test_failures_and_filters() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);

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
}

#[test]
fn jvm_test_limits_exit_codes_tags_and_reruns() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);

    // `test-timeout` fails a test that runs too long, and the rest of the run goes on.
    t.write("app/tests/Slow.kt", "import kotlin.test.*\nclass Slow {\n    @Test fun sleeps() = Thread.sleep(600_000)\n}\n");
    t.write("app/kargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[test]\ntest-timeout = \"2s\"\n");
    t.kargo("app").arg("test").assert().code(101).stdout(predicate::str::contains("sleeps()"));

    // `timeout` stops the whole run, and says so.
    t.write("app/kargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[test]\ntimeout = \"3s\"\n");
    t.kargo("app").arg("test").assert().code(101).stderr(predicate::str::contains("JVM tests timed out after 3s"));
    std::fs::remove_file(t.dir.path().join("app/tests/Slow.kt")).unwrap();
    t.write("app/kargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");

    // A test that exits the JVM is reported as such, in both output modes.
    t.write("app/tests/Exit.kt", "import kotlin.test.*\nclass Exit {\n    @Test fun quits() { System.exit(5) }\n}\n");
    let exited = "test JVM exited with code 5 before finishing";
    t.kargo("app").arg("test").assert().code(101).stderr(predicate::str::contains(exited));
    t.kargo("app").args(["test", "--message-format", "json"]).assert().code(101).stderr(predicate::str::contains(exited));
    std::fs::remove_file(t.dir.path().join("app/tests/Exit.kt")).unwrap();

    // Tags select tests on both sides of `--include-tag` and `--exclude-tag`.
    t.create(&["tags"]);
    t.write(
        "tags/tests/Tagged.kt",
        "import kotlin.test.*\nimport org.junit.jupiter.api.Tag\nclass Tagged {\n    @Test @Tag(\"fast\") fun quick() {}\n    @Test @Tag(\"slow\") fun lengthy() {}\n}\n",
    );
    let only = |flag: &str, tag: &str| t.kargo("tags").args(["test", flag, tag]).assert().success().get_output().stdout.clone();
    let included = String::from_utf8(only("--include-tag", "fast")).unwrap();
    assert!(included.contains("quick()") && !included.contains("lengthy()"), "{included}");
    let excluded = String::from_utf8(only("--exclude-tag", "fast")).unwrap();
    assert!(excluded.contains("lengthy()") && !excluded.contains("quick()"), "{excluded}");
    t.kargo("tags")
        .args(["test", "--include-tag", "nothing"])
        .assert()
        .code(101)
        .stderr(predicate::str::contains("no JVM tests match the --include-tag and --exclude-tag filters"));

    // `--failed` reruns the classes that failed in the last run, and nothing else.
    t.create(&["reruns"]);
    t.write("reruns/tests/Passes.kt", "import kotlin.test.*\nclass Passes { @Test fun ok() {} }\n");
    t.write("reruns/tests/Flaky.kt", "import kotlin.test.*\nclass Flaky { @Test fun bad() = assertEquals(1, 2) }\n");
    t.kargo("reruns").args(["test", "--failed"]).assert().code(101).stderr(predicate::str::contains("no failed JVM tests are recorded"));
    let rerun = String::from_utf8(t.kargo("reruns").args(["test", "--failed"]).assert().code(101).get_output().stdout.clone()).unwrap();
    assert!(rerun.contains("bad()") && !rerun.contains("ok()"), "{rerun}");
    t.write("reruns/tests/Flaky.kt", "import kotlin.test.*\nclass Flaky { @Test fun bad() {} }\n");
    t.kargo("reruns").args(["test", "--failed"]).assert().success().stdout(predicate::str::contains("bad()"));
    t.kargo("reruns").args(["test", "--failed"]).assert().success().stderr(predicate::str::contains("no JVM test failed in the last run"));
}

/// The JSON messages on a command's stdout, which must hold nothing else.
fn json_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    let text = String::from_utf8_lossy(stdout);
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}\n{text}"))).collect()
}

#[test]
fn jvm_tests_are_listed_and_reported_as_json() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    t.write(
        "app/tests/MathTest.kt",
        "package com.example.app\n\nimport kotlin.test.*\n\nclass MathTest {\n    @Test\n    fun `adds numbers`() {\n        \
         println(\"from a test\")\n        assertEquals(4, 2 + 2)\n    }\n\n    @Test fun fails() = assertEquals(5, 2 + 2, \"math\")\n\n    \
         @Ignore @Test fun later() {}\n}\n",
    );

    // `--list` compiles the tests and prints them without running them.
    t.kargo("app")
        .args(["test", "--list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jvm: com.example.app.MathTest.adds numbers\n"))
        .stdout(predicate::str::contains("jvm: com.example.app.GreetingTest.greetsByName\n"))
        .stderr(predicate::str::contains("Listed 4 jvm tests"));
    let out = t.kargo("app").args(["test", "--list", "--format", "json", "MathTest"]).assert().success().get_output().stdout.clone();
    let listed = json_lines(&out);
    assert_eq!(listed.len(), 3, "{listed:?}");
    let adds = listed.iter().find(|m| m["name"] == "adds numbers").unwrap();
    assert_eq!(adds["reason"], "test-discovered");
    assert_eq!((adds["package"].as_str(), adds["target"].as_str()), (Some("app"), Some("jvm")));
    assert_eq!(adds["suite"], "com.example.app.MathTest");
    assert!(adds["id"].as_str().unwrap().contains("[method:adds numbers()]"), "{adds}");
    assert!(adds["file"].as_str().unwrap().ends_with("MathTest.kt"), "{adds}");
    assert_eq!(adds["line"], 7);

    // `--message-format json` reports each test as it runs; the human report goes to stderr.
    let out = t.kargo("app").args(["test", "--message-format", "json"]).assert().code(101).get_output().clone();
    let messages = json_lines(&out.stdout);
    let finished = |name: &str| messages.iter().find(|m| m["reason"] == "test-finished" && m["name"] == name).unwrap().clone();
    assert!(messages.iter().any(|m| m["reason"] == "test-started" && m["name"] == "adds numbers" && m["id"] == adds["id"]));
    assert_eq!(finished("adds numbers")["outcome"], "passed");
    assert_eq!(finished("adds numbers")["stdout"].as_str().map(str::trim_end), Some("from a test"));
    let fails = finished("fails");
    assert_eq!(fails["outcome"], "failed");
    assert!(fails["message"].as_str().unwrap().contains("math"), "{fails}");
    assert!(fails["details"].as_str().unwrap().contains("MathTest.kt"), "{fails}");
    assert_eq!(finished("later")["outcome"], "ignored");
    let summary = messages.last().unwrap();
    assert_eq!(summary["reason"], "test-summary");
    assert_eq!((summary["passed"].as_u64(), summary["failed"].as_u64(), summary["ignored"].as_u64()), (Some(2), Some(1), Some(1)));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fails()"));
}

#[test]
fn thin_jar_follows_main_class_and_runtime_classpath() {
    require_e2e!();
    let t = E2e::new();
    let package = "[package]\nname = \"stale\"\nversion = \"0.1.0\"\n";
    t.write("stale/kargo.toml", &format!("{package}\n[target.jvm]\nmain-class = \"MainKt\"\n"));
    t.write("stale/src/Main.kt", "fun main() = println(\"main\")\n");
    t.write("stale/src/Other.kt", "object Other { @JvmStatic fun main(args: Array<String>) = println(\"other\") }\n");
    t.kargo("stale").arg("build").assert().success();

    // Neither change recompiles anything, but both must reach the jar's manifest.
    let repo = common::LocalRepo::new();
    repo.publish("com.acme:lib", "1.0", &[]);
    t.write(
        "stale/kargo.toml",
        &format!(
            "{package}\n[target.jvm]\nmain-class = \"Other\"\n\n[repositories]\nlocal = \"{}\"\n\n\
             [dependencies]\n\"com.acme:lib\" = {{ version = \"1.0\", scope = \"runtime\" }}\n",
            repo.url()
        ),
    );
    t.kargo("stale").arg("build").assert().success().stderr(predicate::str::contains("Compiling").not());

    let jar = t.dir.path().join("stale/target/debug/jvm/stale-0.1.0.jar");
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("META-INF/MANIFEST.MF").unwrap(), &mut text).unwrap();
    let text = text.replace("\r\n ", "");
    assert!(text.contains("Main-Class: Other\r\n"), "{text}");
    assert!(text.contains("lib/lib-1.0.jar"), "{text}");
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
        .stderr(predicate::str::contains("m.other.OtherKt"))
        .stderr(predicate::str::contains("kargo run --main <class>"));
}

#[test]
fn multiple_entry_points() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["multi"]);
    std::fs::remove_file(t.dir.path().join("multi/tests/GreetingTest.kt")).unwrap();
    t.write("multi/src/Main.kt", "package m\nfun main(args: Array<String>) = println(\"app \" + args.joinToString())\n");
    t.write("multi/src/tools/Migrate.kt", "package m.tools\nfun main(args: Array<String>) = println(\"migrate \" + args.joinToString())\n");
    t.write("multi/src/tools/Seed.kt", "package m.seed\nobject Seed { @JvmStatic fun main(args: Array<String>) = println(\"seed\") }\n");

    // Without a pick, the error lists the mains; `--main` picks one.
    t.kargo("multi").arg("run").assert().code(101).stderr(predicate::str::contains("m.tools.MigrateKt"));
    t.kargo("multi")
        .args(["run", "--main", "m.tools.MigrateKt", "--", "up"])
        .assert()
        .success()
        .stdout(predicate::str::contains("migrate up"));

    let manifest = std::fs::read_to_string(t.dir.path().join("multi/kargo.toml")).unwrap();
    t.write(
        "multi/kargo.toml",
        &format!(
            "{manifest}\n[target.jvm]\nmain-class = \"m.MainKt\"\n\n[[bin]]\nname = \"migrate\"\nmain = \"m.tools.MigrateKt\"\n\n\
             [[bin]]\nname = \"seed\"\npath = \"src/tools/Seed.kt\"\n"
        ),
    );
    t.kargo("multi").arg("run").assert().success().stdout(predicate::str::contains("app"));
    t.kargo("multi").args(["run", "--bin", "migrate", "--", "down"]).assert().success().stdout(predicate::str::contains("migrate down"));
    t.kargo("multi").args(["run", "--bin", "seed"]).assert().success().stdout(predicate::str::contains("seed"));

    // `kargo package --bin` writes a launcher jar for the bin.
    let jar = t.kargo("multi").args(["package", "-q", "--bin", "migrate"]).assert().success().get_output().stdout.clone();
    let jar = String::from_utf8(jar).unwrap();
    assert!(jar.trim().ends_with("migrate-0.1.0-all.jar"), "{jar}");
    let out = StdCommand::new("java").arg("-jar").arg(jar.trim()).arg("x").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("migrate x"));

    // Examples compile on their own against the main classes, each with its own `main`.
    t.write("multi/examples/hello.kt", "fun main() = println(\"example \" + m.tools.Api.NAME)\n");
    t.write("multi/examples/bye.kt", "fun main() = println(\"bye\")\n");
    t.write("multi/src/tools/Api.kt", "package m.tools\nobject Api { const val NAME = \"api\" }\n");
    t.kargo("multi")
        .args(["run", "--example", "hello"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Compiling multi v0.1.0 (jvm, example hello)"))
        .stdout(predicate::str::contains("example api"));
    t.kargo("multi").args(["run", "--example", "bye"]).assert().success().stdout(predicate::str::contains("bye"));
}

#[test]
fn release_package_runs_the_r8_optimized_jar() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["optimized"]);
    t.write("optimized/src/Main.kt", "package sample\nfun main() = println(\"optimized app\")\n");
    t.write("optimized/kargo.toml", "[package]\nname = \"optimized\"\nversion = \"0.1.0\"\n[target.jvm]\nmain-class = \"sample.MainKt\"\n");

    let assert = t.kargo("optimized").args(["package", "--release"]).assert().success();
    let output = assert.get_output().clone();
    let jar = String::from_utf8(output.stdout).unwrap();
    let jar = PathBuf::from(jar.trim());
    assert!(jar.file_name().unwrap().to_string_lossy().ends_with("-min.jar"), "{}", jar.display());
    assert!(jar.with_file_name("optimized-0.1.0-all.jar").is_file());
    assert!(String::from_utf8_lossy(&output.stderr).contains("without R8"), "{}", String::from_utf8_lossy(&output.stderr));
    let out = StdCommand::new("java").arg("-jar").arg(&jar).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("optimized app"));
    let mapping = jar.with_file_name("optimized-0.1.0-mapping.txt");
    assert!(mapping.is_file(), "{}", mapping.display());

    t.create(&["against-min"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("against-min/kargo.toml")).unwrap();
    t.write("against-min/kargo.toml", &format!("{manifest}\n[package.optimize]\nobfuscate = false\n"));
    // Without keep rules R8 inlines `greeting` into `main` and removes it, and shrinks the standard library the tests use.
    t.write("against-min/proguard-rules.pro", "-keep class com.example.** { *; }\n-keep class kotlin.** { *; }\n");
    t.kargo("against-min").args(["test", "-t", "jvm", "--against-min"]).assert().success();
}

#[test]
fn run_a_single_file() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["proj"]);
    t.write("proj/src/Api.kt", "package proj\nobject Api { const val NAME = \"api\" }\n");
    t.write(
        "proj/tools/migrate.kt",
        "fun main(args: Array<String>) = println(\"migrate \" + proj.Api.NAME + \" \" + args.joinToString())\n",
    );
    // Inside a project the file sees the project's code, but is not part of its build.
    t.kargo("proj")
        .args(["run", "tools/migrate.kt", "up", "--", "--fast"])
        .assert()
        .success()
        .stderr(predicate::str::contains("(jvm, tools"))
        .stdout(predicate::str::contains("migrate api up, --fast"));
    // An unchanged file is not compiled again.
    t.kargo("proj")
        .args(["run", "tools/migrate.kt", "down"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Compiling").not())
        .stdout(predicate::str::contains("migrate api down"));
    let jvm = t.dir.path().join("proj/target/debug/jvm");
    assert!(jvm.join("files/migrate/MigrateKt.class").is_file());
    assert!(!jvm.join("classes/MigrateKt.class").exists(), "the file is not part of the project's build");
    // Outside a project, and for scripts, the file runs as `kargo script` runs it.
    t.write("loose/hello.kt", "fun main(args: Array<String>) = println(\"hello \" + args.joinToString())\n");
    t.kargo("loose").args(["run", "hello.kt", "world"]).assert().success().stdout(predicate::str::diff("hello world\n").normalize());
    t.write("loose/twice.main.kts", "println(\"twice ${args.joinToString()}\")\n");
    t.kargo("loose").args(["run", "twice.main.kts", "x"]).assert().success().stdout(predicate::str::diff("twice x\n").normalize());
    t.kargo("loose").args(["run", "-e", "println(1 + 1)"]).assert().success().stdout(predicate::str::diff("2\n").normalize());
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
fn warnings_as_errors_summary_comes_after_the_diagnostics() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["strict"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("strict/kargo.toml")).unwrap();
    t.write("strict/kargo.toml", &format!("{manifest}\n[kotlin]\nwarnings-as-errors = true\n"));
    t.write("strict/src/Main.kt", "@Deprecated(\"use other\")\nfun old(): Int = 1\n\nfun main() = println(old())\n");

    // kotlinc prints "warnings found and -Werror specified" first; Kargo prints it last.
    for daemon in ["false", "true"] {
        let out = t.kargo("strict").arg("build").env("KARGO_DAEMON", daemon).assert().failure().code(101).get_output().stderr.clone();
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text.lines().collect();
        let position = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no `{needle}` in {text}"));
        assert!(position("is deprecated") < position("warnings found and -Werror specified"), "daemon={daemon}: {text}");
        assert!(position("warnings found and -Werror specified") < position("could not compile `strict`"), "daemon={daemon}: {text}");
    }

    // The same order in JSON messages.
    for daemon in ["false", "true"] {
        let out = t
            .kargo("strict")
            .args(["build", "--message-format", "json"])
            .env("KARGO_DAEMON", daemon)
            .assert()
            .failure()
            .code(101)
            .get_output()
            .stdout
            .clone();
        let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&out).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let compiler: Vec<&str> =
            messages.iter().filter(|m| m["reason"] == "compiler-message").map(|m| m["message"].as_str().unwrap()).collect();
        assert!(compiler.len() >= 2, "daemon={daemon}: {messages:?}");
        assert_eq!(compiler.last(), Some(&"warnings found and -Werror specified"), "daemon={daemon}: {compiler:?}");
    }
}

#[test]
fn dependencies_and_hooks() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["deps", "--with", "serialization,coroutines"]);
    t.kargo("deps").args(["add", "org.jetbrains.kotlinx:kotlinx-datetime:0.7.1"]).assert().success();
    let hook = if cfg!(windows) { "echo %KARGO_HOOK% > hook.txt" } else { "echo $KARGO_HOOK > hook.txt" };
    let manifest = std::fs::read_to_string(t.dir.path().join("deps/kargo.toml")).unwrap();
    t.write("deps/kargo.toml", &format!("{manifest}\n[tasks]\nafter = {{ run = '{hook}', hooks = [\"post-build\"] }}\n"));
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

/// A task and a hook plugin written as Kotlin scripts get the task's or hook's environment, the working
/// directory, the arguments after `--` (or the hook name), and the build lock of the build that runs them.
#[test]
fn tasks_and_plugins_run_kotlin_scripts() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["scripted"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("scripted/kargo.toml")).unwrap();
    t.write(
        "scripted/kargo.toml",
        &format!(
            "{manifest}
[tasks]
hello = {{ script = \"scripts/hello.kts\", env = {{ CHANNEL = \"releases\" }} }}
notes = {{ script = \"scripts/notes.kt\", depends-on = [\"hello\"] }}
boom = {{ script = \"scripts/boom.kts\" }}

[plugins.notify]
script = \"scripts/notify.kts\"
args = [\"--quiet\"]
hooks = [\"post-build\"]
env = {{ CHANNEL = \"builds\" }}
"
        ),
    );
    t.write(
        "scripted/scripts/hello.kts",
        "java.io.File(\"hello.txt\").writeText(listOf(System.getenv(\"KARGO_TASK\"), System.getenv(\"KARGO_PKG_NAME\"), System.getenv(\"CHANNEL\"), args.joinToString(\"|\")).joinToString(\" \"))\n",
    );
    t.write(
        "scripted/scripts/notes.kt",
        "/* kargo\n*/\nfun main(args: Array<String>) {\n    println(\"notes \" + args.joinToString(\"|\"))\n}\n",
    );
    t.write("scripted/scripts/boom.kts", "throw IllegalStateException(\"boom\")\n");
    t.write(
        "scripted/scripts/notify.kts",
        "java.io.File(\"notify.txt\").writeText(args.joinToString(\"|\") + \" \" + System.getenv(\"CHANNEL\") + \" \" + (System.getenv(\"KARGO_BUILD_LOCK\") != null))\n",
    );

    t.kargo("scripted").args(["task", "hello", "--", "a", "b"]).assert().success();
    assert_eq!(std::fs::read_to_string(t.dir.path().join("scripted/hello.txt")).unwrap(), "hello scripted releases a|b");
    // Arguments go to the requested task only, and a `.kt` file runs as `kargo script` runs it.
    t.kargo("scripted").args(["notes", "x"]).assert().success().stdout(predicate::str::diff("notes x\n").normalize());
    assert_eq!(std::fs::read_to_string(t.dir.path().join("scripted/hello.txt")).unwrap(), "hello scripted releases ");
    t.kargo("scripted").args(["task", "boom"]).assert().code(101).stderr(predicate::str::contains("task `boom` failed"));

    t.kargo("scripted").arg("build").assert().success();
    assert_eq!(std::fs::read_to_string(t.dir.path().join("scripted/notify.txt")).unwrap(), "--quiet|post-build builds true");
}

/// An `[source-sets.shared]` of the JVM and Wasm: its `actual` matches the `expect` of `src/`, and the
/// set's tests run on both targets.
#[test]
fn source_set_shared_by_jvm_and_wasm() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["sets", "--target", "jvm,wasm"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("sets/kargo.toml")).unwrap();
    t.write(
        "sets/kargo.toml",
        &format!(
            "{manifest}
[source-sets.shared]
targets = [\"jvm\", \"wasm\"]
"
        ),
    );
    t.write(
        "sets/src/Platform.kt",
        "package com.example.sets

expect fun platform(): String
",
    );
    t.write(
        "sets/src-shared/Platform.kt",
        "package com.example.sets

actual fun platform(): String = \"shared+\" + target()

expect fun target(): String
",
    );
    t.write(
        "sets/src-jvm/Target.kt",
        "package com.example.sets

actual fun target(): String = \"jvm\"
",
    );
    t.write(
        "sets/src-wasm/Target.kt",
        "package com.example.sets

actual fun target(): String = \"wasm\"
",
    );
    t.write(
        "sets/src/Main.kt",
        "package com.example.sets

fun greeting(n: String) = \"Hello, $n!\"

fun main() = println(greeting(platform()))
",
    );
    t.write(
        "sets/tests-shared/PlatformTest.kt",
        "package com.example.sets

import kotlin.test.Test
import kotlin.test.assertTrue

class PlatformTest {
    @Test
    fun startsWithTheSet() = assertTrue(platform().startsWith(\"shared+\"))
}
",
    );
    t.kargo("sets").arg("run").assert().success().stdout(predicate::str::contains("Hello, shared+jvm!"));
    t.kargo("sets").args(["run", "--target", "wasm"]).assert().success().stdout(predicate::str::contains("Hello, shared+wasm!"));
    t.kargo("sets")
        .args(["test", "--no-fail-fast"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PlatformTest.startsWithTheSet ... ok"));
    let out = t.kargo("sets").args(["metadata", "--no-deps"]).assert().success().get_output().stdout.clone();
    let metadata: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(metadata["packages"][0]["targets"][0]["source-sets"][0]["name"], "shared");
}

/// A shared module with an Android library and a JVM jar, consumed by an Android app and a JVM app: the
/// code of `[source-sets.jvmAndAndroid]` is in both.
#[test]
fn shared_module_for_an_android_and_a_jvm_app() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.write(
        "kargo.toml",
        "[workspace]
members = [\"shared\", \"androidApp\", \"desktopApp\"]
",
    );
    t.write(
        "shared/kargo.toml",
        "[package]
name = \"shared\"
version = \"0.1.0\"
group = \"com.example\"

[target.jvm]

         [target.android]
kind = \"library\"

[source-sets.jvmAndAndroid]
targets = [\"jvm\", \"android\"]

         [publish]
javadoc = \"empty\"
",
    );
    t.write(
        "shared/src/Platform.kt",
        "package com.example.shared

expect fun platform(): String

fun hello() = \"Hello from \" + platform() + \" via \" + via()

expect fun via(): String
",
    );
    t.write(
        "shared/src-jvmAndAndroid/Via.kt",
        "package com.example.shared

actual fun via(): String = java.io.StringReader(\"java.io\").readText()
",
    );
    t.write(
        "shared/src-jvm/Platform.kt",
        "package com.example.shared

actual fun platform(): String = \"jvm\"
",
    );
    t.write(
        "shared/src-android/Platform.kt",
        "package com.example.shared

actual fun platform(): String = \"android\"
",
    );
    t.create(&["androidApp", "--target", "android"]);
    let app = std::fs::read_to_string(t.dir.path().join("androidApp/kargo.toml")).unwrap();
    t.write(
        "androidApp/kargo.toml",
        &format!(
            "{app}
[dependencies]
shared = {{ path = \"../shared\" }}
"
        ),
    );
    t.write(
        "androidApp/src-android/Welcome.kt",
        "package com.example.androidapp

fun welcome(): String = com.example.shared.hello()
",
    );
    t.create(&["desktopApp"]);
    let desktop = std::fs::read_to_string(t.dir.path().join("desktopApp/kargo.toml")).unwrap();
    t.write(
        "desktopApp/kargo.toml",
        &format!(
            "{desktop}
[dependencies]
shared = {{ path = \"../shared\" }}
"
        ),
    );
    t.write(
        "desktopApp/src/Main.kt",
        "package com.example.desktopapp

fun main() = println(com.example.shared.hello())
",
    );
    t.kargo("").arg("build").assert().success();
    t.kargo("").args(["run", "-p", "desktopApp"]).assert().success().stdout(predicate::str::contains("Hello from jvm via java.io"));
    assert!(t.dir.path().join("target/android/androidApp/build/outputs/apk/debug/androidApp-debug.apk").is_file());
    assert!(t.dir.path().join("target/android/shared/build/outputs/aar/shared-debug.aar").is_file());
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
    // The runner's TeamCity messages become JSON messages too.
    let out = t.kargo("mpp").args(["test", "--target", "wasm", "--message-format", "json"]).assert().success().get_output().stdout.clone();
    let messages = json_lines(&out);
    let finished = messages.iter().find(|m| m["reason"] == "test-finished").expect("a test-finished message");
    assert_eq!(
        (finished["target"].as_str(), finished["name"].as_str(), finished["outcome"].as_str()),
        (Some("wasm"), Some("greetsByName"), Some("passed"))
    );
    assert!(messages.iter().any(|m| m["reason"] == "test-started" && m["id"] == finished["id"]));
}

#[test]
fn wasm_main_receives_program_arguments() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["wargs", "--target", "wasm"]);
    t.write("wargs/src/Main.kt", "package com.example.wargs\n\nfun greeting(name: String) = \"Hello, $name!\"\n\nfun main(args: Array<String>) = println(\"args=\" + args.toList())\n");
    for environment in ["js", "wasi"] {
        t.write(
            "wargs/kargo.toml",
            &format!("[package]\nname = \"wargs\"\nversion = \"0.1.0\"\n\n[target.wasm]\nenvironment = \"{environment}\"\n"),
        );
        t.kargo("wargs").args(["run", "--", "a", "b c"]).assert().success().stdout(predicate::str::contains("args=[a, b c]"));
    }
    t.kargo("wargs").arg("test").assert().success();
}

/// A Wasm project (`web`) with the given `[target.wasm]` body.
fn wasm_project(t: &E2e, name: &str, wasm: &str) {
    t.create(&[name, "--target", "wasm"]);
    t.write(&format!("{name}/kargo.toml"), &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[target.wasm]\n{wasm}"));
}

#[test]
fn wasm_browser_tests_run_in_a_headless_browser() {
    require_e2e!();
    if kargo_toolchain::browser::browser().is_err() {
        eprintln!("skipping: no browser installed");
        return;
    }
    let t = E2e::new();
    wasm_project(&t, "web", "environment = \"browser\"\n");
    t.kargo("web")
        .arg("test")
        .assert()
        .success()
        .stderr(predicate::str::contains("Running Wasm tests (headless browser)"))
        .stdout(predicate::str::contains("test com.example.web.GreetingTest.greetsByName ... ok"));

    // Output, failures and reports come through like they do under Node.js.
    t.write(
        "web/tests/PageTest.kt",
        "package com.example.web\n\nimport kotlin.test.*\n\nclass PageTest {\n    @Test fun talks() { println(\"hello from the page\") }\n    @Test fun fails() = assertEquals(3, 1 + 1, \"math\")\n    @Test @Ignore fun skipped() {}\n}\n",
    );
    t.kargo("web").args(["test", "--report", "junit"]).assert().failure().code(101).stdout(
        predicate::str::contains("hello from the page")
            .and(predicate::str::contains("PageTest.fails ... FAILED"))
            .and(predicate::str::contains("PageTest.skipped ... ignored"))
            .and(predicate::str::contains("test result: FAILED. 2 passed; 1 failed; 1 ignored")),
    );
    let report = std::fs::read_to_string(t.dir.path().join("web/target/debug/wasm/test-reports/TEST-wasm.xml")).unwrap();
    assert!(report.contains("PageTest") && report.contains("<failure"), "{report}");

    // The browser is chosen with KARGO_BROWSER; one that does not exist is reported.
    t.kargo("web")
        .arg("test")
        .env("KARGO_BROWSER", t.dir.path().join("no-such-browser"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("KARGO_BROWSER is"));
}

#[test]
fn wasm_browser_tests_run_in_every_installed_browser() {
    require_e2e!();
    let browsers = kargo_toolchain::browser::installed_browsers();
    if browsers.is_empty() {
        eprintln!("skipping: no browser installed");
        return;
    }
    let t = E2e::new();
    wasm_project(&t, "each", "environment = \"browser\"\n");
    for browser in browsers {
        eprintln!("testing in {}", browser.exe.display());
        t.kargo("each")
            .arg("test")
            .env("KARGO_BROWSER", &browser.exe)
            .assert()
            .success()
            .stdout(predicate::str::contains("test com.example.each.GreetingTest.greetsByName ... ok"));
    }
}

#[test]
fn wasm_browser_app_is_served() {
    require_e2e!();
    let t = E2e::new();
    wasm_project(&t, "site", "environment = \"browser\"\n");
    t.write("site/resources/style.css", "body { margin: 0 }\n");
    let server = Served::start(&t, "site");
    let url = server.host.clone();

    let home = http_get(&url, "/");
    assert!(home.starts_with("HTTP/1.1 200 OK") && home.contains("Content-Type: text/html"), "{home}");
    assert!(home.contains("<script type=\"module\" src=\"./site.mjs\"></script>"), "{home}");
    let module = http_get(&url, "/site.mjs");
    assert!(module.starts_with("HTTP/1.1 200 OK") && module.contains("Content-Type: text/javascript"), "{module}");
    // Browsers only compile a streamed module that is served as WebAssembly.
    assert!(http_get(&url, "/site.wasm").contains("Content-Type: application/wasm"));
    // `resources/` is copied next to the module.
    let css = http_get(&url, "/style.css");
    assert!(css.contains("Content-Type: text/css") && css.contains("margin: 0"), "{css}");
    assert!(http_get(&url, "/missing.js").starts_with("HTTP/1.1 404"));
    assert!(http_get(&url, "/..%2Fkargo.toml").starts_with("HTTP/1.1 404"));
}

#[test]
fn wasm_browser_app_serves_its_own_page() {
    require_e2e!();
    let t = E2e::new();
    wasm_project(&t, "custom", "environment = \"browser\"\n");
    t.write(
        "custom/resources/index.html",
        "<!doctype html><html><head><title>mine</title></head><body><script type=\"module\" src=\"./custom.mjs\"></script></body></html>\n",
    );
    let server = Served::start(&t, "custom");
    let home = http_get(&server.host, "/");
    assert!(home.contains("<title>mine</title>") && !home.contains("is running"), "{home}");
    assert!(!home.contains("importmap"), "a page that needs no npm package gets no import map:\n{home}");
}

#[test]
fn wasm_browser_app_gets_the_web_resources_of_its_libraries() {
    require_e2e!();
    let t = E2e::new();
    let repo = t.dir.path().join("repo");
    // A library that binds to a separately built module, which it ships with an init script.
    t.create(&["physics", "--lib", "--target", "wasm"]);
    t.write(
        "physics/kargo.toml",
        &format!(
            "[package]\nname = \"physics\"\nversion = \"0.4.0\"\ngroup = \"com.example\"\n\n[target.wasm]\nenvironment = \"browser\"\nkind = \"library\"\n\
             web-resources = [\"web/engine.mjs\", \"web/assets\"]\nweb-init = \"web/init.mjs\"\n\n[repositories]\nstaging = \"{}\"\n\n\
             [publish]\nrepository = \"staging\"\n",
            file_url(&repo)
        ),
    );
    t.write("physics/web/engine.mjs", "export default () => 42;\n");
    t.write("physics/web/assets/logo.txt", "logo\n");
    t.write("physics/web/init.mjs", "import engine from './engine.mjs';\nglobalThis.engine = engine();\n");

    wasm_project(&t, "game", "environment = \"browser\"\n\n[dependencies]\nphysics = { path = \"../physics\" }\n");
    t.write("game/resources/style.css", "body { margin: 0 }\n");
    {
        let server = Served::start(&t, "game");
        let home = http_get(&server.host, "/");
        let init = home.find("await import(\"./init.mjs\");").unwrap_or_else(|| panic!("the page imports the init script:\n{home}"));
        assert!(init < home.find("await import(\"./game.mjs\");").unwrap(), "the init script runs first:\n{home}");
        assert!(http_get(&server.host, "/engine.mjs").contains("export default () => 42;"));
        assert!(http_get(&server.host, "/assets/logo.txt").contains("logo"));
        assert!(http_get(&server.host, "/style.css").contains("margin: 0"));
    }
    // The app's own file of the same name wins, and a dropped file does not linger.
    t.write("game/resources/engine.mjs", "export default () => 7;\n");
    t.kargo("game").arg("build").assert().success();
    let out = t.dir.path().join("game/target/debug/wasm");
    assert_eq!(std::fs::read_to_string(out.join("engine.mjs")).unwrap(), "export default () => 7;\n");
    std::fs::remove_file(t.dir.path().join("game/resources/engine.mjs")).unwrap();
    std::fs::remove_file(t.dir.path().join("game/resources/style.css")).unwrap();
    t.kargo("game").arg("build").assert().success();
    assert!(!out.join("style.css").exists());
    assert_eq!(std::fs::read_to_string(out.join("engine.mjs")).unwrap(), "export default () => 42;\n");
    // The tests' page gets them too.
    t.kargo("game").args(["test", "--no-run"]).assert().success();
    let tests = std::fs::read_to_string(out.join("test/index.html")).unwrap();
    assert!(tests.contains("for (const init of [\"./init.mjs\"]) await import(init);") && out.join("test/engine.mjs").is_file(), "{tests}");

    // A declared file that is missing is named.
    std::fs::rename(t.dir.path().join("physics/web/init.mjs"), t.dir.path().join("physics/web/init.bak")).unwrap();
    t.kargo("game")
        .arg("build")
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("`physics` ships the web resource `web/init.mjs`"));
    std::fs::rename(t.dir.path().join("physics/web/init.bak"), t.dir.path().join("physics/web/init.mjs")).unwrap();

    // Published, the files travel as a classifier zip that a Maven dependency brings along.
    t.kargo("physics").arg("publish").assert().success();
    let zip = repo.join("com/example/physics-wasm-js/0.4.0/physics-wasm-js-0.4.0-web-resources.zip");
    let mut names = zip_names(&zip);
    names.sort();
    assert_eq!(names, ["assets/logo.txt", "engine.mjs", "init.mjs"]);
    wasm_project(
        &t,
        "remote",
        &format!(
            "environment = \"browser\"\n\n[dependencies]\n\"com.example:physics\" = \"0.4.0\"\n\n[repositories]\nstaging = \"{}\"\n",
            file_url(&repo)
        ),
    );
    t.kargo("remote").arg("build").assert().success();
    let remote = t.dir.path().join("remote/target/debug/wasm");
    assert_eq!(std::fs::read_to_string(remote.join("engine.mjs")).unwrap(), "export default () => 42;\n");
    assert!(std::fs::read_to_string(remote.join("index.html")).unwrap().contains("await import(\"./init.mjs\");"));
    assert!(
        std::fs::read_to_string(t.dir.path().join("remote/kargo.lock")).unwrap().contains("physics-wasm-js-0.4.0-web-resources.zip"),
        "the zip is locked"
    );
}

/// A Kotlin/JS project (`name`) with the given `[target.js]` body.
fn js_project(t: &E2e, name: &str, js: &str) {
    t.create(&[name, "--target", "js"]);
    t.write(&format!("{name}/kargo.toml"), &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[target.js]\n{js}"));
}

#[test]
fn js_node_program_runs_and_tests() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    js_project(&t, "jsapp", "");
    t.write(
        "jsapp/src/Main.kt",
        "package com.example.jsapp\n\nfun greeting(name: String) = \"Hello, $name!\"\n\nfun main(args: Array<String>) = println(greeting(\"JS\") + \" args=\" + args.toList())\n",
    );
    t.kargo("jsapp")
        .args(["run", "--", "a", "b c"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Compiling jsapp v0.1.0 (js)"))
        .stdout(predicate::str::contains("Hello, JS! args=[a, b c]"));
    assert!(t.dir.path().join("jsapp/target/debug/js/jsapp.mjs").is_file());
    t.kargo("jsapp")
        .arg("test")
        .assert()
        .success()
        .stderr(predicate::str::contains("Running Kotlin/JS tests (Node.js)"))
        .stdout(predicate::str::contains("test com.example.jsapp.GreetingTest.greetsByName ... ok"));

    // Failures, ignored tests, filters and reports come through as on the other targets.
    t.write(
        "jsapp/tests/MoreTest.kt",
        "package com.example.jsapp\n\nimport kotlin.test.*\n\nclass MoreTest {\n    @Test fun talks() { println(\"hello from a test\") }\n    @Test fun fails() = assertEquals(3, 1 + 1, \"math\")\n    @Test @Ignore fun skipped() {}\n}\n",
    );
    t.kargo("jsapp").args(["test", "--report", "junit"]).assert().failure().code(101).stdout(
        predicate::str::contains("hello from a test")
            .and(predicate::str::contains("MoreTest.fails ... FAILED"))
            .and(predicate::str::contains("MoreTest.skipped ... ignored"))
            .and(predicate::str::contains("test result: FAILED. 2 passed; 1 failed; 1 ignored")),
    );
    let report = std::fs::read_to_string(t.dir.path().join("jsapp/target/debug/js/test-reports/TEST-js.xml")).unwrap();
    assert!(report.contains("MoreTest") && report.contains("<failure"), "{report}");
    t.kargo("jsapp")
        .args(["test", "GreetingTest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GreetingTest.greetsByName ... ok").and(predicate::str::contains("MoreTest").not()));
    t.kargo("jsapp").args(["check", "--tests"]).assert().success();
}

#[test]
fn js_and_jvm_share_common_code() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["jsmpp", "--target", "jvm,js"]);
    t.write("jsmpp/src/Platform.kt", "package com.example.jsmpp\n\nexpect fun platform(): String\n");
    t.write("jsmpp/src-jvm/Platform.kt", "package com.example.jsmpp\n\nactual fun platform() = \"jvm\"\n");
    t.write("jsmpp/src-js/Platform.kt", "package com.example.jsmpp\n\nactual fun platform() = \"js\"\n");
    t.write(
        "jsmpp/src/Main.kt",
        "package com.example.jsmpp\n\nfun greeting(n: String) = \"Hello, $n!\"\n\nfun main() = println(greeting(platform()))\n",
    );
    t.kargo("jsmpp").arg("run").assert().success().stdout(predicate::str::contains("Hello, jvm!"));
    t.kargo("jsmpp").args(["run", "-t", "js"]).assert().success().stdout(predicate::str::contains("Hello, js!"));
    t.kargo("jsmpp").args(["test", "-t", "js"]).assert().success().stdout(predicate::str::contains("GreetingTest.greetsByName ... ok"));
}

#[test]
fn js_browser_tests_run_in_a_headless_browser() {
    require_e2e!();
    if kargo_toolchain::browser::browser().is_err() {
        eprintln!("skipping: no browser installed");
        return;
    }
    let t = E2e::new();
    js_project(&t, "jsweb", "environment = \"browser\"\n");
    t.write(
        "jsweb/src/Main.kt",
        "package com.example.jsweb\n\nimport kotlinx.browser.document\n\nfun greeting(name: String) = \"Hello, $name!\"\n\nfun main() {\n    document.body?.append(greeting(\"page\"))\n}\n",
    );
    t.write(
        "jsweb/tests/PageTest.kt",
        "package com.example.jsweb\n\nimport kotlinx.browser.document\nimport kotlin.test.*\n\nclass PageTest {\n    @Test fun hasADocument() = assertNotNull(document.body)\n}\n",
    );
    t.kargo("jsweb")
        .arg("test")
        .assert()
        .success()
        .stderr(predicate::str::contains("Running Kotlin/JS tests (headless browser)"))
        .stdout(predicate::str::contains("test com.example.jsweb.PageTest.hasADocument ... ok"))
        .stdout(predicate::str::contains("test com.example.jsweb.GreetingTest.greetsByName ... ok"));
    t.kargo("jsweb").arg("build").assert().success();
    let page = std::fs::read_to_string(t.dir.path().join("jsweb/target/debug/js/index.html")).unwrap();
    assert!(page.contains("<script type=\"module\" src=\"./jsweb.mjs\"></script>"), "{page}");
}

#[test]
fn compose_html_builds_and_tests_in_a_browser() {
    require_e2e!();
    if kargo_toolchain::browser::browser().is_err() {
        eprintln!("skipping: no browser installed");
        return;
    }
    let t = E2e::new();
    t.create(&["htmlapp", "--with", "compose-html"]);
    t.write(
        "htmlapp/tests/DomTest.kt",
        "package com.example.htmlapp\n\nimport androidx.compose.runtime.*\nimport kotlinx.browser.document\nimport kotlin.test.*\nimport org.jetbrains.compose.web.dom.Div\nimport org.jetbrains.compose.web.dom.Text\nimport org.jetbrains.compose.web.renderComposable\n\nclass DomTest {\n    @Test\n    fun rendersRealElements() {\n        val root = document.createElement(\"div\").also { document.body!!.appendChild(it) }\n        renderComposable(root) { Div(attrs = { id(\"hello\") }) { Text(greeting(\"DOM\")) } }\n        assertEquals(\"Hello, DOM!\", document.getElementById(\"hello\")?.textContent)\n    }\n}\n",
    );
    t.kargo("htmlapp")
        .arg("test")
        .assert()
        .success()
        .stderr(predicate::str::contains("Running Kotlin/JS tests (headless browser)"))
        .stdout(predicate::str::contains("DomTest.rendersRealElements ... ok"))
        .stdout(predicate::str::contains("GreetingTest.greetsByName ... ok"));

    t.kargo("htmlapp").arg("build").assert().success();
    let page = std::fs::read_to_string(t.dir.path().join("htmlapp/target/debug/js/index.html")).unwrap();
    assert!(page.contains("<div id=\"root\"></div>") && page.contains("src=\"./htmlapp.mjs\""), "{page}");

    // The release bundle drops dead code and reports its size.
    t.kargo("htmlapp")
        .args(["build", "--release"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Size").and(predicate::str::contains("htmlapp.mjs")));
    assert!(t.dir.path().join("htmlapp/target/release/js/htmlapp.mjs").is_file());

    // Compose HTML is Kotlin/JS only.
    t.write(
        "htmlapp/kargo.toml",
        "[package]\nname = \"htmlapp\"\nversion = \"0.1.0\"\n\n[target.wasm]\nenvironment = \"browser\"\n\n[frameworks]\ncompose = { html = true }\n",
    );
    t.kargo("htmlapp").arg("build").assert().failure().stderr(predicate::str::contains("Kotlin/JS only"));
}

#[test]
fn js_library_builds_an_npm_package() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["jsgreeter", "--lib", "--target", "js"]);
    t.write(
        "jsgreeter/kargo.toml",
        "[package]\nname = \"jsgreeter\"\nversion = \"0.3.0\"\n\n[target.js]\nkind = \"library\"\nmodule-kind = \"commonjs\"\nnpm-name = \"@acme/greeter\"\n",
    );
    t.write(
        "jsgreeter/src/Greeter.kt",
        "package com.example.greeter\n\n@OptIn(ExperimentalJsExport::class)\n@JsExport\nfun greet(name: String): String = \"Hello, $name!\"\n",
    );
    t.kargo("jsgreeter").args(["build", "--release"]).assert().success();
    let out = t.dir.path().join("jsgreeter/target/release/js");
    let declarations = std::fs::read_to_string(out.join("jsgreeter.d.ts")).unwrap();
    assert!(declarations.contains("function greet(name: string): string;"), "{declarations}");
    let package: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("package.json")).unwrap()).unwrap();
    assert_eq!((package["name"].as_str(), package["type"].as_str()), (Some("@acme/greeter"), Some("commonjs")));
    assert_eq!(package["main"], "./jsgreeter.js");
    t.kargo("jsgreeter").args(["run", "-t", "js"]).assert().failure().stderr(predicate::str::contains("has no program to run"));
    if which("node").is_some() {
        let script = "console.log(require('./target/release/js/jsgreeter.js').com.example.greeter.greet('npm'))";
        let output = StdCommand::new("node").args(["-e", script]).current_dir(t.dir.path().join("jsgreeter")).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Hello, npm!", "{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn wasm_library_builds_an_npm_package() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["greeter", "--lib", "--target", "wasm"]);
    t.write(
        "greeter/kargo.toml",
        "[package]\nname = \"greeter\"\nversion = \"0.3.0\"\nlicense = \"MIT\"\n\n[target.wasm]\nkind = \"library\"\nmodule-name = \"greeter-core\"\nnpm-name = \"@acme/greeter\"\nwasm-opt = false\n",
    );
    t.write(
        "greeter/src/Greeter.kt",
        "package com.example.greeter\n\n@OptIn(ExperimentalJsExport::class)\n@JsExport\nfun greet(name: String): String = \"Hello, $name!\"\n\nfun unused(): String = \"not exported\"\n",
    );
    t.kargo("greeter").arg("build").assert().success().stderr(predicate::str::contains("Compiling greeter v0.3.0 (wasm)"));

    let out = t.dir.path().join("greeter/target/debug/wasm");
    for file in ["greeter-core.mjs", "greeter-core.wasm", "greeter-core.d.mts", "package.json"] {
        assert!(out.join(file).is_file(), "{file} is missing");
    }
    let declarations = std::fs::read_to_string(out.join("greeter-core.d.mts")).unwrap();
    assert!(declarations.contains("export declare function greet(name: string): string;"), "{declarations}");
    let package: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["name"], "@acme/greeter");
    assert_eq!(package["version"], "0.3.0");
    assert_eq!(package["license"], "MIT");
    assert_eq!(package["type"], "module");
    assert_eq!(package["main"], "./greeter-core.mjs");
    assert_eq!(package["types"], "./greeter-core.d.mts");

    // A library has no program, and the module exports what is marked `@JsExport`.
    t.kargo("greeter").args(["run", "-t", "wasm"]).assert().failure().stderr(predicate::str::contains("has no program to run"));
    if which("node").is_some() {
        let script =
            "import('./target/debug/wasm/greeter-core.mjs').then(m => console.log(m.greet('npm'), Object.keys(m).includes('unused')))";
        let output = StdCommand::new("node").args(["-e", script]).current_dir(t.dir.path().join("greeter")).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Hello, npm! false", "{}", String::from_utf8_lossy(&output.stderr));
    }
    t.kargo("greeter").arg("test").assert().success();
}

#[test]
fn publish_a_wasm_library_to_npm_dry_run() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: npm is not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["greeter", "--lib", "--target", "wasm"]);
    t.write(
        "greeter/kargo.toml",
        "[package]\nname = \"greeter\"\nversion = \"0.3.0\"\ndescription = \"Greets\"\nlicense = \"MIT\"\n\
         repository = \"https://github.com/acme/greeter\"\n\n[target.wasm]\nkind = \"library\"\nmodule-name = \"greeter-core\"\n\
         wasm-opt = false\npackage-manager = \"npm\"\n\n[publish.npm]\nscope = \"acme\"\naccess = \"public\"\nkeywords = [\"kotlin\"]\n",
    );
    t.write(
        "greeter/src/Greeter.kt",
        "package com.example.greeter\n\n@OptIn(ExperimentalJsExport::class)\n@JsExport\nfun greet(name: String): String = \"Hello, $name!\"\n",
    );
    // No registry is reachable from a test, so the check for the version on the registry only warns in a dry run.
    t.kargo("greeter").args(["publish", "--npm", "--dry-run"]).env("NPM_CONFIG_REGISTRY", "http://127.0.0.1:9").assert().success().stderr(
        predicate::str::contains("Packaging @acme/greeter v0.3.0 (npm)")
            .and(predicate::str::contains("greeter-core.wasm"))
            .and(predicate::str::contains("greeter-core.d.mts"))
            .and(predicate::str::contains("package.json"))
            .and(predicate::str::contains("aborting upload of @acme/greeter")),
    );
    let package: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(t.dir.path().join("greeter/target/release/wasm/package.json")).unwrap()).unwrap();
    assert_eq!(package["name"], "@acme/greeter");
    assert_eq!(package["keywords"], serde_json::json!(["kotlin"]));
    assert_eq!(package["bugs"]["url"], "https://github.com/acme/greeter/issues");
    assert!(
        t.dir
            .path()
            .join("greeter/target/release/npm/acme-greeter")
            .read_dir()
            .unwrap()
            .any(|e| { e.unwrap().file_name().to_string_lossy().ends_with(".tgz") })
    );
}

#[test]
fn wasm_release_removes_dead_code_and_reports_the_size() {
    require_e2e!();
    let t = E2e::new();
    wasm_project(&t, "slim", "wasm-opt = false\n");
    t.kargo("slim").arg("build").assert().success().stderr(predicate::str::contains("Size").not());
    let debug = std::fs::metadata(t.dir.path().join("slim/target/debug/wasm/slim.wasm")).unwrap().len();

    t.kargo("slim").args(["build", "--release"]).assert().success().stderr(
        predicate::str::contains("Size slim.wasm").and(predicate::str::contains("gzip,")).and(predicate::str::contains("Binaryen").not()),
    );
    let release = std::fs::metadata(t.dir.path().join("slim/target/release/wasm/slim.wasm")).unwrap().len();
    assert!(release * 4 < debug, "dead code elimination should shrink the module a lot: {release} bytes in release, {debug} in debug");
    if which("node").is_some() {
        t.kargo("slim").args(["run", "--release", "-t", "wasm"]).assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
    }
}

#[test]
fn wasm_release_runs_wasm_opt_when_it_is_installed() {
    require_e2e!();
    if which("wasm-opt").is_none() {
        eprintln!("skipping: Binaryen (wasm-opt) not installed");
        return;
    }
    let t = E2e::new();
    wasm_project(&t, "tiny", "");
    t.kargo("tiny")
        .args(["build", "--release"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Optimizing tiny.wasm with wasm-opt").and(predicate::str::contains("Optimized tiny.wasm:")));
    // Unchanged output is not optimized again.
    t.kargo("tiny").args(["build", "--release"]).assert().success().stderr(predicate::str::contains("Optimizing").not());
    if which("node").is_some() {
        t.kargo("tiny").args(["run", "--release", "-t", "wasm"]).assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
    }
}

#[test]
fn wasm_release_uses_pinned_binaryen_and_supports_budgets_compression_and_hashes() {
    require_e2e!();
    let t = E2e::new();
    let home = tempfile::tempdir().unwrap();
    let empty_path = tempfile::tempdir().unwrap();
    let java_home =
        std::env::var_os("JAVA_HOME").map(PathBuf::from).or_else(|| which("java").and_then(|p| p.parent()?.parent().map(PathBuf::from)));
    wasm_project(
        &t,
        "optimized",
        "environment = \"browser\"\nopt-level = \"size\"\ncompress = [\"gzip\", \"brotli\"]\ncontent-hash = true\nmax-size = \"1 B\"\n",
    );
    let mut command = t.kargo("optimized");
    command
        .args(["build", "--release", "--size-report", "--message-format", "json"])
        .env("KARGO_HOME", home.path())
        .env("KARGO_AUTO_INSTALL", "1")
        .env("PATH", empty_path.path());
    if let Some(java_home) = java_home {
        command.env("JAVA_HOME", java_home);
    } else {
        command.env_remove("JAVA_HOME");
    }
    command
        .assert()
        .failure()
        .stderr(predicate::str::contains("Downloading binaryen-version_133-"))
        .stderr(predicate::str::contains("Optimizing optimized.wasm with wasm-opt"))
        .stderr(predicate::str::contains("Wasm size budget exceeded"))
        .stdout(predicate::str::contains("\"reason\":\"wasm-size\"").and(predicate::str::contains("\"gzip_bytes\"")));
    let out = t.dir.path().join("optimized/target/release/wasm");
    let wasm = std::fs::read_dir(&out).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "wasm")).unwrap();
    let stem = wasm.file_stem().unwrap().to_string_lossy();
    assert!(stem.starts_with("optimized."), "{stem}");
    for suffix in ["wasm.gz", "wasm.br", "mjs.gz", "mjs.br"] {
        assert!(out.join(format!("{stem}.{suffix}")).is_file(), "missing {stem}.{suffix}");
    }
    let page = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(page.contains(&format!("{stem}.mjs")), "the page loads the content-addressed module: {page}");
}

/// `kargo run --port 0` on a project, stopped when dropped.
struct Served {
    child: std::process::Child,
    /// `127.0.0.1:<port>`
    host: String,
}

impl Served {
    fn start(t: &E2e, project: &str) -> Self {
        use std::io::{BufRead, BufReader};
        let program = Command::cargo_bin("kargo").unwrap().get_program().to_owned();
        let mut cmd = StdCommand::new(program);
        cmd.current_dir(t.dir.path().join(project))
            .env_remove("KARGO_TARGET_DIR")
            .env("NO_COLOR", "1")
            .args(["run", "--port", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let mut log = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let host = loop {
            let wait = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(wait) {
                Ok(line) => {
                    if let Some(at) = line.find("http://") {
                        break line[at + "http://".len()..].split('/').next().unwrap().to_owned();
                    }
                    log.push(line);
                }
                Err(_) => {
                    let _ = child.kill();
                    panic!("kargo run never started serving:\n{}", log.join("\n"));
                }
            }
        };
        Served { child, host }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A bare HTTP/1.1 GET that returns the whole response.
fn http_get(host: &str, path: &str) -> String {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(host).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    String::from_utf8_lossy(&response).into_owned()
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
}

#[test]
fn doc_multiplatform() {
    require_e2e!();
    let t = E2e::new();
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
    // Documenting some targets leaves the documentation of the others alone.
    let combined = index;
    t.kargo("mpp-doc").args(["doc", "-t", "wasm"]).assert().success().stderr(predicate::str::contains("doc-wasm"));
    assert!(t.dir.path().join("mpp-doc/target/debug/doc-wasm/index.html").is_file());
    assert_eq!(std::fs::read_to_string(t.dir.path().join("mpp-doc/target/debug/doc/index.html")).unwrap(), combined);
}

/// Applies the `TextEdit`s of an LSP reply to ASCII `text`.
fn apply_edits(text: &str, edits: &[serde_json::Value]) -> String {
    let offset = |position: &serde_json::Value| {
        let line = position["line"].as_u64().unwrap() as usize;
        let character = position["character"].as_u64().unwrap() as usize;
        text.split_inclusive('\n').take(line).map(str::len).sum::<usize>() + character
    };
    let mut out = text.to_owned();
    let mut edits: Vec<_> =
        edits.iter().map(|e| (offset(&e["range"]["start"]), offset(&e["range"]["end"]), e["newText"].as_str().unwrap())).collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (start, end, new) in edits {
        out.replace_range(start..end, new);
    }
    out
}

#[test]
fn lsp_formats_kotlin_with_ktlint() {
    require_e2e!();
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;

    let t = E2e::new();
    t.create(&["style"]);
    let root = t.dir.path().join("style");
    let manifest = root.join("kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[fmt]\nexclude = [\"src/gen/**\"]\n")).unwrap();
    // The project's own style: ktlint reads it from here, as `kargo fmt` does.
    t.write("style/.editorconfig", "root = true\n\n[*.{kt,kts}]\nindent_size = 2\n");
    let messy = "package com.example.style\n\nfun   messy( ): Int {\n        val n =  listOf(1,2).size\n        return n\n}\n";
    t.write("style/src/Messy.kt", messy);
    t.write("style/src/gen/Generated.kt", messy);

    let uri = |rel: &str| {
        let text = root.join(rel).display().to_string().replace('\\', "/");
        if text.starts_with('/') { format!("file://{text}") } else { format!("file:///{text}") }
    };
    let mut child = StdCommand::new(Command::cargo_bin("kargo").unwrap().get_program())
        .arg("lsp")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut send = |message: serde_json::Value| {
        let body = serde_json::to_vec(&message).unwrap();
        write!(input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        input.write_all(&body).unwrap();
        input.flush().unwrap();
    };
    let mut reply = |id: i64| -> serde_json::Value {
        loop {
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(output.read_line(&mut line).unwrap() > 0, "kargo lsp ended before answering {id}");
                match line.trim().strip_prefix("Content-Length: ") {
                    Some(n) => length = n.parse().unwrap(),
                    None if line.trim().is_empty() => break,
                    None => {}
                }
            }
            let mut body = vec![0; length];
            output.read_exact(&mut body).unwrap();
            let message: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
        }
    };
    send(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": uri(""), "capabilities": {} } }));
    let init = reply(1);
    assert_eq!(init["result"]["capabilities"]["documentFormattingProvider"], true, "{init}");
    send(serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    for rel in ["src/Messy.kt", "src/gen/Generated.kt"] {
        let document = serde_json::json!({ "uri": uri(rel), "languageId": "kotlin", "version": 1, "text": messy });
        send(serde_json::json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": document } }));
    }

    let params = |rel: &str| serde_json::json!({ "textDocument": { "uri": uri(rel) }, "options": { "tabSize": 4, "insertSpaces": true } });
    send(serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/formatting", "params": params("src/Messy.kt") }));
    let formatted = reply(2);
    let edits = formatted["result"].as_array().unwrap_or_else(|| panic!("{formatted}"));
    assert_eq!(edits.len(), 1, "one minimal edit: {formatted}");
    assert_eq!(
        apply_edits(messy, edits),
        "package com.example.style\n\nfun messy(): Int {\n  val n = listOf(1, 2).size\n  return n\n}\n",
        "ktlint with the project's .editorconfig"
    );
    // `[fmt] exclude` applies as it does to `kargo fmt`.
    send(serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "textDocument/formatting", "params": params("src/gen/Generated.kt") }));
    assert_eq!(reply(3)["result"], serde_json::json!([]));
    // A syntax error is an error, not an empty file.
    send(serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": { "textDocument": { "uri": uri("src/Messy.kt"), "version": 2 }, "contentChanges": [{ "text": "fun broken( {\n" }] }
    }));
    send(serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "textDocument/formatting", "params": params("src/Messy.kt") }));
    let failed = reply(4);
    assert!(failed["error"]["message"].as_str().is_some_and(|m| m.contains("Not a valid Kotlin file")), "{failed}");

    send(serde_json::json!({ "jsonrpc": "2.0", "id": 5, "method": "shutdown" }));
    reply(5);
    send(serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
    drop(send);
    child.wait().unwrap();
}

#[test]
fn fmt_with_ktlint() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["style"]);
    // A new project is already formatted.
    t.kargo("style").args(["fmt", "--check"]).assert().success().stderr(predicate::str::contains("Checking formatting of style v0.1.0"));

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
}

#[test]
fn lint_with_detekt() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["style"]);
    // A new project is lint-free.
    t.kargo("style").arg("lint").assert().success().stderr(predicate::str::contains("Linting style v0.1.0"));

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
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
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
fn wasm_calls_npm_packages_through_generated_declarations() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["web", "--target", "wasm"]);
    let manifest = t.dir.path().join("web/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    // express ships no TypeScript declarations, so Kargo installs `@types/express` for them.
    std::fs::write(&manifest, format!("{text}\n[target.wasm.npm-dependencies]\nexpress = \"5.1.0\"\n")).unwrap();
    t.write(
        "web/src/Main.kt",
        "import express.Router\nimport express.express\n\nfun main() {\n    val app = express()\n    app.get(\"/\") { _, res, _ -> res.status(200).send(\"hi\") }\n    \
         app.use(\"/api\", Router())\n    println(\"env: \" + app.get(\"env\"))\n}\n",
    );
    t.kargo("web")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Installing npm packages @types/express@^5"))
        .stderr(predicate::str::contains("Generating Kotlin declarations for npm packages express"))
        .stderr(predicate::str::contains("warning").not())
        .stdout(predicate::str::contains("env: development"));
    let generated = t.dir.path().join("web/target/debug/generated/npm");
    assert!(generated.join("express/Default.kt").is_file());
    assert!(generated.join("express_serve_static_core/Types.kt").is_file());
    // Nothing changed: no second install or generation.
    t.kargo("web").arg("build").assert().success().stderr(predicate::str::contains("Generating").not());
}

#[test]
fn kotlin_js_calls_npm_packages_through_generated_declarations() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    t.create(&["web", "--target", "js"]);
    let manifest = t.dir.path().join("web/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[target.js.npm-dependencies]\nexpress = \"5.1.0\"\n")).unwrap();
    t.write(
        "web/src/Main.kt",
        "import express.express\n\nfun main() {\n    val app = express()\n    println(\"env: \" + app.get(\"env\"))\n}\n",
    );
    t.kargo("web")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating Kotlin declarations for npm packages express"))
        .stderr(predicate::str::contains("warning").not())
        .stdout(predicate::str::contains("env: development"));
    assert!(t.dir.path().join("web/target/debug/generated/npm-js/express/Default.kt").is_file());
}

/// A Kotlin module that calls a function of its own TypeScript module, which calls back into an
/// `@JsExport` Kotlin function through the module's `.d.mts`.
fn typescript_interop(t: &E2e, name: &str, target: &str) {
    t.create(&[name, "--target", target]);
    // The template's test calls the `greeting` that `Main.kt` below replaces.
    let _ = std::fs::remove_file(t.dir.path().join(format!("{name}/tests/GreetingTest.kt")));
    t.write(
        &format!("{name}/js/greeter.ts"),
        &format!(
            "import {{ exclaim }} from \"../{name}.mjs\";\n\n\
             /** Greets `name`. */\nexport function greet(name: string, times: number): string {{\n    \
             return exclaim(`Hello, ${{name}}`) + \" x\" + times;\n}}\n"
        ),
    );
    t.write(
        &format!("{name}/src/Main.kt"),
        "import js.greeter.greet\n\n@OptIn(kotlin.js.ExperimentalJsExport::class)\n@JsExport\nfun exclaim(text: String): String = \"$text!\"\n\n\
         fun main() = println(greet(\"Kargo\", 2))\n",
    );
}

#[test]
fn wasm_and_typescript_call_each_other() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    typescript_interop(&t, "mixed", "wasm");
    t.write("mixed/tests/GreeterTest.kt", "import js.greeter.greet\nimport kotlin.test.*\n\nclass GreeterTest {\n    @Test\n    fun greets() = assertEquals(\"Hello, test! x1\", greet(\"test\", 1))\n}\n");
    t.kargo("mixed")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("TypeScript module (tsc"))
        .stdout(predicate::str::contains("Hello, Kargo! x2"));
    let out = t.dir.path().join("mixed/target/debug/wasm");
    assert!(out.join("js/greeter.js").is_file() && out.join("mixed.d.mts").is_file());
    assert!(t.dir.path().join("mixed/target/debug/generated/js-sources/js/greeter/Exports.kt").is_file());
    t.kargo("mixed").arg("test").assert().success();
    // Nothing changed: tsc does not run again.
    t.kargo("mixed").arg("build").assert().success().stderr(predicate::str::contains("TypeScript").not());

    // A type error is reported at its line, and fails the build.
    t.write("mixed/js/greeter.ts", "export function greet(name: string, times: number): string {\n    return times;\n}\n");
    t.kargo("mixed")
        .args(["build", "--message-format", "json"])
        .assert()
        .code(101)
        .stdout(predicate::str::contains("\"reason\":\"compiler-message\"").and(predicate::str::contains("greeter.ts")))
        .stdout(predicate::str::contains("\"line\":2"));
}

/// The default compiler is the newest TypeScript, which `kargo.lock` pins; `[toolchain] typescript` names another
/// one, and both compile the same modules with the same `tsc` command line and `tsconfig.json`.
#[test]
fn typescript_compiler_is_the_latest_unless_pinned() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    typescript_interop(&t, "tsver", "js");
    let lock = t.dir.path().join("tsver/kargo.lock");
    let compiler = |out: &std::process::Output| {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let (_, rest) = stderr.split_once("(tsc ").expect("tsc ran");
        rest.split_once(')').expect("a version").0.to_owned()
    };

    let out = t.kargo("tsver").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kargo! x2")).get_output().clone();
    let latest = compiler(&out);
    let locked = std::fs::read_to_string(&lock).unwrap();
    assert!(locked.contains("request = \"latest\"") && locked.contains(&format!("version = \"{latest}\"")), "{locked}");
    // Nothing changed: it is neither looked up nor run again, and `--locked` accepts the lock.
    t.kargo("tsver").args(["build", "--locked"]).assert().success().stderr(predicate::str::contains("TypeScript").not());

    let manifest = t.dir.path().join("tsver/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        format!(
            "{text}
[toolchain]
typescript = \"5.9.3\"
"
        ),
    )
    .unwrap();
    let out = t.kargo("tsver").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kargo! x2")).get_output().clone();
    assert_eq!(compiler(&out), "5.9.3");

    std::fs::write(
        &manifest,
        format!(
            "{text}
[toolchain]
typescript = \"^6\"
"
        ),
    )
    .unwrap();
    let out = t.kargo("tsver").arg("run").assert().success().get_output().clone();
    assert!(compiler(&out).starts_with("6."), "{}", String::from_utf8_lossy(&out.stderr));
    let locked = std::fs::read_to_string(&lock).unwrap();
    assert!(locked.contains("request = \"^6\""), "{locked}");
}

#[test]
fn kotlin_js_and_typescript_call_each_other() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    typescript_interop(&t, "mixedjs", "js");
    t.kargo("mixedjs").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kargo! x2"));
    assert!(t.dir.path().join("mixedjs/target/debug/generated/js-sources-js/js/greeter/Exports.kt").is_file());
}

/// Object parameters built with the generated factories, on Wasm and Kotlin/JS: a named interface
/// and an inline object type, with optional properties left out of the object when not given.
#[test]
fn object_parameters_are_built_with_generated_factories() {
    require_e2e!();
    if which("node").is_none() {
        eprintln!("skipping: Node.js not installed");
        return;
    }
    let t = E2e::new();
    for (name, target, generated) in [("objwasm", "wasm", "js-sources"), ("objjs", "js", "js-sources-js")] {
        t.create(&[name, "--target", target]);
        t.write(
            &format!("{name}/js/greet.js"),
            "export function greet(g, style) {\n    \
             const s = `Hello, ${g.name}` + (\"times\" in g ? ` x${g.times}` : \"\") + (style.suffix ?? \"\");\n    \
             return style.loud ? s.toUpperCase() : s;\n}\n",
        );
        t.write(
            &format!("{name}/js/greet.d.ts"),
            "export interface Greeting { name: string; times?: number }\n\
             export declare function greet(greeting: Greeting, style: { loud: boolean; suffix?: string }): string;\n",
        );
        t.write(
            &format!("{name}/src/Main.kt"),
            "import js.greet.*\n\nfun main() {\n    println(greet(Greeting(name = \"Kargo\"), GreetStyle(loud = false)))\n    \
             println(greet(Greeting(name = \"Kargo\", times = 2), GreetStyle(loud = true, suffix = \"!\")))\n}\n",
        );
        t.kargo(name)
            .arg("run")
            .assert()
            .success()
            .stderr(predicate::str::contains("warning").not())
            .stdout(predicate::str::contains("Hello, Kargo\n"))
            .stdout(predicate::str::contains("HELLO, KARGO X2!\n"));
        assert!(t.dir.path().join(format!("{name}/target/debug/generated/{generated}/js/greet/Factories.kt")).is_file());
        assert!(t.dir.path().join(format!("{name}/target/debug/{target}/js/kargo-factories/js.greet.mjs")).is_file());
    }
}

#[test]
fn wasm_browser_app_serves_its_typescript_modules() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    typescript_interop(&t, "page", "wasm");
    let manifest = t.dir.path().join("page/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap().replace("environment = \"js\"", "environment = \"browser\"");
    std::fs::write(&manifest, text).unwrap();
    t.write("page/tests/GreeterTest.kt", "import js.greeter.greet\nimport kotlin.test.*\n\nclass GreeterTest {\n    @Test\n    fun greets() = assertEquals(\"Hello, page! x3\", greet(\"page\", 3))\n}\n");
    t.kargo("page").arg("test").assert().success();
    assert!(t.dir.path().join("page/target/debug/wasm/test/js/greeter.js").is_file());
}

#[test]
fn fetch_prepares_an_offline_jvm_build() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    // A cache of its own, so the commands below only see what `kargo fetch` downloaded.
    let home = tempfile::tempdir().unwrap();
    let manifest = t.dir.path().join("app/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[toolchain]\njdk = \"21\"\n")).unwrap();

    t.kargo("app").env("KARGO_HOME", home.path()).arg("fetch").assert().success();
    t.kargo("app").env("KARGO_HOME", home.path()).args(["fetch", "--offline"]).assert().success();
    // The pinned JDK was downloaded by `fetch`, and the test run needs no network: the JUnit launcher is cached too.
    t.kargo("app")
        .env("KARGO_HOME", home.path())
        .args(["toolchain", "which", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("JDK 21"));
    t.kargo("app").env("KARGO_HOME", home.path()).args(["test", "--offline"]).assert().success();
}

#[test]
fn fetch_installs_wasm_npm_packages_for_offline_builds() {
    require_e2e!();
    if which("npm").is_none() {
        eprintln!("skipping: Node.js/npm not installed");
        return;
    }
    let t = E2e::new();
    let home = tempfile::tempdir().unwrap();
    t.create(&["dt", "--target", "wasm"]);
    t.kargo("dt").env("KARGO_HOME", home.path()).args(["add", "org.jetbrains.kotlinx:kotlinx-datetime:0.7.1"]).assert().success();
    t.kargo("dt").env("KARGO_HOME", home.path()).args(["add", "org.jetbrains.kotlinx:kotlinx-serialization-core:1.9.0"]).assert().success();
    t.write(
        "dt/src/Main.kt",
        "package com.example.dt\n\nimport kotlinx.datetime.*\n\nfun greeting(n: String) = \"Hello, $n!\"\n\n\
         fun main() = println(LocalDate(2024, 1, 31).plus(1, DateTimeUnit.MONTH))\n",
    );
    t.kargo("dt")
        .env("KARGO_HOME", home.path())
        .arg("fetch")
        .assert()
        .success()
        .stderr(predicate::str::contains("Installing npm packages @js-joda/core@"));
    assert!(t.dir.path().join("dt/target/debug/node_modules/@js-joda/core").is_dir());
    // The build finds the installed packages and runs without the network.
    t.kargo("dt")
        .env("KARGO_HOME", home.path())
        .args(["run", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2024-02-29"));
}

#[test]
fn fetch_tools_serves_fmt_lint_and_doc_offline() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["style"]);
    let home = tempfile::tempdir().unwrap();
    t.kargo("style").env("KARGO_HOME", home.path()).args(["fetch", "--tools"]).assert().success();
    t.kargo("style").env("KARGO_HOME", home.path()).args(["fmt", "--check", "--offline"]).assert().success();
    t.kargo("style").env("KARGO_HOME", home.path()).args(["lint", "--offline"]).assert().success();
    t.kargo("style").env("KARGO_HOME", home.path()).args(["doc", "--offline"]).assert().success();
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
    // The build prints the framework versions it resolved, with the BOM's Spring Framework and Jackson versions.
    let spring = format!("spring-boot {} (Spring Framework ", kargo_ops::builtins::SPRING_BOOT_VERSION);
    t.kargo("sb").arg("build").assert().success().stderr(predicate::str::contains(spring).and(predicate::str::contains("Jackson ")));
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
fn ktor_compiles() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["kt", "--with", "ktor,serialization"]);
    t.kargo("kt").arg("test").assert().success();
}

#[test]
fn compose_compiles() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["ui", "--with", "compose"]);
    t.kargo("ui").arg("build").assert().success();
}

#[test]
fn compose_compiler_options() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["ui", "--with", "compose"]);
    let path = t.dir.path().join("ui/kargo.toml");
    let manifest = std::fs::read_to_string(&path).unwrap();
    assert!(manifest.contains("compose = true"), "{manifest}");
    let options = r#"compose = { stability-config = "stability.conf", feature-flags = ["OptimizeNonSkippingGroups"], metrics = true, reports = true }"#;
    std::fs::write(&path, manifest.replace("compose = true", options)).unwrap();
    t.write("ui/stability.conf", "java.time.*\n");
    t.kargo("ui").arg("build").assert().success();
    let files = |dir: &str| -> Vec<String> {
        std::fs::read_dir(t.dir.path().join("ui/target/compose/jvm").join(dir))
            .unwrap_or_else(|e| panic!("target/compose/jvm/{dir}: {e}"))
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let metrics = files("metrics");
    assert!(metrics.iter().any(|f| f.ends_with("-module.json")), "{metrics:?}");
    let reports = files("reports");
    assert!(reports.iter().any(|f| f.ends_with("-composables.txt")), "{reports:?}");
}

/// Adds a string resource to a `compose-multiplatform` project's shared `App()`.
fn use_a_compose_string(t: &E2e, project: &str) {
    t.write(&format!("{project}/composeResources/values/strings.xml"), "<resources><string name=\"title\">From Res</string></resources>\n");
    let app = t.dir.path().join(project).join("src/App.kt");
    let source = std::fs::read_to_string(&app).unwrap();
    let source = source
        .replace(
            "import androidx.compose.ui.Modifier\n",
            &format!("import androidx.compose.ui.Modifier\nimport com.example.{project}.generated.resources.*\nimport org.jetbrains.compose.resources.stringResource\n"),
        )
        .replace("            Text(greeting(\"Compose\"))\n", "            Text(greeting(\"Compose\"))\n            Text(stringResource(Res.string.title))\n");
    std::fs::write(app, source).unwrap();
}

#[test]
fn compose_for_web() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["web", "--with", "compose-multiplatform"]);
    use_a_compose_string(&t, "web");
    t.kargo("web").args(["build", "-t", "wasm"]).assert().success();
    let out = t.dir.path().join("web/target/debug/wasm");
    for file in
        ["web.mjs", "web.wasm", "skiko.mjs", "skiko.wasm", "composeResources/com.example.web.generated.resources/values/strings.cvr"]
    {
        assert!(out.join(file).is_file(), "{file} is missing from {}", out.display());
    }
    let page = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(page.contains("overflow: hidden") && !page.contains("is running"), "{page}");
}

#[test]
fn compose_preview_renders_png() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["ui", "--with", "compose-multiplatform"]);
    use_a_compose_string(&t, "ui");
    t.kargo("ui").args(["preview", "--width", "320", "--height", "200", "--density", "2"]).assert().success();
    let png = std::fs::read(t.dir.path().join("ui/target/preview/com.example.ui.AppPreview.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // The IHDR chunk: the size in pixels is the size in dp times the density.
    let size = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    assert_eq!((size(16), size(20)), (640, 400));
}

/// Needs a display, since the app opens a window: set KARGO_E2E_GUI=1 where there is one.
#[test]
fn compose_hot_reload() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_GUI").is_none() {
        eprintln!("skipping: set KARGO_E2E_GUI=1 (opens a window)");
        return;
    }
    use std::io::BufRead;
    let t = E2e::new();
    t.create(&["hot", "--with", "compose-multiplatform"]);
    let app = t.dir.path().join("hot/src/App.kt");
    let source = std::fs::read_to_string(&app).unwrap().replace(
        "        var clicks by remember { mutableStateOf(0) }\n",
        // The app exits once `stop` exists: on Windows, killing `kargo` would leave the app running.
        "        var clicks by remember { mutableStateOf(0) }\n        androidx.compose.runtime.SideEffect { println(\"ui: \" + greeting(\"Compose\")) }\n        \
         androidx.compose.runtime.LaunchedEffect(Unit) {\n            while (!java.io.File(\"stop\").exists()) kotlinx.coroutines.delay(100)\n            \
         kotlin.system.exitProcess(0)\n        }\n",
    );
    std::fs::write(&app, &source).unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("kargo"))
        .current_dir(t.dir.path().join("hot"))
        .args(["run", "--hot-reload"])
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    for stream in [Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, Box::new(child.stderr.take().unwrap())] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    let wait_for = |needle: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
                if line.contains(needle) {
                    return seen;
                }
                seen.push(line);
            }
        }
        panic!("`{needle}` never appeared:\n{}", seen.join("\n"));
    };
    wait_for("ui: Hello, Compose!");
    std::fs::write(&app, source.replace("\"Hello, $name!\"", "shout(\"Hot, $name!\")\n\nfun shout(s: String) = s.uppercase()")).unwrap();
    let before = wait_for("ui: HOT, COMPOSE!");
    let after = wait_for("Reloaded");
    std::fs::write(t.dir.path().join("hot/stop"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();
    assert!(!before.iter().chain(&after).any(|l| l.contains("Running")), "the app reloaded without a restart:\n{before:?}\n{after:?}");
}

/// Needs macOS with Xcode for the iOS frameworks.
#[test]
fn compose_for_ios() {
    require_e2e!();
    require_native!();
    if !cfg!(target_os = "macos") {
        eprintln!("skipping: Compose for iOS builds on macOS");
        return;
    }
    let t = E2e::new();
    t.create(&["ios", "--with", "compose-multiplatform", "-t", "native"]);
    use_a_compose_string(&t, "ios");
    t.kargo("ios").args(["build", "-t", "native"]).assert().success();
    let out = t.dir.path().join("ios/target/debug/native");
    assert!(out.join("ios_arm64/ios.framework").is_dir(), "no framework in {}", out.display());
    assert!(out.join("compose-resources/composeResources/com.example.ios.generated.resources/values/strings.cvr").is_file());
}

// Kotlin/Native links slowly, so each link gets its own test and they run in parallel.

#[test]
fn native_check() {
    require_e2e!();
    require_native!();
    let t = E2e::new();
    t.create(&["nat", "--target", "native", "--with", "coroutines"]);
    t.kargo("nat").args(["check", "--tests"]).assert().success().stderr(predicate::str::contains("Checking nat v0.1.0 (native, "));
    assert!(!t.dir.path().join("nat/target/debug/native").exists(), "check does not link an executable");
}

#[test]
fn native_executable() {
    require_e2e!();
    require_native!();
    let t = E2e::new();
    t.create(&["nat", "--target", "native", "--with", "coroutines"]);
    t.kargo("nat").arg("run").assert().success().stdout(predicate::str::contains("Hello, Kotlin!"));
}

/// A source set of one family of Native platforms (the host's) below `src-native/`: it implements what `src-native/`
/// declares, and a set of another family is not compiled.
#[test]
fn source_sets_of_some_native_platforms() {
    require_e2e!();
    require_native!();
    let family = if cfg!(target_os = "macos") {
        "apple"
    } else if cfg!(windows) {
        "mingw"
    } else {
        "linux"
    };
    let other = if family == "linux" { "mingw" } else { "linux" };
    let t = E2e::new();
    t.create(&["plat", "--target", "native"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("plat/kargo.toml")).unwrap();
    let sets = format!("\n[source-sets.hostFamily]\nplatforms = [\"{family}\"]\n\n[source-sets.otherFamily]\nplatforms = [\"{other}\"]\n");
    t.write("plat/kargo.toml", &format!("{manifest}{sets}"));
    t.write("plat/src/Os.kt", "package com.example.plat\n\nexpect fun os(): String\n");
    // `src-native/` holds the code of every platform and declares what the families implement below it.
    t.write(
        "plat/src-native/Os.kt",
        "package com.example.plat\n\nactual fun os(): String = \"native \" + detail()\n\nexpect fun detail(): String\n",
    );
    t.write("plat/src-hostFamily/Detail.kt", "package com.example.plat\n\nactual fun detail(): String = \"family\"\n");
    // Code for a platform this build does not target would not compile here.
    t.write("plat/src-otherFamily/Other.kt", "package com.example.plat\n\nval broken: Int = \"not an int\"\n");
    t.write("plat/src/Main.kt", "package com.example.plat\n\nfun main() = println(os())\n");
    t.kargo("plat").arg("run").assert().success().stdout(predicate::str::contains("native family"));
    t.kargo("plat").args(["test", "--no-run"]).assert().success();
}

#[test]
fn native_tests() {
    require_e2e!();
    require_native!();
    let t = E2e::new();
    t.create(&["nat", "--target", "native", "--with", "coroutines"]);
    t.kargo("nat").args(["test", "--report", "junit"]).assert().success().stdout(predicate::str::contains("1 passed"));
    let report = std::fs::read_to_string(t.dir.path().join("nat/target/debug/native/test-reports/TEST-native.xml")).unwrap();
    assert!(report.contains("<testcase") && report.contains(r#"failures="0""#), "{report}");

    let out = t.kargo("nat").args(["test", "--list", "--format", "json"]).assert().success().get_output().stdout.clone();
    let listed = json_lines(&out);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!((listed[0]["target"].as_str(), listed[0]["reason"].as_str()), (Some("native"), Some("test-discovered")));
    assert!(listed[0]["file"].as_str().is_some_and(|f| f.ends_with(".kt")), "{listed:?}");
    let out = t.kargo("nat").args(["test", "--message-format", "json"]).assert().success().get_output().stdout.clone();
    let messages = json_lines(&out);
    assert!(
        messages.iter().any(|m| m["reason"] == "test-finished" && m["id"] == listed[0]["id"] && m["outcome"] == "passed"),
        "{messages:?}"
    );
}

#[test]
fn native_multiple_entry_points() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_NATIVE").is_none() {
        eprintln!("skipping: set KARGO_E2E_NATIVE=1 to run Kotlin/Native tests");
        return;
    }
    let t = E2e::new();
    t.create(&["nm", "--target", "native"]);
    std::fs::remove_file(t.dir.path().join("nm/tests/GreetingTest.kt")).unwrap();
    t.write("nm/src/Main.kt", "package m\nfun main() = println(\"app\")\n");
    t.write("nm/src/tools/Tool.kt", "package m.tools\nfun main(args: Array<String>) = println(\"tool \" + args.joinToString())\n");
    t.kargo("nm").arg("build").assert().code(101).stderr(predicate::str::contains("kargo run --entry-point <function>"));
    t.kargo("nm").args(["run", "--entry-point", "m.tools.main", "--", "x"]).assert().success().stdout(predicate::str::contains("tool x"));

    // With a bin per `main`, `kargo build` links each one.
    let manifest = std::fs::read_to_string(t.dir.path().join("nm/kargo.toml")).unwrap();
    t.write(
        "nm/kargo.toml",
        &format!(
            "{manifest}\n[[bin]]\nname = \"app\"\nentry-point = \"m.main\"\n\n[[bin]]\nname = \"tool\"\npath = \"src/tools/Tool.kt\"\n"
        ),
    );
    t.kargo("nm").arg("build").assert().success().stderr(predicate::str::contains("bin tool)"));
    t.kargo("nm").args(["run", "--bin", "tool", "--", "y"]).assert().success().stdout(predicate::str::contains("tool y"));
    t.kargo("nm").args(["run", "--bin", "app"]).assert().success().stdout(predicate::str::contains("app"));
    t.kargo("nm").arg("run").assert().code(101).stderr(predicate::str::contains("available bins: app, tool"));

    // An example links against the main sources as a library.
    t.write("nm/src/Api.kt", "package m\nfun api() = \"api\"\n");
    t.write("nm/examples/demo.kt", "fun main() = println(\"demo \" + m.api())\n");
    t.kargo("nm").args(["run", "--example", "demo"]).assert().success().stdout(predicate::str::contains("demo api"));
}

/// A native project that binds a C header and a function defined in its `.def` file.
fn cinterop_project() -> E2e {
    let t = E2e::new();
    t.create(&["cin", "--target", "native"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("cin/kargo.toml")).unwrap();
    t.write(
        "cin/kargo.toml",
        &format!(
            "{manifest}\n[target.native.cinterop.mathx]\npackage = \"mathx\"\ncompiler-options = [\"-Iinclude\"]\nlibrary-paths = [\"lib\"]\n"
        ),
    );
    t.write("cin/lib/.keep", "");
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
    t
}

#[test]
fn native_cinterop() {
    require_e2e!();
    require_native!();
    let t = cinterop_project();
    t.kargo("cin")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating mathx C bindings"))
        .stdout(predicate::str::contains("answer 42"));
    // Up to date: the bindings are not regenerated.
    t.kargo("cin").arg("build").assert().success().stderr(predicate::str::contains("Generating").not());
    // The klib records the linker options, so programs that depend on it link them too.
    let klib = t.dir.path().join(format!("cin/target/debug/native/{}/cinterop/mathx.klib", host_konan_target()));
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&klib).unwrap()).unwrap();
    let mut manifest = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("default/manifest").unwrap(), &mut manifest).unwrap();
    assert!(manifest.lines().any(|l| l.starts_with("linkerOpts=-L") && l.contains("/cin/lib")), "no -L option in:\n{manifest}");
}

#[test]
fn native_cinterop_tests() {
    require_e2e!();
    require_native!();
    let t = cinterop_project();
    t.kargo("cin").arg("test").assert().success().stdout(predicate::str::contains("1 passed"));
}

#[test]
fn native_cinterop_missing_header_is_explained_without_a_stack_trace() {
    require_e2e!();
    require_native!();
    let t = cinterop_project();
    t.write("cin/interop/mathx.def", "headers = missing.h\n");
    t.kargo("cin")
        .arg("build")
        .assert()
        .failure()
        .stderr(predicate::str::contains("note: the C header `missing.h` was not found"))
        .stderr(predicate::str::contains("org.jetbrains.kotlin.native.interop.gen").not());
}

/// The Kotlin/Native target of this machine.
fn host_konan_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => "mingw_x64",
        ("macos", "aarch64") => "macos_arm64",
        ("macos", _) => "macos_x64",
        _ => "linux_x64",
    }
}

#[test]
fn native_c_sources_and_platform_options() {
    require_e2e!();
    require_native!();
    let t = E2e::new();
    t.create(&["csrc", "--target", "native"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("csrc/kargo.toml")).unwrap();
    let host = host_konan_target();
    // C sources under `c/`, compiled with the toolchain's clang; their directory is also the bindings' include directory.
    t.write(
        "csrc/kargo.toml",
        &format!(
            "{manifest}\n[target.native.c-sources]\ndefines = [\"FACTOR=2\"]\n\n[target.native.c-sources.platform.{host}]\n\
             defines = [\"BONUS=100\"]\n\n[target.native.cinterop.mathx]\nheaders = [\"mathx.h\"]\npackage = \"mathx\"\n\
             excluded-functions = [\"mathx_hidden\"]\n"
        ),
    );
    t.write("csrc/c/mathx.h", "int mathx_scale(int x);\nint mathx_hidden(void);\n");
    t.write(
        "csrc/c/mathx.c",
        "#include \"mathx.h\"\n#ifndef BONUS\n#define BONUS 0\n#endif\nint mathx_scale(int x) { return x * FACTOR + BONUS; }\nint mathx_hidden(void) { return 0; }\n",
    );
    t.write(
        "csrc/src/Main.kt",
        "import kotlinx.cinterop.ExperimentalForeignApi\n\n@OptIn(ExperimentalForeignApi::class)\nfun main() = println(\"scaled \" + mathx.mathx_scale(21))\n",
    );
    t.write(
        "csrc/tests/GreetingTest.kt",
        "import kotlinx.cinterop.ExperimentalForeignApi\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\n\
         @OptIn(ExperimentalForeignApi::class)\nclass ScaleTest {\n    @Test\n    fun scales() = assertEquals(104, mathx.mathx_scale(2))\n}\n",
    );
    t.kargo("csrc")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("C sources"))
        .stdout(predicate::str::contains("scaled 142"));
    t.kargo("csrc").arg("build").assert().success().stderr(predicate::str::contains("Compiling").not());
    t.kargo("csrc").arg("test").assert().success().stdout(predicate::str::contains("1 passed"));
    // An excluded function gets no binding.
    t.write(
        "csrc/src/Main.kt",
        "import kotlinx.cinterop.ExperimentalForeignApi\n\n@OptIn(ExperimentalForeignApi::class)\nfun main() = println(mathx.mathx_hidden())\n",
    );
    t.kargo("csrc").arg("build").assert().code(101).stderr(predicate::str::contains("mathx_hidden"));

    // A shared library comes with its C header in `include/`.
    t.write("csrc/src/Main.kt", "fun twice(x: Int): Int = x * 2\n");
    let shared =
        std::fs::read_to_string(t.dir.path().join("csrc/kargo.toml")).unwrap().replace("kind = \"executable\"", "kind = \"shared\"");
    t.write("csrc/kargo.toml", &shared);
    t.kargo("csrc").arg("build").assert().success();
    let include = t.dir.path().join(format!("csrc/target/debug/native/{host}/include"));
    let header = if host == "mingw_x64" { "csrc_api.h" } else { "libcsrc_api.h" };
    assert!(include.join(header).is_file(), "no {header} in {}", include.display());
}

#[test]
fn android_compose_resources() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid", "--target", "android", "--with", "compose"]);
    t.write("droid/composeResources/values/strings.xml", "<resources><string name=\"app_name\">Droid</string></resources>\n");
    t.write(
        "droid/src/Strings.kt",
        r#"package com.example.droid

import androidx.compose.runtime.Composable
import com.example.droid.generated.resources.*
import org.jetbrains.compose.resources.stringResource

@Composable
fun appName(): String = stringResource(Res.string.app_name)
"#,
    );
    t.kargo("droid").arg("build").assert().success();
    let apk = t.dir.path().join("droid/target/android/build/outputs/apk/debug/droid-debug.apk");
    let zip = zip::ZipArchive::new(std::fs::File::open(&apk).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    let cvr = "assets/composeResources/com.example.droid.generated.resources/values/strings.cvr";
    assert!(names.iter().any(|n| n == cvr), "{cvr} is missing from {}", apk.display());
}

#[test]
fn android_expect_actual() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid", "--target", "android"]);
    t.write("droid/src/Platform.kt", "package com.example.droid\n\nexpect fun platformName(): String\n");
    t.write("droid/src-android/Platform.kt", "package com.example.droid\n\nactual fun platformName(): String = \"android\"\n");
    t.kargo("droid").arg("build").assert().success();
}

#[test]
fn android_lint_placeholders_and_locales() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid", "--target", "android"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("droid/kargo.toml")).unwrap();
    assert!(manifest.contains("min-sdk = 24\n"), "{manifest}");
    let manifest =
        manifest.replace("min-sdk = 24\n", "min-sdk = 24\nmanifest-placeholders = { MAPS_KEY = \"abc123\" }\nlocales = [\"en\"]\n");
    let manifest = format!("{manifest}\n[target.android.lint]\nabort-on-error = false\nreports = [\"xml\", \"sarif\"]\n");
    t.write("droid/kargo.toml", &manifest);
    let android_manifest = t.dir.path().join("droid/android/AndroidManifest.xml");
    let xml = std::fs::read_to_string(&android_manifest).unwrap();
    let xml = xml.replace("</application>", "<meta-data android:name=\"maps.key\" android:value=\"${MAPS_KEY}\" /></application>");
    t.write("droid/android/AndroidManifest.xml", &xml);

    t.kargo("droid").arg("build").assert().success();
    // Gradle substituted the placeholder in the merged manifest.
    fn merged_manifests(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                merged_manifests(&path, out);
            } else if path.file_name().is_some_and(|n| n == "AndroidManifest.xml") {
                out.extend(std::fs::read_to_string(path));
            }
        }
    }
    let mut merged = Vec::new();
    merged_manifests(&t.dir.path().join("droid/target/android/build/intermediates/merged_manifests"), &mut merged);
    assert!(merged.iter().any(|m| m.contains("abc123") && !m.contains("${MAPS_KEY}")), "{merged:?}");

    t.kargo("droid")
        .args(["lint", "--android"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Linting droid v0.1.0 (android, gradle, lint)"));
    let reports = t.dir.path().join("droid/target/debug/android/lint");
    assert!(reports.join("lint-results-debug.sarif").is_file(), "no SARIF report in {}", reports.display());
    assert!(reports.join("lint-results-debug.xml").is_file());

    t.kargo("droid").args(["lint", "--android", "--create-baseline"]).assert().success();
    assert!(t.dir.path().join("droid/lint-baseline.xml").is_file());
    t.kargo("droid").args(["lint", "--android", "--message-format", "json"]).assert().success();
}

#[test]
fn android_app_with_a_library_module() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
        return;
    }
    let t = E2e::new();
    t.create(&["app", "--target", "android"]);
    t.create(&["ui", "--lib", "--target", "android"]);
    t.create(&["model", "--lib"]);
    t.write("kargo.toml", "[workspace]\nmembers = [\"app\", \"ui\", \"model\"]\n");
    // The library's namespace comes from its group and name.
    t.write(
        "ui/kargo.toml",
        "[package]\nname = \"ui\"\nversion = \"0.1.0\"\ngroup = \"com.example\"\n\n[target.android]\nkind = \"library\"\n\n\
         [dependencies]\nmodel = { path = \"../model\", exported = true }\n\n[publish]\njavadoc = \"empty\"\n",
    );
    t.write("ui/android/res/values/strings.xml", "<resources><string name=\"greeting\">Hello from ui</string></resources>\n");
    t.write(
        "ui/src-android/Greeter.kt",
        "package com.example.ui\n\nimport android.content.Context\n\nfun greeting(context: Context): String = context.getString(R.string.greeting)\n",
    );
    t.write("model/src/User.kt", "package com.example.model\n\ndata class User(val name: String)\n");
    let app = std::fs::read_to_string(t.dir.path().join("app/kargo.toml")).unwrap();
    t.write("app/kargo.toml", &format!("{app}\n[dependencies]\nui = {{ path = \"../ui\" }}\n"));
    // The app reaches the library's code, its `R` class (by its namespace) and what it exports.
    t.write(
        "app/src-android/Welcome.kt",
        "package com.example.app\n\nimport android.content.Context\nimport com.example.model.User\nimport com.example.ui.greeting\n\n\
         fun welcome(context: Context, user: User): String =\n    greeting(context) + \", \" + user.name + context.getString(com.example.ui.R.string.greeting)\n",
    );
    t.kargo("").arg("build").assert().success().stderr(predicate::str::contains("Compiling ui v0.1.0 (android, gradle, with app)"));
    let apk = t.dir.path().join("target/android/app/build/outputs/apk/debug/app-debug.apk");
    assert!(apk.is_file(), "no {}", apk.display());
    // The library's AAR is where building it alone puts it.
    let aar = t.dir.path().join("target/android/ui/build/outputs/aar/ui-debug.aar");
    assert!(aar.is_file(), "no {}", aar.display());
    let zip = zip::ZipArchive::new(std::fs::File::open(&aar).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
    for entry in ["AndroidManifest.xml", "classes.jar", "R.txt", "res/values/values.xml"] {
        assert!(names.iter().any(|n| n == entry), "{entry} is missing from {}: {names:?}", aar.display());
    }

    // `kargo publish` stages the release AAR with its POM and `.module`.
    t.kargo("ui").args(["publish", "--local", "--dry-run"]).env("KARGO_MAVEN_LOCAL", t.dir.path().join("m2")).assert().success();
    let staged = t.dir.path().join("target/release/ui/publish/repository/com/example/ui/0.1.0");
    for file in ["ui-0.1.0.aar", "ui-0.1.0.pom", "ui-0.1.0.module"] {
        assert!(staged.join(file).is_file(), "no {file} in {}", staged.display());
    }
    let pom = std::fs::read_to_string(staged.join("ui-0.1.0.pom")).unwrap();
    assert!(pom.contains("<packaging>aar</packaging>") && pom.contains("<artifactId>model</artifactId>"), "{pom}");
}

#[test]
fn android_build_with_a_kargo_managed_sdk() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (downloads the Android SDK from Google)");
        return;
    }
    let t = E2e::new();
    t.create(&["droid", "--target", "android"]);
    let manifest = t.dir.path().join("droid/kargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    t.write("droid/kargo.toml", &format!("{text}\n[toolchain]\nandroid-sdk = \"36\"\n"));
    t.kargo("droid")
        .args(["toolchain", "install", "android-sdk", "--accept-licenses"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed Android SDK platform 36, build-tools"));
    // The system SDK is not needed, nor looked at.
    t.kargo("droid").env_remove("ANDROID_HOME").env_remove("ANDROID_SDK_ROOT").arg("build").assert().success();
    assert!(t.dir.path().join("droid/target/android/build/outputs/apk/debug/droid-debug.apk").is_file());
    let local = std::fs::read_to_string(t.dir.path().join("droid/target/android/local.properties")).unwrap();
    assert!(local.contains("android-sdk"), "{local}");
}

#[test]
fn android_app_with_compose() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_ANDROID").is_none() {
        eprintln!("skipping: set KARGO_E2E_ANDROID=1 (needs the Android SDK)");
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
    // Kargo-managed Gradle comes with a wrapper, so the generated project builds on its own.
    let props = t.dir.path().join("droid/target/android/gradle/wrapper/gradle-wrapper.properties");
    assert!(std::fs::read_to_string(&props).unwrap().contains("distributionSha256Sum="), "{}", props.display());
    assert!(t.dir.path().join("droid/target/android/gradlew").is_file());
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
fn a_machine_without_java_gets_a_managed_jdk() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    // A home of its own, so no managed JDK is installed yet, and nothing on PATH, so no `java`.
    let home = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let kargo = |args: &[&str]| {
        let mut cmd = t.kargo("app");
        cmd.env("KARGO_HOME", home.path()).env_remove("JAVA_HOME").env("PATH", empty.path()).env_remove("KARGO_AUTO_INSTALL").args(args);
        cmd
    };
    kargo(&["run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello, Kotlin!"))
        .stderr(predicate::str::contains("installing Temurin JDK 21"));
    // The next command uses it as it is, offline too.
    kargo(&["--offline", "run"]).assert().success().stderr(predicate::str::contains("installing").not());
    let which = kargo(&["toolchain", "which"]).assert().success().get_output().stdout.clone();
    let which = String::from_utf8(which).unwrap();
    assert!(
        which.contains("system (JDK 21 at") && which.contains(&home.path().join("toolchains").join("jdk").display().to_string()),
        "{which}"
    );
    kargo(&["daemon", "stop"]).assert().success();
}

#[test]
fn path_dependencies() {
    require_e2e!();
    let t = calc_with_mathlib();

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

    // Changing a body of the dependency rebuilds it, but not its consumer, whose classes don't change.
    t.write("mathlib/src/Lib.kt", &std::fs::read_to_string(t.dir.path().join("mathlib/src/Lib.kt")).unwrap().replace("a + b", "b + a"));
    t.kargo("calc").args(["build", "-t", "jvm"]).assert().success().stderr(
        predicate::str::contains("Compiling mathlib v0.1.0 (jvm)").and(predicate::str::contains("Compiling calc v0.1.0 (jvm)").not()),
    );
    // A new declaration changes its ABI, which the consumer compiles against.
    t.write("mathlib/src/More.kt", "package mathlib\n\nfun sub(a: Int, b: Int): Int = a - b\n");
    t.kargo("calc").args(["build", "-t", "jvm"]).assert().success().stderr(predicate::str::contains("Compiling calc v0.1.0 (jvm)"));

    // Tests and the fat jar see it too.
    t.kargo("calc").args(["test", "-t", "jvm"]).assert().success();
    let fat = t.kargo("calc").args(["package", "-q"]).assert().success().get_output().stdout.clone();
    let out = StdCommand::new("java").arg("-jar").arg(String::from_utf8(fat).unwrap().trim()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 + 3 = 5"));
}

#[test]
fn path_dependencies_on_wasm() {
    require_e2e!();
    let t = calc_with_mathlib();
    t.kargo("calc").args(["add", "--path", "../mathlib"]).assert().success();
    // Wasm links the dependency's klib.
    t.kargo("calc").args(["run", "-t", "wasm"]).assert().success().stdout(predicate::str::contains("2 + 3 = 5"));
    t.kargo("calc").args(["test", "-t", "wasm"]).assert().success();
}

#[test]
fn workspaces() {
    require_e2e!();
    let t = calc_with_mathlib();
    t.kargo("calc").args(["add", "--path", "../mathlib"]).assert().success();

    // The packages as a workspace: one kargo.lock and one target/ at the root.
    t.write("kargo.toml", "[workspace]\nmembers = [\"mathlib\", \"calc\"]\n");
    t.kargo("").args(["build", "-t", "jvm"]).assert().success();
    assert!(t.dir.path().join("target/debug/mathlib/jvm/mathlib-0.1.0.jar").is_file());
    assert!(t.dir.path().join("target/debug/calc/jvm/calc-0.1.0.jar").is_file());
    assert!(std::fs::read_to_string(t.dir.path().join("kargo.lock")).unwrap().contains("[resolution.\"calc/jvm/runtime\"]"));
    t.kargo("").args(["run", "-p", "calc", "-t", "jvm"]).assert().success().stdout(predicate::str::contains("2 + 3 = 5"));
    t.kargo("").args(["test", "--workspace", "-t", "jvm"]).assert().success();
}

/// The layout of issue #45, without Android and Native: a multiplatform `core` (JVM and Wasm, with
/// `expect`/`actual`) used by a JVM `server` and a Wasm `web` app, built from the workspace root.
#[test]
fn multiplatform_workspace_builds_in_dependency_order_and_recompiles_on_abi_changes() {
    require_e2e!();
    let t = E2e::new();
    t.write(
        "kargo.toml",
        "[workspace]\nmembers = [\"libs/*\", \"apps/*\"]\n\n[workspace.package]\nversion = \"1.2.0\"\ngroup = \"com.shop\"\n",
    );
    let package =
        |name: &str, targets: &str| format!("[package]\nname = \"{name}\"\nversion.workspace = true\ngroup.workspace = true\n\n{targets}");
    t.write("libs/core/kargo.toml", &package("core", "[target.jvm]\n\n[target.wasm]\n"));
    t.write("libs/core/src/Platform.kt", "package core\n\nexpect fun platform(): String\n");
    t.write("libs/core/src-jvm/Platform.kt", "package core\n\nactual fun platform(): String = \"jvm\"\n");
    t.write("libs/core/src-wasm/Platform.kt", "package core\n\nactual fun platform(): String = \"wasm\"\n");
    let core = |suffix: &str, shout: &str| {
        format!(
            "package core\n\nfun greet(name: String): String = \"Hello, $name from ${{platform()}}\" + suffix()\n\n\
             private fun suffix(): String = \"{suffix}\"\n\ninline fun shout(text: String): String = text.{shout}()\n"
        )
    };
    t.write("libs/core/src/Core.kt", &core("!", "uppercase"));
    t.write("libs/core/test-fixtures/Fixtures.kt", "package core\n\nobject Fixtures {\n    val visitor = \"tester\"\n}\n");

    t.write(
        "apps/server/kargo.toml",
        &format!(
            "{}[dependencies]\ncore = {{ path = \"../../libs/core\" }}\n\n[dev-dependencies]\ncore = {{ path = \"../../libs/core\", test-fixtures = true }}\n",
            package("server", "[target.jvm]\n\n")
        ),
    );
    t.write(
        "apps/server/src/Main.kt",
        "import core.greet\nimport core.shout\n\nfun main() {\n    println(greet(\"server\"))\n    println(shout(\"quiet\"))\n}\n",
    );
    t.write(
        "apps/server/tests/ServerTest.kt",
        "import kotlin.test.*\n\nclass ServerTest {\n    @Test fun greetsTheFixture() = assertTrue(core.greet(core.Fixtures.visitor).startsWith(\"Hello, tester\"))\n}\n",
    );
    t.write(
        "apps/web/kargo.toml",
        &format!("{}[dependencies]\ncore = {{ path = \"../../libs/core\" }}\n", package("web", "[target.wasm]\n\n")),
    );
    t.write("apps/web/src/Main.kt", "import core.greet\n\nfun main() {\n    println(greet(\"web\"))\n}\n");

    // The root builds every member, `core` first; each app runs with `-p`.
    t.kargo("").args(["build", "-j", "2"]).assert().success();
    assert!(t.dir.path().join("target/debug/core/jvm/core-1.2.0.jar").is_file());
    assert!(t.dir.path().join("target/debug/core/jvm/core-1.2.0.jar.abi").is_file());
    t.kargo("")
        .args(["run", "-p", "server"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello, server from jvm!").and(predicate::str::contains("QUIET")));
    t.kargo("").args(["run", "-p", "web"]).assert().success().stdout(predicate::str::contains("Hello, web from wasm!"));
    t.kargo("").args(["test", "-p", "server"]).assert().success();

    // A private function body: `core` recompiles, `server` doesn't, and runs the new code.
    t.write("libs/core/src/Core.kt", &core("!!", "uppercase"));
    t.kargo("")
        .args(["build", "-p", "server"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Compiling core v1.2.0 (jvm)").and(predicate::str::contains("Compiling server").not()));
    t.kargo("").args(["run", "-p", "server"]).assert().success().stdout(predicate::str::contains("Hello, server from jvm!!"));
    // The same for a public function that is not inline: only its signature is part of the ABI.
    t.write("libs/core/src/Core.kt", &core("!!", "uppercase").replace("Hello, ", "Hi, "));
    t.kargo("").args(["build", "-p", "server"]).assert().success().stderr(predicate::str::contains("Compiling server").not());
    t.kargo("").args(["run", "-p", "server"]).assert().success().stdout(predicate::str::contains("Hi, server from jvm!!"));

    // An inline function's body is compiled into its callers, so it is part of the ABI.
    t.write("libs/core/src/Core.kt", &core("!!", "lowercase"));
    t.kargo("").args(["build", "-p", "server"]).assert().success().stderr(predicate::str::contains("Compiling server v1.2.0 (jvm)"));
    t.kargo("").args(["run", "-p", "server"]).assert().success().stdout(predicate::str::contains("quiet"));

    // A failure names the member and the target.
    t.write("libs/core/src/Broken.kt", "package core\n\nval broken: String = 1\n");
    t.kargo("").arg("build").assert().failure().code(101).stderr(predicate::str::contains("could not build `core` (jvm)"));
}

/// A `mathlib` library and a `calc` application that uses it, both for the JVM and Wasm.
/// `calc` does not depend on `mathlib` yet.
fn calc_with_mathlib() -> E2e {
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
    t
}

#[test]
fn native_path_dependency() {
    require_e2e!();
    require_native!();
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

/// The `file:///` URL of a directory, for `[repositories]`.
fn file_url(dir: &std::path::Path) -> String {
    format!("file:///{}", dir.display().to_string().replace('\\', "/").trim_start_matches('/'))
}

#[test]
fn publish_a_jvm_library_and_use_it() {
    require_e2e!();
    let t = E2e::new();
    let maven_local = t.dir.path().join("m2");
    t.create(&["--lib", "mathlib"]);
    t.write(
        "mathlib/kargo.toml",
        "[package]\nname = \"mathlib\"\nversion = \"1.0.0\"\ngroup = \"com.example\"\ndescription = \"Math\"\nlicense = \"MIT\"\n\n\
         [target.jvm]\njvm-target = \"17\"\n\n[publish]\njavadoc = \"empty\"\n",
    );
    t.write("mathlib/src/Math.kt", "package com.example.mathlib\n\nfun twice(x: Int): Int = x * 2\n");

    // A library's `kargo package` is its own classes and sources, not a fat jar with the stdlib.
    let jar = t.kargo("mathlib").args(["package", "-q"]).assert().success().get_output().stdout.clone();
    let jar = String::from_utf8(jar).unwrap();
    assert!(jar.trim().ends_with("mathlib-1.0.0.jar"), "{jar}");
    let names = zip_names(std::path::Path::new(jar.trim()));
    assert!(names.iter().any(|n| n == "com/example/mathlib/MathKt.class") && !names.iter().any(|n| n.starts_with("kotlin/")), "{names:?}");

    t.kargo("mathlib")
        .args(["publish", "--local"])
        .env("KARGO_MAVEN_LOCAL", &maven_local)
        .assert()
        .success()
        .stderr(predicate::str::contains("Published com.example:mathlib:1.0.0 to maven-local"));
    let dir = maven_local.join("com/example/mathlib/1.0.0");
    for file in
        ["mathlib-1.0.0.jar", "mathlib-1.0.0-sources.jar", "mathlib-1.0.0-javadoc.jar", "mathlib-1.0.0.pom", "mathlib-1.0.0.jar.sha1"]
    {
        assert!(dir.join(file).is_file(), "{file} is published");
    }
    let pom = std::fs::read_to_string(dir.join("mathlib-1.0.0.pom")).unwrap();
    assert!(pom.contains("<artifactId>kotlin-stdlib</artifactId>") && pom.contains("<name>MIT</name>"), "{pom}");

    t.create(&["app"]);
    t.write("app/kargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[repositories]\nlocal = \"maven-local\"\n\n[dependencies]\n\"com.example:mathlib\" = \"1.0.0\"\n");
    t.write("app/src/Main.kt", "import com.example.mathlib.twice\n\nfun main() {\n    println(twice(21))\n}\n");
    t.kargo("app").arg("run").env("KARGO_MAVEN_LOCAL", &maven_local).assert().success().stdout(predicate::str::contains("42"));
}

#[test]
fn publish_a_multiplatform_library_and_use_it() {
    require_e2e!();
    let t = E2e::new();
    let repo = t.dir.path().join("repo");
    t.create(&["--lib", "kmp", "--target", "jvm,wasm"]);
    t.write(
        "kmp/kargo.toml",
        &format!(
            "[package]\nname = \"kmp\"\nversion = \"1.0.0\"\ngroup = \"com.example\"\n\n[target.jvm]\njvm-target = \"17\"\n\n[target.wasm]\n\n\
             [repositories]\nstaging = \"{}\"\n\n[publish]\nrepository = \"staging\"\n",
            file_url(&repo)
        ),
    );
    t.write(
        "kmp/src/Platform.kt",
        "package com.example.kmp\n\nexpect fun platform(): String\n\nfun hello(): String = \"Hello from ${platform()}\"\n",
    );
    t.write("kmp/src-jvm/Platform.jvm.kt", "package com.example.kmp\n\nactual fun platform(): String = \"JVM\"\n");
    t.write("kmp/src-wasm/Platform.wasm.kt", "package com.example.kmp\n\nactual fun platform(): String = \"Wasm\"\n");
    // The default javadoc jar comes from Dokka.
    t.kargo("kmp").arg("publish").assert().success().stderr(predicate::str::contains("Published com.example:kmp:1.0.0 to `staging`"));
    for file in [
        "kmp/1.0.0/kmp-1.0.0.module",
        "kmp/maven-metadata.xml",
        "kmp-jvm/1.0.0/kmp-jvm-1.0.0.jar",
        "kmp-jvm/1.0.0/kmp-jvm-1.0.0-javadoc.jar",
        "kmp-wasm-js/1.0.0/kmp-wasm-js-1.0.0.klib",
        "kmp-wasm-js/1.0.0/kmp-wasm-js-1.0.0.module",
    ] {
        assert!(repo.join("com/example").join(file).is_file(), "{file} is published");
    }
    assert!(zip_names(&repo.join("com/example/kmp-jvm/1.0.0/kmp-jvm-1.0.0-javadoc.jar")).iter().any(|n| n == "index.html"));

    t.create(&["app", "--target", "jvm,wasm"]);
    t.write(
        "app/kargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.jvm]\n\n[target.wasm]\n\n[repositories]\nstaging = \"{}\"\n\n\
             [dependencies]\n\"com.example:kmp\" = \"1.0.0\"\n",
            file_url(&repo)
        ),
    );
    t.write("app/src/Main.kt", "import com.example.kmp.hello\n\nfun main() {\n    println(hello())\n}\n");
    t.kargo("app").args(["run", "-t", "jvm"]).assert().success().stdout(predicate::str::contains("Hello from JVM"));
    t.kargo("app").args(["build", "-t", "wasm"]).assert().success();
    t.kargo("app").args(["tree", "-t", "wasm"]).assert().success().stdout(predicate::str::contains("com.example:kmp-wasm-js:1.0.0"));
}

fn zip_names(path: &std::path::Path) -> Vec<String> {
    let archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    archive.file_names().map(|n| n.unwrap().into_owned()).collect()
}

/// A DAP client talking to `kargo dap`.
struct Dap {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    messages: std::sync::mpsc::Receiver<serde_json::Value>,
    seq: u64,
    output: String,
}

impl Dap {
    fn start(dir: &std::path::Path) -> Self {
        use std::io::{BufRead, Read};
        let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("kargo"))
            .arg("dap")
            .current_dir(dir)
            .env_remove("KARGO_TARGET_DIR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let (tx, messages) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("Content-Length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                stdout.read_exact(&mut body).unwrap();
                if tx.send(serde_json::from_slice(&body).unwrap()).is_err() {
                    return;
                }
            }
        });
        Self { child, stdin, messages, seq: 0, output: String::new() }
    }

    fn send(&mut self, command: &str, arguments: serde_json::Value) {
        use std::io::Write;
        self.seq += 1;
        let body = serde_json::json!({ "seq": self.seq, "type": "request", "command": command, "arguments": arguments }).to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Waits for a message matching `want`, collecting the program's output on the way.
    fn wait(&mut self, want: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        loop {
            let message = self
                .messages
                .recv_timeout(std::time::Duration::from_secs(300))
                .unwrap_or_else(|_| panic!("no answer from kargo dap; output so far:\n{}", self.output));
            if message["event"] == "output" {
                self.output.push_str(message["body"]["output"].as_str().unwrap_or_default());
            }
            if want(&message) {
                return message;
            }
        }
    }

    /// Launches `kargo <args>` with a breakpoint, and checks the program stops there with the
    /// stack frame in the real source file.
    fn stops_at(&mut self, args: &[&str], file: &std::path::Path, line: u64) {
        self.send(
            "initialize",
            serde_json::json!({ "adapterID": "kargo", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path" }),
        );
        self.wait(|m| m["command"] == "initialize");
        self.send("launch", serde_json::json!({ "args": args }));
        let source = serde_json::json!({ "name": file.file_name().unwrap().to_str(), "path": file.display().to_string() });
        self.send("setBreakpoints", serde_json::json!({ "source": source, "breakpoints": [{ "line": line }] }));
        self.wait(|m| m["command"] == "setBreakpoints");
        self.send("configurationDone", serde_json::json!({}));
        let launched = self.wait(|m| m["command"] == "launch");
        assert_eq!(launched["success"], true, "{launched}\n{}", self.output);
        let stopped = self.wait(|m| m["event"] == "stopped");
        let thread = stopped["body"]["threadId"].clone();
        self.send("stackTrace", serde_json::json!({ "threadId": thread }));
        let trace = self.wait(|m| m["command"] == "stackTrace");
        let top = &trace["body"]["stackFrames"][0];
        assert_eq!(top["line"], line, "{trace}");
        assert_eq!(top["source"]["path"], file.display().to_string(), "{trace}");
        self.send("continue", serde_json::json!({ "threadId": thread }));
        self.wait(|m| m["event"] == "terminated" || m["event"] == "exited");
        self.send("disconnect", serde_json::json!({}));
    }
}

impl Drop for Dap {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn dap_debugs_programs_and_tests() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["dbg"]);
    t.write("dbg/src/Main.kt", "package com.example.dbg\n\nfun greeting(name: String): String = \"Hello, $name!\"\n\nfun main() {\n    println(greeting(\"Kotlin\"))\n}\n");
    t.write(
        "dbg/tests/GreetingTest.kt",
        "package com.example.dbg\n\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\nclass GreetingTest {\n    @Test\n    fun greets() {\n        assertEquals(\"Hello, Kargo!\", greeting(\"Kargo\"))\n    }\n}\n",
    );
    let dir = t.dir.path().join("dbg");
    let main = dir.join("src").join("Main.kt");

    let mut dap = Dap::start(&dir);
    dap.stops_at(&["run"], &main, 3);
    assert!(dap.output.contains("Hello, Kotlin!"), "the program ran to the end after continuing:\n{}", dap.output);

    let mut dap = Dap::start(&dir);
    dap.stops_at(&["test"], &dir.join("tests").join("GreetingTest.kt"), 9);
}

#[test]
fn scripts_run_with_their_arguments_and_are_cached() {
    require_e2e!();
    let e = E2e::new();
    e.write(
        "fetch.kts",
        "#!/usr/bin/env -S kargo script\n@file:DependsOn(\"org.jetbrains.kotlinx:kotlinx-coroutines-core-jvm:1.10.2\")\n\nimport kotlinx.coroutines.runBlocking\nimport kotlinx.coroutines.delay\n\nrunBlocking { delay(1) }\nprintln(\"hello ${args.joinToString(\"|\")}\")\n",
    );
    e.kargo("")
        .args(["script", "fetch.kts", "world", "--flag"])
        .assert()
        .success()
        .stdout(predicate::str::diff("hello world|--flag\n").normalize());
    // The second run starts the classes the first one compiled, so it does not compile again.
    e.kargo("")
        .args(["script", "fetch.kts", "again"])
        .assert()
        .success()
        .stdout(predicate::str::diff("hello again\n").normalize())
        .stderr(predicate::str::contains("Compiling").not());
}

#[test]
fn scripts_report_failures_like_programs() {
    require_e2e!();
    let e = E2e::new();
    e.write("boom.kts", "println(\"before\")\nthrow IllegalStateException(\"boom\")\n");
    e.kargo("")
        .args(["script", "boom.kts"])
        .assert()
        .code(1)
        .stdout(predicate::str::diff("before\n").normalize())
        .stderr(predicate::str::contains("boom"));
    // The cached classes throw the same way.
    e.kargo("").args(["script", "boom.kts"]).assert().code(1).stdout(predicate::str::diff("before\n").normalize());
    e.write("bad.kts", "println(undefinedThing)\n");
    e.kargo("").args(["script", "bad.kts"]).assert().code(101).stderr(predicate::str::contains("unresolved reference"));
}

#[test]
fn kotlin_files_with_front_matter_and_expressions() {
    require_e2e!();
    let e = E2e::new();
    e.write(
        "tool.kt",
        "/* kargo\n[dependencies]\n\"com.github.ajalt.clikt:clikt\" = \"5.0.0\"\n*/\nfun main(args: Array<String>) {\n    println(\"kt \" + args.joinToString())\n}\n",
    );
    e.kargo("").args(["script", "tool.kt", "a", "b"]).assert().success().stdout(predicate::str::diff("kt a, b\n").normalize());
    e.kargo("").args(["script", "tool.kt", "a", "b"]).assert().success().stdout(predicate::str::diff("kt a, b\n").normalize());
    e.kargo("").args(["script", "-e", "println(1 + 1)"]).assert().success().stdout(predicate::str::diff("2\n").normalize());
    e.kargo("")
        .args(["script", "-e", "println(args.joinToString(\"|\"))", "--", "--a", "b"])
        .assert()
        .success()
        .stdout(predicate::str::diff("--a|b\n").normalize());
    e.write("no-main.kt", "/* kargo\n*/\nval x = 1\n");
    e.kargo("").args(["script", "no-main.kt"]).assert().code(101).stderr(predicate::str::contains("no `main` function found"));
}

#[test]
fn script_annotations_set_kotlin_jdk_and_frameworks() {
    require_e2e!();
    let e = E2e::new();
    e.write(
        "settings.kts",
        "@file:Kotlin(\"2.4.21\")\n@file:Jdk(\"system\")\n@file:Framework(\"coroutines\")\nimport kotlinx.coroutines.*\nprintln(runBlocking { async { 6 * 7 }.await() })\n",
    );
    e.kargo("").args(["script", "settings.kts"]).assert().success().stdout(predicate::str::diff("42\n").normalize());
    e.write("dotted.main.kts", "println(\"dotted\")\n");
    e.kargo("").args(["script", "dotted.main.kts"]).assert().success().stdout(predicate::str::diff("dotted\n").normalize());
}

#[cfg(unix)]
#[test]
fn shebang_runs_the_script() {
    require_e2e!();
    use std::os::unix::fs::PermissionsExt;
    let e = E2e::new();
    e.write("hi.kts", "#!/usr/bin/env -S kargo script\nprintln(\"shebang \" + args.size)\n");
    let script = e.dir.path().join("hi.kts");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_kargo")).parent().unwrap();
    let path = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap_or_default());
    let output = StdCommand::new(&script).arg("x").env("PATH", path).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "shebang 1\n");
}

#[test]
fn scripts_exit_when_they_return_even_with_threads_left_running() {
    require_e2e!();
    let e = E2e::new();
    e.write("leftover.kts", "Thread { Thread.sleep(600_000) }.start()\nprintln(\"done\")\n");
    // Without the explicit exit, the thread would keep the first run, and then the cached one, alive for ten minutes.
    e.kargo("")
        .args(["script", "leftover.kts"])
        .timeout(std::time::Duration::from_secs(300))
        .assert()
        .success()
        .stdout(predicate::str::diff("done\n").normalize());
    e.kargo("")
        .args(["script", "leftover.kts"])
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success()
        .stdout(predicate::str::diff("done\n").normalize());
}

#[test]
fn jvm_run_watch_restarts_the_program_after_a_change() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    // The program prints once and exits, so nothing outlives the test but Kargo, which is killed at the end.
    t.write("app/src/Main.kt", "fun main() { println(\"first\") }\n");
    let stderr = std::fs::File::create(t.dir.path().join("watch.err")).unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("kargo"))
        .args(["run", "--watch"])
        .current_dir(t.dir.path().join("app"))
        .env_remove("KARGO_TARGET_DIR")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(stderr)
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)).map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut wait_for = |text: &str| {
        let timeout = std::time::Duration::from_secs(300);
        loop {
            match rx.recv_timeout(timeout) {
                Ok(line) if line == text => break,
                Ok(_) => {}
                Err(e) => {
                    let log = std::fs::read_to_string(t.dir.path().join("watch.err")).unwrap_or_default();
                    let _ = child.kill();
                    panic!("no `{text}` from kargo run --watch: {e}\n{log}");
                }
            }
        }
    };
    wait_for("first");
    t.write("app/src/Main.kt", "fun main() { println(\"second\") }\n");
    wait_for("second");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn jvm_hot_reload_swaps_a_method_body_without_a_restart() {
    require_e2e!();
    use std::io::BufRead;
    let t = E2e::new();
    t.create(&["app"]);
    // The program exits once `stop` exists: on Windows, killing `kargo` would leave it running.
    let program = |word: &str| {
        format!(
            "fun message() = \"{word}\"\n\nfun main() {{\n    val pid = ProcessHandle.current().pid()\n    while (!java.io.File(\"stop\").exists()) {{\n        \
             println(\"tick $pid ${{message()}}\")\n        Thread.sleep(100)\n    }}\n}}\n"
        )
    };
    t.write("app/src/Main.kt", &program("first"));
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("kargo"))
        .args(["run", "--hot"])
        .current_dir(t.dir.path().join("app"))
        .env_remove("KARGO_TARGET_DIR")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    for stream in [Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, Box::new(child.stderr.take().unwrap())] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    // The lines seen before one that contains `needle`, which is returned last.
    let wait_for = |needle: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
                let found = line.contains(needle);
                seen.push(line);
                if found {
                    return seen;
                }
            }
        }
        panic!("`{needle}` never appeared:\n{}", seen.join("\n"));
    };
    let pid = |lines: &[String]| lines.last().and_then(|l| l.split_whitespace().nth(1)).map(str::to_owned).unwrap();
    let first = pid(&wait_for("first"));
    t.write("app/src/Main.kt", &program("second"));
    let swapped = wait_for("second");
    let after = wait_for("Reloaded");
    std::fs::write(t.dir.path().join("app/stop"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(pid(&swapped), first, "the same process printed the new behavior:\n{swapped:?}");
    assert!(!swapped.iter().chain(&after).any(|l| l.contains("Running")), "the program was restarted:\n{swapped:?}\n{after:?}");
}

#[test]
fn custom_profiles_have_their_own_output_jvm_args_and_dependencies() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    t.write(
        "app/kargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[profile.dev]\njvm-args = [\"-Dapp.env=dev\"]\n\n\
         [profile.staging]\ninherits = \"release\"\ndebug = true\njvm-args = [\"-Dapp.env=staging\", \"-Xmx96m\"]\n\n\
         [profile.staging.dependencies]\n\"com.google.code.gson:gson\" = \"2.11.0\"\n",
    );
    // The template's test calls `greeting` in the package `com.example.app`, so it stays there.
    t.write(
        "app/src/Main.kt",
        "package com.example.app\n\nfun greeting(name: String) = \"Hello, $name!\"\n\nfun main() { println(\"env=${System.getProperty(\"app.env\")}\") }\n",
    );
    t.write(
        "app/tests/ProfileTest.kt",
        "import kotlin.test.*\nclass ProfileTest { @Test fun jvmArgsOfTheProfileApply() = assertEquals(\"staging\", System.getProperty(\"app.env\")) }\n",
    );

    // The profile builds into its own directory, with the dependencies only it has.
    t.kargo("app").args(["build", "--profile", "staging"]).assert().success().stderr(predicate::str::contains("`staging` profile"));
    let lib = |profile: &str| std::fs::read_dir(t.dir.path().join("app/target").join(profile).join("jvm/lib")).unwrap();
    assert!(lib("staging").any(|e| e.unwrap().file_name().to_string_lossy().starts_with("gson-")));
    t.kargo("app").arg("build").assert().success();
    assert!(lib("debug").all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("gson-")));
    // The lockfile resolves the profile's own dependencies apart from the shared resolution.
    let lock = std::fs::read_to_string(t.dir.path().join("app/kargo.lock")).unwrap();
    assert!(lock.contains("[resolution.\"jvm/runtime@staging\"]") && lock.contains("[resolution.\"jvm/runtime\"]"), "{lock}");

    // `jvm-args` of the profile reach the program (`run`) and the tests (`test`).
    t.kargo("app").arg("run").assert().success().stdout(predicate::str::contains("env=dev"));
    t.kargo("app").args(["run", "--profile", "staging"]).assert().success().stdout(predicate::str::contains("env=staging"));
    t.kargo("app").args(["test", "--profile", "staging"]).assert().success();
}

/// Runs `program` with neither `JAVA_HOME` nor a `java` on `PATH`, as on a machine without Java.
fn run_without_java(program: &std::path::Path, args: &[&str]) -> std::process::Output {
    let path = if cfg!(windows) {
        std::env::var("SystemRoot").map(|root| format!(r"{root}\System32")).unwrap_or_default()
    } else {
        "/usr/bin:/bin".to_owned()
    };
    StdCommand::new(program).args(args).env_remove("JAVA_HOME").env("PATH", path).output().unwrap()
}

#[test]
fn jvm_packages_runtime_images_and_app_images() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["app"]);
    t.write(
        "app/src/Main.kt",
        r#"fun main(args: Array<String>) { println(System.getProperty("greeting") + " " + args.joinToString("|")) }"#,
    );
    let manifest = std::fs::read_to_string(t.dir.path().join("app/kargo.toml")).unwrap();
    let extra = "[target.jvm]\njvm-args = [\"-Dgreeting=Hi there\"]\n\n[package.installer]\ntype = \"app-image\"\nconsole = true\n";
    t.write("app/kargo.toml", &format!("{manifest}\n{extra}"));

    // A runtime image runs on a machine without Java.
    let image = t.kargo("app").args(["package", "-q", "--format", "jlink"]).assert().success().get_output().stdout.clone();
    let image = std::path::PathBuf::from(String::from_utf8(image).unwrap().trim());
    assert_eq!(image.file_name().unwrap(), "app-0.1.0");
    let launcher = image.join("bin").join(if cfg!(windows) { "app.bat" } else { "app" });
    let out = run_without_java(&launcher, &["a b", "c"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hi there a b|c");
    let release = std::fs::read_to_string(image.join("runtime/release")).unwrap();
    assert!(release.contains("java.base") && !release.contains("java.desktop"), "a trimmed runtime: {release}");

    // So does jpackage's application image.
    let app = t.kargo("app").args(["package", "-q", "--format", "installer"]).assert().success().get_output().stdout.clone();
    let app = std::path::PathBuf::from(String::from_utf8(app).unwrap().trim());
    let launcher = match std::env::consts::OS {
        "windows" => app.join("app.exe"),
        "macos" => app.join("Contents/MacOS/app"),
        _ => app.join("bin/app"),
    };
    let out = run_without_java(&launcher, &["x"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hi there x");
}

#[test]
fn compose_desktop_resources_and_packages() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["ui", "--with", "compose"]);
    t.write(
        "ui/src/Main.kt",
        r#"package com.example.ui

import com.example.ui.generated.resources.*
import kotlinx.coroutines.runBlocking
import org.jetbrains.compose.resources.getPluralString
import org.jetbrains.compose.resources.getString

fun main(): Unit = runBlocking {
    println(getString(Res.string.app_name) + " " + getPluralString(Res.plurals.notes, 2, 2))
    println(Res.readBytes("files/data.json").decodeToString())
    // Loads Skiko's native library.
    println("skia " + org.jetbrains.skia.Surface.makeRasterN32Premul(4, 4).width)
}
"#,
    );
    t.write(
        "ui/composeResources/values/strings.xml",
        r#"<resources>
    <string name="app_name">Notes</string>
    <plurals name="notes"><item quantity="one">%1$d note</item><item quantity="other">%1$d notes</item></plurals>
</resources>
"#,
    );
    t.write("ui/composeResources/files/data.json", r#"{"ok":true}"#);
    let manifest = std::fs::read_to_string(t.dir.path().join("ui/kargo.toml")).unwrap();
    t.write("ui/kargo.toml", &format!("{manifest}\n[package.installer]\nname = \"Notes\"\ntype = \"app-image\"\nconsole = true\n"));
    let expected = "Notes 2 notes\n{\"ok\":true}\nskia 4";
    let run = t.kargo("ui").args(["run", "-q"]).assert().success().get_output().stdout.clone();
    assert_eq!(String::from_utf8_lossy(&run).replace("\r\n", "\n").trim(), expected);

    // The application image carries Skiko's library outside the jars and runs without Java.
    let app = t.kargo("ui").args(["package", "-q", "--desktop"]).assert().success().get_output().stdout.clone();
    let app = std::path::PathBuf::from(String::from_utf8(app).unwrap().trim());
    let (launcher, content) = match std::env::consts::OS {
        "windows" => (app.join("Notes.exe"), app.join("app")),
        "macos" => (app.join("Contents/MacOS/Notes"), app.join("Contents/app")),
        _ => (app.join("bin/Notes"), app.join("lib/app")),
    };
    let names: Vec<String> = std::fs::read_dir(&content).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|n| n.contains("skiko-") && !n.ends_with(".jar")), "{names:?}");
    assert!(!names.iter().any(|n| n.starts_with("skiko-awt-runtime-")), "{names:?}");
    let out = run_without_java(&launcher, &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n").trim(), expected, "{}", String::from_utf8_lossy(&out.stderr));

    // A jar for every platform.
    let jar = t
        .kargo("ui")
        .args(["package", "-q", "--desktop", "--format", "jar", "--platforms", "all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jar = std::path::PathBuf::from(String::from_utf8(jar).unwrap().trim());
    let archive = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let natives: Vec<String> =
        archive.file_names().map(|n| n.unwrap().into_owned()).filter(|n| n.contains("skiko-") && !n.ends_with(".sha256")).collect();
    for platform in ["windows-x64", "windows-arm64", "macos-x64", "macos-arm64", "linux-x64", "linux-arm64"] {
        assert!(natives.iter().any(|n| n.contains(platform)), "{platform} in {natives:?}");
    }
}

#[test]
fn jvm_builds_a_native_image_with_traced_configuration() {
    require_e2e!();
    if std::env::var_os("KARGO_E2E_GRAALVM").is_none() {
        eprintln!("skipping: set KARGO_E2E_GRAALVM=1 to build GraalVM native images");
        return;
    }
    let t = E2e::new();
    t.create(&["app"]);
    // Reflection that native-image cannot see without the tracing agent's configuration.
    t.write(
        "app/src/Main.kt",
        r#"package com.example.app

class Greeter { fun greet() = "Hello from reflection" }

fun main() {
    val name = listOf("com.example.app", "Greeter").joinToString(".")
    val greeter = Class.forName(name).getDeclaredConstructor().newInstance() as Greeter
    println(greeter.greet())
}
"#,
    );
    let manifest = std::fs::read_to_string(t.dir.path().join("app/kargo.toml")).unwrap();
    t.write("app/kargo.toml", &format!("{manifest}\n[toolchain]\ngraalvm = \"25\"\n"));

    t.kargo("app").args(["run", "--native-agent"]).assert().success().stdout(predicate::str::contains("Hello from reflection"));
    assert!(t.dir.path().join("app/native-image").read_dir().unwrap().next().is_some(), "the agent wrote configuration");
    let exe =
        t.kargo("app").args(["package", "-q", "--release", "--format", "native-image"]).assert().success().get_output().stdout.clone();
    let exe = std::path::PathBuf::from(String::from_utf8(exe).unwrap().trim());
    let out = run_without_java(&exe, &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello from reflection");
}

/// The launcher `kargo install` wrote for `name` below `root`.
fn installed_launcher(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let bin = root.join("bin");
    [format!("{name}.cmd"), name.to_owned(), format!("{name}.exe")]
        .iter()
        .map(|f| bin.join(f))
        .find(|f| f.is_file())
        .unwrap_or_else(|| panic!("no `{name}` in {}", bin.display()))
}

#[test]
fn install_a_jvm_tool() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["tool"]);
    std::fs::remove_file(t.dir.path().join("tool/tests/GreetingTest.kt")).unwrap();
    t.write("tool/src/Main.kt", "package m\nfun main(args: Array<String>) = println(\"tool \" + args.joinToString(\"|\"))\n");
    t.write("tool/src/tools/Seed.kt", "package m.tools\nfun main(args: Array<String>) = println(\"seed \" + args.joinToString(\"|\"))\n");
    let manifest = std::fs::read_to_string(t.dir.path().join("tool/kargo.toml")).unwrap();
    t.write(
        "tool/kargo.toml",
        &format!("{manifest}\n[target.jvm]\nmain-class = \"m.MainKt\"\n\n[[bin]]\nname = \"seed\"\npath = \"src/tools/Seed.kt\"\n"),
    );
    let root = tempfile::tempdir().unwrap();

    // Every entry point is installed: the project's own and the [[bin]].
    t.kargo("tool")
        .args(["install", "--path", ".", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed package `tool v0.1.0` (executables `seed`, `tool`)"));
    let run = |name: &str, args: &[&str]| {
        let out = StdCommand::new(installed_launcher(root.path(), name)).args(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(run("tool", &["a", "b"]), "tool a|b");
    assert_eq!(run("seed", &["x"]), "seed x");

    t.kargo("tool")
        .args(["install", "--list", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("tool v0.1.0 (").and(predicate::str::contains("    seed\n    tool\n")));

    // A second install needs --force; with --bin only that program is installed.
    t.kargo("tool")
        .args(["install", "--path", ".", "--root"])
        .arg(root.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("`tool` v0.1.0 is already installed"));
    t.write("tool/src/Main.kt", "package m\nfun main(args: Array<String>) = println(\"tool 2 \" + args.joinToString(\"|\"))\n");
    t.kargo("tool")
        .args(["install", "--path", ".", "--bin", "tool", "--force", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed package `tool v0.1.0` (executable `tool`)"));
    assert_eq!(run("tool", &["a"]), "tool 2 a");
    assert!(!root.path().join("bin/seed").exists() && !root.path().join("bin/seed.cmd").exists(), "seed is not installed any more");

    t.kargo("tool").args(["uninstall", "tool", "--root"]).arg(root.path()).assert().success();
    assert!(!root.path().join("bin/tool").exists() && !root.path().join("bin/tool.cmd").exists());
    assert!(!root.path().join("install/tool").exists());
}

#[test]
fn install_a_published_application_from_maven() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["greeter"]);
    std::fs::remove_file(t.dir.path().join("greeter/tests/GreetingTest.kt")).unwrap();
    t.write("greeter/src/Main.kt", "package g\nfun main(args: Array<String>) = println(\"greeter \" + args.joinToString(\"|\"))\n");
    let repo = tempfile::tempdir().unwrap();
    let url = format!("file:///{}", repo.path().display().to_string().replace('\\', "/").trim_start_matches('/'));
    t.kargo("greeter").args(["publish", "--dry-run", "--repository", "maven-local"]).assert().success();
    let staged = t.dir.path().join("greeter/target/release/publish/repository");
    kargo_util::fs::copy_dir(&staged, repo.path()).unwrap();

    let root = tempfile::tempdir().unwrap();
    t.kargo("greeter")
        .args(["install", "com.example:greeter:0.1.0", "--repository", &url, "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed package `greeter v0.1.0`"));
    let out = StdCommand::new(installed_launcher(root.path(), "greeter")).args(["hi", "there"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "greeter hi|there", "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn install_a_native_tool() {
    require_e2e!();
    require_native!();
    let t = E2e::new();
    t.create(&["nat", "--target", "native"]);
    let root = tempfile::tempdir().unwrap();
    t.kargo("nat")
        .args(["install", "--path", ".", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed package `nat v0.1.0` (executable `nat`)"));
    // No JVM involved: the executable is the program.
    let exe = root.path().join("bin").join(if cfg!(windows) { "nat.exe" } else { "nat" });
    let out = run_without_java(&exe, &[]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Hello, Kotlin!"), "{}", String::from_utf8_lossy(&out.stderr));
}

// `kargo import`: a Maven or Gradle project becomes a Kargo project that builds and passes its tests.

#[test]
fn import_spring_boot_maven_project() {
    require_e2e!();
    let t = E2e::new();
    let kotlin = kargo_manifest::DEFAULT_KOTLIN_VERSION;
    t.write(
        "sb/pom.xml",
        &format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>org.springframework.boot</groupId>
    <artifactId>spring-boot-starter-parent</artifactId>
    <version>3.3.5</version>
    <relativePath/>
  </parent>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>0.0.1-SNAPSHOT</version>
  <properties>
    <java.version>21</java.version>
    <kotlin.version>{kotlin}</kotlin.version>
  </properties>
  <dependencies>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>
    <dependency><groupId>com.fasterxml.jackson.module</groupId><artifactId>jackson-module-kotlin</artifactId></dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-reflect</artifactId></dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-stdlib</artifactId></dependency>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-test</artifactId><scope>test</scope></dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-test-junit5</artifactId><scope>test</scope></dependency>
  </dependencies>
  <build>
    <sourceDirectory>${{project.basedir}}/src/main/kotlin</sourceDirectory>
    <testSourceDirectory>${{project.basedir}}/src/test/kotlin</testSourceDirectory>
    <plugins>
      <plugin><groupId>org.springframework.boot</groupId><artifactId>spring-boot-maven-plugin</artifactId></plugin>
      <plugin>
        <groupId>org.jetbrains.kotlin</groupId>
        <artifactId>kotlin-maven-plugin</artifactId>
        <configuration>
          <args><arg>-Xjsr305=strict</arg></args>
          <compilerPlugins><plugin>spring</plugin></compilerPlugins>
        </configuration>
        <dependencies>
          <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-allopen</artifactId><version>${{kotlin.version}}</version></dependency>
        </dependencies>
      </plugin>
    </plugins>
  </build>
</project>
"#
        ),
    );
    t.write(
        "sb/src/main/kotlin/com/example/demo/DemoApplication.kt",
        r#"package com.example.demo

import org.springframework.boot.autoconfigure.SpringBootApplication
import org.springframework.boot.runApplication
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.RestController

@SpringBootApplication
class DemoApplication

@RestController
class Hello {
    @GetMapping("/")
    fun hello() = "hello"
}

fun main(args: Array<String>) {
    runApplication<DemoApplication>(*args)
}
"#,
    );
    t.write(
        "sb/src/test/kotlin/com/example/demo/DemoApplicationTests.kt",
        r#"package com.example.demo

import kotlin.test.Test
import kotlin.test.assertEquals

class DemoApplicationTests {
    @Test
    fun hello() = assertEquals("hello", Hello().hello())
}
"#,
    );
    // `kargo import` builds the project at the end, and fails when it does not compile.
    t.kargo("sb").arg("import").assert().success().stderr(predicate::str::contains("[frameworks] spring-boot"));
    let manifest = std::fs::read_to_string(t.dir.path().join("sb/kargo.toml")).unwrap();
    assert!(manifest.contains("spring-boot = { version = \"3.3.5\", starters = [\"web\"] }"), "{manifest}");
    assert!(t.dir.path().join("sb/src/com/example/demo/DemoApplication.kt").is_file());
    t.kargo("sb").arg("test").assert().success();
}

#[test]
fn import_ktor_gradle_project() {
    require_e2e!();
    let t = E2e::new();
    let kotlin = kargo_manifest::DEFAULT_KOTLIN_VERSION;
    t.write("kt/settings.gradle.kts", "rootProject.name = \"ktor-sample\"\n");
    t.write("kt/gradle.properties", &format!("kotlin.code.style=official\nkotlin_version={kotlin}\nlogback_version=1.4.14\n"));
    t.write(
        "kt/build.gradle.kts",
        &format!(
            r#"val kotlin_version: String by project
val logback_version: String by project

plugins {{
    kotlin("jvm") version "{kotlin}"
    id("io.ktor.plugin") version "3.0.0"
    id("org.jetbrains.kotlin.plugin.serialization") version "{kotlin}"
}}

group = "com.example"
version = "0.0.1"

application {{
    mainClass.set("io.ktor.server.netty.EngineMain")
}}

repositories {{
    mavenCentral()
}}

dependencies {{
    implementation("io.ktor:ktor-server-core-jvm")
    implementation("io.ktor:ktor-server-content-negotiation-jvm")
    implementation("io.ktor:ktor-serialization-kotlinx-json-jvm")
    implementation("io.ktor:ktor-server-netty-jvm")
    implementation("ch.qos.logback:logback-classic:$logback_version")
    implementation("io.ktor:ktor-server-config-yaml")
    testImplementation("io.ktor:ktor-server-test-host-jvm")
    testImplementation("org.jetbrains.kotlin:kotlin-test-junit:$kotlin_version")
}}
"#
        ),
    );
    t.write(
        "kt/src/main/kotlin/Application.kt",
        r#"package com.example

import io.ktor.http.ContentType
import io.ktor.serialization.kotlinx.json.json
import io.ktor.server.application.Application
import io.ktor.server.application.install
import io.ktor.server.plugins.contentnegotiation.ContentNegotiation
import io.ktor.server.response.respondText
import io.ktor.server.routing.get
import io.ktor.server.routing.routing

fun main(args: Array<String>) {
    io.ktor.server.netty.EngineMain.main(args)
}

fun Application.module() {
    install(ContentNegotiation) { json() }
    routing {
        get("/") { call.respondText("Hello World!", ContentType.Text.Plain) }
    }
}
"#,
    );
    t.write(
        "kt/src/main/resources/application.yaml",
        "ktor:\n    application:\n        modules:\n            - com.example.ApplicationKt.module\n    deployment:\n        port: 8080\n",
    );
    t.write(
        "kt/src/test/kotlin/ApplicationTest.kt",
        r#"package com.example

import io.ktor.client.request.get
import io.ktor.client.statement.bodyAsText
import io.ktor.http.HttpStatusCode
import io.ktor.server.testing.testApplication
import kotlin.test.Test
import kotlin.test.assertEquals

class ApplicationTest {
    @Test
    fun testRoot() = testApplication {
        application { module() }
        val response = client.get("/")
        assertEquals(HttpStatusCode.OK, response.status)
        assertEquals("Hello World!", response.bodyAsText())
    }
}
"#,
    );
    t.kargo("kt").arg("import").assert().success().stderr(predicate::str::contains("[frameworks] ktor"));
    t.kargo("kt").arg("test").assert().success();
}

#[test]
fn import_multiplatform_gradle_project() {
    require_e2e!();
    let t = E2e::new();
    let kotlin = kargo_manifest::DEFAULT_KOTLIN_VERSION;
    t.write("kmp/settings.gradle.kts", "rootProject.name = \"shared\"\n");
    t.write(
        "kmp/build.gradle.kts",
        &format!(
            r#"plugins {{
    kotlin("multiplatform") version "{kotlin}"
}}

group = "org.example"
version = "1.0"

kotlin {{
    jvm()
    wasmJs {{
        nodejs()
        binaries.executable()
    }}
    linuxX64 {{
        binaries {{
            executable {{
                entryPoint = "main"
            }}
        }}
    }}

    sourceSets {{
        commonMain.dependencies {{
            implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.1")
        }}
        commonTest.dependencies {{
            implementation(kotlin("test"))
        }}
    }}
}}
"#
        ),
    );
    t.write(
        "kmp/src/commonMain/kotlin/Greeting.kt",
        "fun greeting(name: String): String = \"Hello, $name!\"\n\nfun main() {\n    println(greeting(\"KMP\"))\n}\n",
    );
    t.write(
        "kmp/src/commonTest/kotlin/GreetingTest.kt",
        "import kotlin.test.Test\nimport kotlin.test.assertEquals\n\nclass GreetingTest {\n    @Test\n    fun greets() = assertEquals(\"Hello, Kargo!\", greeting(\"Kargo\"))\n}\n",
    );
    t.write("kmp/src/jvmMain/kotlin/Jvm.kt", "fun jvmOnly() = 1\n");
    // Linking Kotlin/Native is slow, so the import does not build it; the JVM and Wasm targets are built here.
    t.kargo("kmp").args(["import", "--no-build"]).assert().success();
    let manifest = std::fs::read_to_string(t.dir.path().join("kmp/kargo.toml")).unwrap();
    for table in [
        "[target.jvm]",
        "[target.wasm]\nenvironment = \"js\"",
        "[target.native]\nplatforms = [\"linuxX64\"]\nkind = \"executable\"\nentry-point = \"main\"",
    ] {
        assert!(manifest.contains(table), "{table} in {manifest}");
    }
    for file in ["src/Greeting.kt", "tests/GreetingTest.kt", "src-jvm/Jvm.kt"] {
        assert!(t.dir.path().join("kmp").join(file).is_file(), "{file}");
    }
    t.kargo("kmp").args(["build", "--target", "jvm", "--target", "wasm"]).assert().success();
    t.kargo("kmp").args(["test", "--target", "jvm"]).assert().success();
}

/// A `[[generate]]` step that writes `class <Name>` for every `messages/<Name>.txt`.
fn message_step() -> String {
    let posix = r#"for f in messages/*.txt; do n=$(basename "$f" .txt); echo "class $n" > "$KARGO_GEN_DIR/$n.kt"; done"#;
    let windows = r#"for %f in (messages\*.txt) do @echo class %~nf> "%KARGO_GEN_DIR%\%~nf.kt""#;
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    format!(
        "\n[[generate]]\nname = \"msgs\"\ninputs = [\"messages/*.txt\"]\nrun = '{posix}'\nwindows-run = '{windows}'\nshell = \"{shell}\"\n"
    )
}

#[test]
fn generated_sources_are_compiled_and_regenerated_only_when_inputs_change() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["gen"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("gen/kargo.toml")).unwrap();
    t.write("gen/kargo.toml", &format!("{manifest}{}", message_step()));
    t.write("gen/messages/Hello.txt", "hello");
    t.write("gen/messages/Bye.txt", "bye");
    t.write("gen/src/Main.kt", "fun main() {\n    println(Hello::class.simpleName)\n    println(Bye::class.simpleName)\n}\n");
    let classes = t.dir.path().join("gen/target/debug/jvm/classes");

    t.kargo("gen")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating `msgs`").and(predicate::str::contains("Compiling gen v0.1.0 (jvm)")));
    assert!(t.dir.path().join("gen/target/generated/debug/msgs/Hello.kt").is_file());
    assert!(!t.dir.path().join("gen/src/Hello.kt").exists(), "nothing is written into src/");
    t.kargo("gen").arg("run").assert().success().stdout(predicate::str::contains("Hello").and(predicate::str::contains("Bye")));

    // Unchanged inputs: no generator, no compiler.
    t.kargo("gen")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating").not().and(predicate::str::contains("Compiling").not()));

    // An input that changes without changing the output runs the generator but not the compiler.
    t.write("gen/messages/Hello.txt", "hello again");
    t.kargo("gen")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating `msgs`").and(predicate::str::contains("Compiling").not()));

    // Deleting an input removes the class it generated.
    assert!(classes.join("Bye.class").is_file());
    std::fs::remove_file(t.dir.path().join("gen/messages/Bye.txt")).unwrap();
    t.write("gen/src/Main.kt", "fun main() {\n    println(Hello::class.simpleName)\n}\n");
    t.kargo("gen")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating `msgs`").and(predicate::str::contains("Compiling gen v0.1.0 (jvm)")));
    assert!(!t.dir.path().join("gen/target/generated/debug/msgs/Bye.kt").exists());
    assert!(!classes.join("Bye.class").exists(), "the class of a deleted message is gone");
    assert!(classes.join("Hello.class").is_file());
}

#[test]
fn build_config_exposes_the_package_to_the_program() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["cfg"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("cfg/kargo.toml")).unwrap();
    t.write(
        "cfg/kargo.toml",
        &format!(
            "{manifest}\n[[generate]]\nname = \"config\"\nbuiltin = \"build-config\"\npackage = \"cfg.build\"\nfields = {{ API_URL = \"https://api.test\", RETRIES = 3 }}\n"
        ),
    );
    t.write(
        "cfg/src/Main.kt",
        "import cfg.build.BuildConfig\n\nfun main() {\n    println(\"${BuildConfig.NAME} ${BuildConfig.VERSION} ${BuildConfig.PROFILE} ${BuildConfig.API_URL} ${BuildConfig.RETRIES} [${BuildConfig.GIT_SHA}]\")\n}\n",
    );
    t.kargo("cfg").arg("run").assert().success().stdout(predicate::str::contains("cfg 0.1.0 dev https://api.test 3 ["));
    t.kargo("cfg").args(["run", "--release"]).assert().success().stdout(predicate::str::contains("cfg 0.1.0 release https://api.test 3"));
    // The next build does not compile again for a file whose contents did not change.
    t.kargo("cfg").arg("build").assert().success().stderr(predicate::str::contains("Compiling").not());

    // In a Git repository, GIT_SHA is the commit that HEAD points to.
    let git = |args: &[&str]| {
        let out = StdCommand::new("git")
            .args(["-c", "user.email=kargo@example.com", "-c", "user.name=Kargo"])
            .args(args)
            .current_dir(t.dir.path().join("cfg"))
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    if which("git").is_some() {
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "first"]);
        let sha = git(&["rev-parse", "HEAD"]);
        t.kargo("cfg").arg("run").assert().success().stdout(predicate::str::contains(format!("[{sha}]")));
    }
}

#[test]
fn a_tool_from_maven_generates_sources() {
    require_e2e!();
    let (Some(javac), Some(jar)) = (which("javac"), which("jar")) else {
        eprintln!("skipping: javac and jar are needed to build the tool");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("Gen.java");
    std::fs::write(
        &source,
        "package tool;\n\npublic class Gen {\n    public static void main(String[] args) throws Exception {\n        \
         java.nio.file.Files.writeString(java.nio.file.Path.of(args[0], \"Answer.kt\"), \"const val ANSWER = \" + args[1] + \"\\n\");\n    }\n}\n",
    )
    .unwrap();
    let classes = work.path().join("classes");
    std::fs::create_dir_all(&classes).unwrap();
    assert!(StdCommand::new(javac).arg("-d").arg(&classes).arg(&source).status().unwrap().success());
    let tool_jar = work.path().join("gen-tool.jar");
    assert!(
        StdCommand::new(jar)
            .arg("--create")
            .arg("--file")
            .arg(&tool_jar)
            .args(["--main-class", "tool.Gen", "-C"])
            .arg(&classes)
            .arg(".")
            .status()
            .unwrap()
            .success()
    );

    let repo = common::LocalRepo::new();
    repo.publish("tools:gen-tool", "1.0", &[]);
    std::fs::copy(&tool_jar, repo.dir.path().join("tools/gen-tool/1.0/gen-tool-1.0.jar")).unwrap();

    let t = E2e::new();
    t.create(&["tooled"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("tooled/kargo.toml")).unwrap();
    t.write(
        "tooled/kargo.toml",
        &format!(
            "{manifest}\n[repositories]\nlocal = \"{}\"\n\n[[generate]]\nname = \"answer\"\ninputs = [\"answer.txt\"]\ntool = \"tools:gen-tool:1.0\"\nargs = [\"$KARGO_GEN_DIR\", \"42\"]\n",
            repo.url()
        ),
    );
    t.write("tooled/answer.txt", "42");
    t.write("tooled/src/Main.kt", "fun main() {\n    println(\"answer=$ANSWER\")\n}\n");
    t.kargo("tooled").arg("run").assert().success().stdout(predicate::str::contains("answer=42"));
    // The tool is pinned in kargo.lock.
    let lock = std::fs::read_to_string(t.dir.path().join("tooled/kargo.lock")).unwrap();
    assert!(lock.contains("generate/answer") && lock.contains("tools:gen-tool"), "{lock}");
    t.kargo("tooled").args(["build", "--locked"]).assert().success().stderr(predicate::str::contains("Generating").not());
    // `kargo update` locks the tool again, so a locked build still finds it.
    t.kargo("tooled").arg("update").assert().success();
    assert!(std::fs::read_to_string(t.dir.path().join("tooled/kargo.lock")).unwrap().contains("generate/answer"));
    t.kargo("tooled").args(["build", "--locked"]).assert().success();
}

const DEMO_PROTO: &str = r#"syntax = "proto3";

package demo;

option java_package = "demo.proto";
option java_multiple_files = true;

message Person {
  string name = 1;
  int32 id = 2;
  string email = 3;
}

service Greeter {
  rpc SayHello (Person) returns (Person);
}
"#;

#[test]
fn protobuf_and_grpc_are_generated_from_proto_files() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["rpc"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("rpc/kargo.toml")).unwrap();
    t.write(
        "rpc/kargo.toml",
        &format!(
            "{manifest}\n[dependencies]\n\"com.google.protobuf:protobuf-kotlin\" = \"4.31.1\"\n\"io.grpc:grpc-protobuf\" = \"1.73.0\"\n\
             \"io.grpc:grpc-stub\" = \"1.73.0\"\n\"io.grpc:grpc-kotlin-stub\" = \"1.4.3\"\n\
             \"org.jetbrains.kotlinx:kotlinx-coroutines-core\" = \"1.10.2\"\n\n\
             [[generate]]\nname = \"protos\"\nbuiltin = \"protobuf\"\ngrpc = true\n"
        ),
    );
    t.write("rpc/proto/demo.proto", DEMO_PROTO);
    t.write(
        "rpc/src/Main.kt",
        "import demo.proto.GreeterGrpc\nimport demo.proto.GreeterGrpcKt\nimport demo.proto.person\n\n\
         fun main() {\n    val p = person {\n        name = \"Ada\"\n        id = 7\n    }\n    \
         println(\"${p.name}#${p.id} ${GreeterGrpc.SERVICE_NAME} ${GreeterGrpcKt.SERVICE_NAME}\")\n}\n",
    );

    t.kargo("rpc")
        .arg("run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating `protos` with protoc"))
        .stdout(predicate::str::contains("Ada#7 demo.Greeter demo.Greeter"));
    let generated = t.dir.path().join("rpc/target/generated/debug/protos");
    assert!(generated.join("demo/proto/Person.java").is_file());
    assert!(generated.join("demo/proto/PersonKt.kt").is_file());
    assert!(generated.join("demo/proto/GreeterGrpc.java").is_file());
    assert!(generated.join("demo/proto/DemoGrpcKt.kt").is_file());
    assert!(!t.dir.path().join("rpc/src/demo").exists(), "nothing is written into src/");

    // The next build neither runs protoc nor compiles.
    t.kargo("rpc")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating").not().and(predicate::str::contains("Compiling").not()));
    t.kargo("rpc").args(["build", "--locked"]).assert().success();

    // A new field regenerates and recompiles; a deleted message removes its generated class.
    t.write("rpc/proto/demo.proto", &DEMO_PROTO.replace("string email = 3;", "string email = 3;\n  bool admin = 4;"));
    t.kargo("rpc")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating `protos`").and(predicate::str::contains("Compiling")));
    t.write("rpc/proto/extra.proto", "syntax = \"proto3\";\npackage demo;\noption java_package = \"demo.proto\";\noption java_multiple_files = true;\nmessage Extra { int32 n = 1; }\n");
    t.kargo("rpc").arg("build").assert().success();
    assert!(generated.join("demo/proto/Extra.java").is_file());
    std::fs::remove_file(t.dir.path().join("rpc/proto/extra.proto")).unwrap();
    t.kargo("rpc").arg("build").assert().success();
    assert!(!generated.join("demo/proto/Extra.java").exists());
    let classes = t.dir.path().join("rpc/target/debug/jvm/classes/demo/proto");
    assert!(!classes.join("Extra.class").exists(), "the class of a deleted message is gone");
}

#[test]
fn an_openapi_client_is_generated_with_the_tool_of_a_step() {
    require_e2e!();
    let t = E2e::new();
    t.create(&["pets"]);
    let manifest = std::fs::read_to_string(t.dir.path().join("pets/kargo.toml")).unwrap();
    t.write(
        "pets/kargo.toml",
        &format!(
            "{manifest}\n[dependencies]\n\"com.squareup.okhttp3:okhttp\" = \"4.12.0\"\n\"com.squareup.moshi:moshi-kotlin\" = \"1.15.2\"\n\
             \"com.squareup.moshi:moshi-adapters\" = \"1.15.2\"\n\n[[generate]]\nname = \"api-client\"\ninputs = [\"openapi.yaml\"]\n\
             tool = \"org.openapitools:openapi-generator-cli:7.16.0\"\n\
             args = [\"generate\", \"-g\", \"kotlin\", \"-i\", \"openapi.yaml\", \"-o\", \"$KARGO_GEN_DIR\"]\nroots = [\"src/main/kotlin\"]\n"
        ),
    );
    t.write(
        "pets/openapi.yaml",
        "openapi: 3.0.3\ninfo:\n  title: Pets\n  version: 1.0.0\npaths:\n  /pets:\n    get:\n      operationId: listPets\n      responses:\n        '200':\n          \
         description: ok\n          content:\n            application/json:\n              schema:\n                type: array\n                items:\n                  \
         $ref: '#/components/schemas/Pet'\ncomponents:\n  schemas:\n    Pet:\n      type: object\n      properties:\n        id:\n          type: integer\n          \
         format: int64\n        name:\n          type: string\n",
    );
    t.write("pets/src/Main.kt", "import org.openapitools.client.models.Pet\n\nfun main() {\n    println(Pet(id = 1, name = \"Rex\"))\n}\n");
    t.kargo("pets").arg("run").assert().success().stdout(predicate::str::contains("Pet(id=1, name=Rex)"));
    // The whole project the generator writes stays in target/; only its Kotlin sources are compiled, not the test stubs.
    let generated = t.dir.path().join("pets/target/generated/debug/api-client");
    assert!(generated.join("src/test/kotlin").is_dir() && generated.join("build.gradle").is_file());
    assert!(!t.dir.path().join("pets/target/debug/jvm/classes/org/openapitools/client/models/PetTest.class").exists());
    t.kargo("pets")
        .arg("build")
        .assert()
        .success()
        .stderr(predicate::str::contains("Generating").not().and(predicate::str::contains("Compiling").not()));
}
