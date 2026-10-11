//! Integration tests for the `kargo` CLI that need neither a JDK nor network
//! access (dependencies come from local `file://` repositories).

mod common;

use common::{LocalRepo, Project, manifest};
use predicates::prelude::*;

#[test]
fn help_and_version() {
    let p = Project::new();
    p.kargo().arg("--help").assert().success().stdout(predicate::str::contains("Kargo builds Kotlin projects"));
    p.kargo().arg("--version").assert().success().stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn new_creates_a_project() {
    let p = Project::new();
    p.kargo()
        .args(["new", "my-app", "--no-vcs", "--with", "serialization", "--target", "jvm,wasm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Created application `my-app` package"));
    let root = p.path().join("my-app");
    let manifest = std::fs::read_to_string(root.join("kargo.toml")).unwrap();
    assert!(manifest.contains("[target.wasm]"));
    assert!(manifest.contains("serialization = true"));
    assert!(root.join("src/Main.kt").is_file());
    assert!(root.join("tests/GreetingTest.kt").is_file());
    assert!(root.join(".gitignore").is_file());
    assert!(!root.join(".git").exists(), "--no-vcs");

    p.kargo().args(["new", "my-app"]).assert().failure().code(101).stderr(predicate::str::contains("already exists"));
}

#[test]
fn new_rejects_invalid_input() {
    let p = Project::new();
    p.kargo().args(["new", "9lives", "--no-vcs"]).assert().failure().stderr(predicate::str::contains("must start with a letter"));
    p.kargo().args(["new", "x", "--no-vcs", "--with", "rails"]).assert().failure().stderr(predicate::str::contains("unknown framework"));
    p.kargo().args(["new", "y", "--target", "ios"]).assert().failure().stderr(predicate::str::contains("invalid value"));
}

#[test]
fn new_escapes_keyword_names_in_the_package() {
    let p = Project::new();
    p.kargo().args(["new", "class", "--no-vcs"]).assert().success();
    let main = std::fs::read_to_string(p.path().join("class/src/Main.kt")).unwrap();
    assert!(main.starts_with("package com.example.class_\n"), "{main}");
}

#[test]
fn new_with_invalid_input_leaves_nothing_behind() {
    let p = Project::new();
    for name in ["9lives", "Bad Name", "ünï"] {
        p.kargo().args(["new", name, "--no-vcs"]).assert().failure().code(101);
        assert!(!p.path().join(name).exists(), "`kargo new {name}` left a directory behind");
    }
    p.kargo().args(["new", "x", "--no-vcs", "--with", "rails"]).assert().failure().stderr(predicate::str::contains("unknown framework"));
    assert!(!p.path().join("x").exists());
    p.kargo().args(["new", "9lives", "--no-vcs", "--name", "lives"]).assert().success();
    assert!(p.path().join("9lives/kargo.toml").is_file());
}

#[test]
fn init_uses_directory_name_and_keeps_files() {
    let p = Project::new();
    let dir = p.path().join("existing-proj");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.kt"), "fun main() {}").unwrap();
    p.kargo().args(["init", "--no-vcs"]).arg(&dir).assert().success();
    assert!(std::fs::read_to_string(dir.join("kargo.toml")).unwrap().contains("name = \"existing-proj\""));
    assert_eq!(std::fs::read_to_string(dir.join("src/Main.kt")).unwrap(), "fun main() {}");
    p.kargo().args(["init", "--no-vcs"]).arg(&dir).assert().failure().stderr(predicate::str::contains("already contains"));
}

#[test]
fn missing_manifest_is_reported() {
    let p = Project::new();
    p.kargo().arg("build").assert().failure().code(101).stderr(predicate::str::contains("could not find `kargo.toml`"));
}

#[test]
fn invalid_manifest_is_reported_with_context() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("bad", "[dependencies]\n\"a:b\" = 3\n"));
    p.kargo()
        .arg("info")
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse manifest"))
        .stderr(predicate::str::contains("invalid dependency `a:b`"));
}

#[test]
fn info_describes_the_project() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("infoapp", "[frameworks]\ncoroutines = true\n[target.native]\n[target.jvm]\n"));
    p.kargo()
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("infoapp v0.1.0"))
        .stdout(predicate::str::contains("targets:      jvm, native"))
        .stdout(predicate::str::contains("frameworks:   coroutines"));
}

#[test]
fn add_and_remove_dependencies() {
    let repo = LocalRepo::new();
    repo.publish("com.acme:widget", "1.0.0", &[]).publish("com.acme:widget", "1.2.0", &[]).publish("com.acme:widget", "2.0.0-beta1", &[]);
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", &format!("\n[repositories]\nlocal = \"{}\"\n", repo.url())));

    p.kargo().args(["add", "com.acme:widget"]).assert().success().stderr(predicate::str::contains("com.acme:widget:1.2.0"));
    assert!(p.read("kargo.toml").contains("\"com.acme:widget\" = \"1.2.0\""));

    p.kargo().args(["add", "com.acme:widget:1.0.0", "--dev"]).assert().success();
    assert!(p.read("kargo.toml").contains("[dev-dependencies]"));

    p.kargo().args(["add", "com.acme:widget", "--target", "jvm", "--scope", "runtime"]).assert().success();
    assert!(p.read("kargo.toml").contains("[target.jvm.dependencies]"));

    p.kargo().args(["add", "com.acme:missing:1.0"]).assert().failure().stderr(predicate::str::contains("not found"));
    p.kargo().args(["add", "--framework", "coroutines"]).assert().success();
    assert!(p.read("kargo.toml").contains("coroutines = true"));

    p.kargo().args(["remove", "com.acme:widget"]).assert().success();
    p.kargo().args(["remove", "com.acme:widget", "--dev"]).assert().success();
    p.kargo().args(["remove", "com.acme:widget"]).assert().failure().stderr(predicate::str::contains("not declared"));
}

#[test]
fn tree_resolves_from_local_repository_and_writes_lockfile() {
    let repo = LocalRepo::new();
    repo.publish("g:lib", "1.0", &[("g:util", "1.0")])
        .publish("g:other", "1.0", &[("g:util", "2.0")])
        .publish("g:util", "1.0", &[])
        .publish("g:util", "2.0", &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "treeapp",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n\"g:other\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );
    // The stdlib is not in the local repository: point it at the local repo too.
    repo.publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[]);

    let out = p.kargo().args(["tree", "--offline"]).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("treeapp v0.1.0 (jvm)"), "{text}");
    assert!(text.contains("g:util:1.0 -> 2.0"), "{text}");
    assert!(text.contains("g:util:2.0"), "{text}");

    let lock = p.read("kargo.lock");
    assert!(lock.contains("[resolution.\"jvm/runtime\"]"), "{lock}");
    assert!(lock.contains("sha256"));

    p.kargo()
        .args(["tree", "--duplicates", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("g:util: requested 1.0, 2.0 -> selected 2.0"));

    // --locked succeeds while the lockfile is current, and fails after a manifest change.
    p.kargo().args(["tree", "--locked", "--offline"]).assert().success();
    let changed = p.read("kargo.toml").replace("\"g:other\" = \"1.0\"\n", "");
    p.file("kargo.toml", &changed);
    p.kargo().args(["tree", "--locked", "--offline"]).assert().failure().stderr(predicate::str::contains("--locked"));
}

#[test]
fn corrupt_cached_artifact_is_redownloaded_before_locking() {
    let repo = LocalRepo::new();
    repo.publish("g:lib", "1.0", &[]).publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "corrupt",
            &format!("[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n", repo.url()),
        ),
    );
    let cached = p.home.path().join("cache/maven/g/lib/1.0/lib-1.0.jar");
    let corrupt = || {
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, "truncated garbage").unwrap();
    };

    // A garbage jar already in the cache is replaced before its hash is locked.
    corrupt();
    p.kargo().args(["tree", "--offline"]).assert().success().stderr(predicate::str::contains("downloading it again"));
    assert_eq!(std::fs::read_to_string(&cached).unwrap(), "jar g:lib 1.0");
    let lock = p.read("kargo.lock");

    // Re-downloading from the lockfile matches the recorded hash.
    std::fs::remove_file(&cached).unwrap();
    p.kargo().args(["tree", "--offline"]).assert().success();
    assert_eq!(p.read("kargo.lock"), lock);

    // --refresh heals a cached file that went bad after locking.
    corrupt();
    p.kargo().args(["tree", "--offline", "--refresh"]).assert().success();
    assert_eq!(std::fs::read_to_string(&cached).unwrap(), "jar g:lib 1.0");
    assert_eq!(p.read("kargo.lock"), lock);
}

#[test]
fn offline_mode_reports_missing_artifacts() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("off", "[dependencies]\n\"never:seen\" = \"1.0\"\n"));
    p.kargo().args(["tree", "--offline"]).assert().failure().stderr(predicate::str::contains("offline"));
}

#[test]
fn tasks_list_run_and_dependencies() {
    let p = Project::new();
    let (a, b) = if cfg!(windows) {
        ("echo a>> log.txt", "echo b %KARGO_PKG_NAME%>> log.txt")
    } else {
        ("echo a >> log.txt", "echo b $KARGO_PKG_NAME >> log.txt")
    };
    p.file(
        "kargo.toml",
        &manifest(
            "tasky",
            &format!(
                "[tasks]\nfirst = {{ run = '{a}', description = \"first step\" }}\nsecond = {{ run = '{b}', depends-on = [\"first\"] }}\n"
            ),
        ),
    );
    p.kargo().arg("task").assert().success().stdout(predicate::str::contains("first step")).stdout(predicate::str::contains("second"));
    p.kargo().args(["task", "second"]).assert().success();
    let log = p.read("log.txt");
    let lines: Vec<&str> = log.lines().map(str::trim).collect();
    assert_eq!(lines, ["a", "b tasky"]);

    // Tasks can also be invoked as `kargo <task>`.
    p.kargo().arg("first").assert().success();
    assert_eq!(p.read("log.txt").lines().count(), 3);

    p.kargo().args(["task", "nope"]).assert().failure().stderr(predicate::str::contains("available tasks: first, second"));
}

#[test]
fn failing_task_propagates_error() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("failing", "[tasks]\nboom = \"exit 9\"\n"));
    p.kargo().args(["task", "boom"]).assert().failure().code(101).stderr(predicate::str::contains("exit code 9"));
}

#[test]
fn external_subcommands_from_kargo_home() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("ext", ""));
    let bin = p.home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    if cfg!(windows) {
        std::fs::write(bin.join("kargo-greet.bat"), "@echo greet %1 %KARGO_PKG_NAME%\r\n@exit /b 3\r\n").unwrap();
    } else {
        let path = bin.join("kargo-greet");
        std::fs::write(&path, "#!/bin/sh\necho greet $1 $KARGO_PKG_NAME\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    p.kargo().args(["greet", "world"]).assert().code(3).stdout(predicate::str::contains("greet world ext"));
    p.kargo().arg("plugins").assert().success().stdout(predicate::str::contains("greet"));
    p.kargo().arg("definitely-not-a-command").assert().failure().stderr(predicate::str::contains("no such command"));
}

#[test]
fn clean_removes_target_dir() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("cleaner", ""));
    p.file("target/debug/jvm/x.txt", "x");
    p.file("target/release/jvm/y.txt", "y");
    p.kargo().args(["clean", "--release"]).assert().success();
    assert!(!p.path().join("target/release").exists());
    assert!(p.path().join("target/debug").exists());
    p.kargo().arg("clean").assert().success();
    assert!(!p.path().join("target").exists());
}

#[test]
fn manifest_path_flag() {
    let p = Project::new();
    p.file("nested/kargo.toml", &manifest("nested", ""));
    p.kargo()
        .args(["info", "--manifest-path"])
        .arg(p.path().join("nested/kargo.toml"))
        .assert()
        .success()
        .stdout(predicate::str::contains("nested v0.1.0"));
}

#[test]
fn android_requires_manifest_file() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("droid", "[target.android]\nnamespace = \"com.example.droid\"\n"));
    p.kargo().args(["build", "--target", "android"]).assert().failure().stderr(predicate::str::contains("AndroidManifest.xml"));
}

#[test]
fn disabled_target_is_rejected() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("jvmonly", ""));
    p.kargo().args(["build", "--target", "native"]).assert().failure().stderr(predicate::str::contains("not enabled"));
}

#[test]
fn check_runs_pre_build_hooks_then_checks_android() {
    let p = Project::new();
    let hook = if cfg!(windows) { "echo gen>> log.txt" } else { "echo gen >> log.txt" };
    p.file(
        "kargo.toml",
        &manifest(
            "droid",
            &format!("[target.android]\nnamespace = \"com.example.droid\"\n[tasks]\ngen = {{ run = '{hook}', hooks = [\"pre-build\"] }}\n"),
        ),
    );
    // Android is checked through the generated Gradle project, which needs the manifest file.
    p.kargo().arg("check").assert().failure().code(101).stderr(predicate::str::contains("AndroidManifest.xml"));
    assert_eq!(p.read("log.txt").trim(), "gen");
    p.kargo().args(["c", "--tests", "-t", "native"]).assert().failure().stderr(predicate::str::contains("not enabled"));
}

fn fake_jdk(home: &std::path::Path, release: &str, major: u32) {
    let dir = home.join("toolchains/jdk").join(release);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("kargo-jdk.json"), format!(r#"{{"release": "{release}", "major": {major}, "vendor": "temurin", "home": ""}}"#))
        .unwrap();
}

#[test]
fn toolchain_list_and_remove() {
    let p = Project::new();
    fake_jdk(p.home.path(), "jdk-21.0.4+7", 21);
    fake_jdk(p.home.path(), "jdk-17.0.12+7", 17);
    let konanc = p.home.path().join("toolchains/kotlin-native/2.4.21/bin").join(if cfg!(windows) { "konanc.bat" } else { "konanc" });
    std::fs::create_dir_all(konanc.parent().unwrap()).unwrap();
    std::fs::write(&konanc, "").unwrap();
    p.file("kargo.toml", &manifest("tc", "[toolchain]\njdk = \"21\"\n"));

    p.kargo()
        .args(["toolchain", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jdk-17.0.12+7"))
        .stdout(predicate::str::is_match(r"jdk-21\.0\.4\+7.*used by this project").unwrap())
        .stdout(predicate::str::contains("2.4.21"));

    p.kargo().args(["toolchain", "remove", "jdk", "17"]).assert().success().stderr(predicate::str::contains("Removed JDK jdk-17.0.12+7"));
    p.kargo().args(["toolchain", "remove", "native", "2.4.21"]).assert().success();
    p.kargo().args(["toolchain", "remove", "native", "2.4.21"]).assert().failure().stderr(predicate::str::contains("not installed"));
    let out = String::from_utf8(p.kargo().args(["toolchain", "ls"]).assert().success().get_output().stdout.clone()).unwrap();
    assert!(!out.contains("jdk-17.0.12+7") && !out.contains("2.4.21"), "{out}");
}

#[test]
fn old_kotlin_on_new_jdk_fails_early() {
    let p = Project::new();
    fake_jdk(p.home.path(), "jdk-25.0.1+8", 25);
    p.file(
        "src/Main.kt",
        "fun main() {}
",
    );
    p.file(
        "kargo.toml",
        &manifest(
            "old",
            "[kotlin]
version = \"2.1.0\"

[toolchain]
jdk = \"25\"
",
        ),
    );
    p.kargo()
        .args(["build", "--offline"])
        .assert()
        .code(101)
        .stderr(predicate::str::contains("Kotlin 2.1.0 cannot run on JDK 25"))
        .stderr(predicate::str::contains("[toolchain] jdk = \"21\""))
        .stderr(predicate::str::contains("Compiling").not());
}

#[test]
fn toolchain_install_requires_network_or_cache() {
    let p = Project::new();
    p.kargo().args(["toolchain", "install", "jdk", "21", "--offline"]).assert().failure().stderr(predicate::str::contains("offline"));
    p.kargo().args(["toolchain", "install"]).assert().failure().stderr(predicate::str::contains("not in a Kargo project"));
    p.kargo().args(["toolchain", "install", "jdk", "seven"]).assert().failure().stderr(predicate::str::contains("invalid JDK"));
    // A pinned JDK that is not installed cannot be provisioned offline.
    p.file("kargo.toml", &manifest("tc", "[toolchain]\njdk = \"21\"\n"));
    p.kargo().args(["toolchain", "which", "--offline"]).assert().failure().stderr(predicate::str::contains("offline"));
}
