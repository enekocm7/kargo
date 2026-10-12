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
fn new_inside_a_git_repository_does_not_nest_one() {
    let Some(git) = common::which("git") else { return };
    let p = Project::new();
    let status = std::process::Command::new(git).args(["init", "--quiet"]).current_dir(p.path()).status().unwrap();
    assert!(status.success());
    // Run from a subdirectory so the relative path never names the repository root.
    let sub = p.path().join("examples");
    std::fs::create_dir_all(&sub).unwrap();
    p.kargo().current_dir(&sub).args(["new", "todo-cli"]).assert().success();
    let root = sub.join("todo-cli");
    assert!(root.join("kargo.toml").is_file());
    assert!(root.join(".gitignore").is_file());
    assert!(!root.join(".git").exists(), "nested repository created inside an existing one");
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
fn info_as_json() {
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "infoapp",
            "[frameworks]\ncoroutines = true\n[target.jvm]\n[dev-dependencies]\n\"g:t\" = \"1\"\n[[bin]]\nname = \"tool\"\nmain = \"t.ToolKt\"\n",
        ),
    );
    p.file("examples/demo.kt", "fun main() {}\n");
    let out = p.kargo().args(["info", "--format", "json"]).assert().success().get_output().stdout.clone();
    let info: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(info["package"]["name"], "infoapp");
    assert_eq!(info["package"]["version"], "0.1.0");
    assert_eq!(info["targets"], serde_json::json!(["jvm"]));
    assert_eq!(info["frameworks"], serde_json::json!(["coroutines"]));
    assert_eq!(info["dev-dependencies"][0], serde_json::json!({"name": "g:t", "module": "g:t", "version": "1", "scope": "compile"}));
    assert!(info["target-dir"].as_str().unwrap().ends_with("target"), "{info}");
    assert_eq!(info["bins"], serde_json::json!([{"name": "tool", "main": "t.ToolKt", "entry-point": null, "path": null}]));
    assert_eq!(info["examples"], serde_json::json!(["demo"]));
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

    p.kargo().args(["add", "com.acme:widget:1.0.0", "--target", "jvm", "--scope", "processor"]).assert().success();
    assert!(p.read("kargo.toml").contains(r#""com.acme:widget" = { version = "1.0.0", scope = "processor" }"#), "{}", p.read("kargo.toml"));

    p.kargo().args(["add", "com.acme:widget:1.2.0", "--scope", "ksp"]).assert().success();
    assert!(p.read("kargo.toml").contains(r#""com.acme:widget" = { version = "1.2.0", scope = "ksp" }"#), "{}", p.read("kargo.toml"));

    p.kargo().args(["add", "com.acme:missing:1.0"]).assert().failure().stderr(predicate::str::contains("not found"));
    p.kargo().args(["add", "--framework", "coroutines"]).assert().success();
    assert!(p.read("kargo.toml").contains("coroutines = true"));

    p.kargo().args(["remove", "com.acme:widget"]).assert().success();
    p.kargo().args(["remove", "com.acme:widget", "--dev"]).assert().success();
    p.kargo().args(["remove", "com.acme:widget"]).assert().failure().stderr(predicate::str::contains("not declared"));
}

#[test]
fn add_prefers_plain_releases_and_remove_targets_a_table() {
    let repo = LocalRepo::new();
    repo.publish("org.acme:datetime", "0.7.1", &[]).publish("org.acme:datetime", "0.8.0", &[]).publish(
        "org.acme:datetime",
        "0.8.0-0.6.x-compat",
        &[],
    );
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", &format!("\n[repositories]\nlocal = \"{}\"\n", repo.url())));

    p.kargo()
        .args(["add", "org.acme:datetime", "-t", "wasm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("org.acme:datetime:0.8.0 to [target.wasm.dependencies]"));
    assert!(p.read("kargo.toml").contains("\"org.acme:datetime\" = \"0.8.0\""));

    p.kargo().args(["remove", "org.acme:datetime"]).assert().failure().stderr(predicate::str::contains("[dependencies]"));
    p.kargo()
        .args(["remove", "org.acme:datetime", "-t", "wasm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Removing org.acme:datetime from [target.wasm.dependencies]"));
    assert!(!p.read("kargo.toml").contains("org.acme:datetime"));
}

#[test]
fn outdated_and_upgrade_edit_versions_in_kargo_toml() {
    let repo = LocalRepo::new();
    for v in ["1.0.0", "1.4.0", "2.0.0"] {
        repo.publish("com.acme:widget", v, &[]);
    }
    repo.publish("com.acme:fresh", "1.0", &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "app",
            &format!(
                "\n[dependencies]\n\"com.acme:widget\" = \"1.0.0\"  # the UI kit\n\"com.acme:fresh\" = \"1.0\"\n\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );

    let out = p.kargo().args(["outdated"]).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("name             current  compatible  latest  kind"), "{text}");
    assert!(text.contains("com.acme:widget  1.0.0    1.4.0       2.0.0   dependencies"), "{text}");
    assert!(!text.contains("com.acme:fresh"), "{text}");

    p.kargo()
        .args(["upgrade", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Upgrading com.acme:widget v1.0.0 -> v1.4.0 in [dependencies]"))
        .stderr(predicate::str::contains("dry run"));
    assert!(p.read("kargo.toml").contains("\"com.acme:widget\" = \"1.0.0\""));

    p.kargo().args(["upgrade"]).assert().success().stderr(predicate::str::contains("--incompatible"));
    assert!(p.read("kargo.toml").contains("\"com.acme:widget\" = \"1.4.0\"  # the UI kit"));
    p.kargo().args(["upgrade", "--incompatible", "com.acme:widget"]).assert().success();
    assert!(p.read("kargo.toml").contains("\"com.acme:widget\" = \"2.0.0\"  # the UI kit"));

    p.kargo().args(["outdated"]).assert().success().stdout("").stderr(predicate::str::contains("newest version"));
    p.kargo().args(["upgrade", "com.acme:nope"]).assert().failure().stderr(predicate::str::contains("not declared"));
}

#[test]
fn update_selected_modules_precisely_or_as_a_dry_run() {
    let repo = LocalRepo::new();
    repo.publish("g:lib", "1.0", &[("g:util", "[1.0,2.0)")])
        .publish("g:other", "1.0", &[("g:dep", "[1.0,2.0)")])
        .publish("g:util", "1.0", &[])
        .publish("g:dep", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", kargo_ops::builtins::JUNIT_VERSION, &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "app",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n\"g:other\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );
    let locked = |p: &Project, module: &str, version: &str| {
        p.read("kargo.lock").contains(&format!("module = \"{module}\"\nversion = \"{version}\""))
    };

    p.kargo().args(["update", "--offline"]).assert().success().stderr(predicate::str::contains("Adding g:util v1.0"));
    assert!(locked(&p, "g:util", "1.0") && locked(&p, "g:dep", "1.0"));
    repo.publish("g:util", "1.1", &[]).publish("g:dep", "1.1", &[]);

    p.kargo()
        .args(["update", "g:util", "--dry-run", "--offline"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Updating g:util v1.0 -> v1.1"))
        .stderr(predicate::str::contains("dry run"));
    assert!(locked(&p, "g:util", "1.0"), "dry run leaves kargo.lock alone");

    p.kargo()
        .args(["update", "g:util", "--offline"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Updating g:util v1.0 -> v1.1"))
        .stderr(predicate::str::contains("g:dep").not());
    assert!(locked(&p, "g:util", "1.1") && locked(&p, "g:dep", "1.0"), "only g:util moves");

    p.kargo().args(["update", "g:util", "--precise", "1.0", "--offline"]).assert().success();
    assert!(locked(&p, "g:util", "1.0"));
    // The precise version survives later builds because the manifest did not change.
    p.kargo().args(["tree", "--locked", "--offline"]).assert().success().stdout(predicate::str::contains("g:util:1.0"));

    p.kargo().args(["update", "g:nope", "--offline"]).assert().failure().stderr(predicate::str::contains("not locked"));
    p.kargo().args(["update", "g:util", "g:dep", "--precise", "1.0"]).assert().failure().stderr(predicate::str::contains("exactly one"));

    p.kargo().args(["update", "--offline"]).assert().success();
    assert!(locked(&p, "g:util", "1.1") && locked(&p, "g:dep", "1.1"));
    p.kargo().args(["update", "--offline"]).assert().success().stderr(predicate::str::contains("up to date"));
}

#[test]
fn update_locks_processors_apart_from_the_classpaths() {
    let repo = LocalRepo::new();
    repo.publish("g:lib", "1.0", &[])
        .publish("g:codegen", "1.0", &[("g:poet", "1.0")])
        .publish("g:poet", "1.0", &[])
        .publish("g:test-codegen", "1.0", &[])
        .publish("g:apt", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", kargo_ops::builtins::JUNIT_VERSION, &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "app",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n\"g:codegen\" = {{ version = \"1.0\", scope = \"ksp\" }}\n\
                 \"g:apt\" = {{ version = \"1.0\", scope = \"processor\" }}\n\
                 [dev-dependencies]\n\"g:test-codegen\" = {{ version = \"1.0\", scope = \"ksp\" }}\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );
    p.kargo().args(["update", "--offline"]).assert().success();
    let lock = kargo_ops::lockfile::Lockfile::load(&p.path().join("kargo.lock")).unwrap();
    let modules = |key: &str| -> Vec<String> { lock.resolutions[key].packages.iter().map(|p| p.module.clone()).collect() };
    assert_eq!(modules("jvm/ksp"), ["g:codegen", "g:poet"]);
    assert_eq!(modules("jvm/test-ksp"), ["g:codegen", "g:test-codegen", "g:poet"]);
    assert_eq!(modules("jvm/annotation-processors"), ["g:apt"]);
    for key in ["jvm/compile", "jvm/runtime", "jvm/test"] {
        let m = modules(key);
        assert!(m.contains(&"g:lib".to_owned()), "{key}: {m:?}");
        assert!(!m.iter().any(|m| m.contains("codegen") || m == "g:apt"), "processors stay off {key}: {m:?}");
    }
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

    let out = p.kargo().args(["tree", "--offline", "--format", "json"]).assert().success().get_output().stdout.clone();
    let tree: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(tree["package"]["name"], "treeapp");
    assert_eq!(tree["configuration"], "runtime");
    let jvm = &tree["trees"][0];
    assert_eq!(jvm["target"], "jvm");
    assert!(jvm["roots"].as_array().unwrap().contains(&serde_json::json!("g:lib")), "{jvm}");
    let lib = jvm["modules"].as_array().unwrap().iter().find(|m| m["id"] == "g:lib").unwrap();
    assert_eq!(lib["dependencies"][0], serde_json::json!({"id": "g:util", "requested": "1.0", "version": "2.0"}));
    assert!(lib["files"][0].as_str().unwrap().ends_with("lib-1.0.jar"), "{lib}");
    p.kargo().args(["tree", "--format", "json", "--depth", "1"]).assert().failure().stderr(predicate::str::contains("cannot be used with"));

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
fn help_and_task_list_show_project_tasks() {
    let p = Project::new();
    p.kargo().arg("--help").assert().success().stdout(predicate::str::contains("Tasks").not());
    p.file("kargo.toml", &manifest("tasky", "[tasks]\ngen = { run = \"echo gen\", description = \"generate sources\" }\n"));
    for args in [&["--help"][..], &["-h"], &["help"], &["-q", "--help"]] {
        p.kargo().args(args).assert().success().stdout(predicate::str::contains("Tasks").and(predicate::str::contains("generate sources")));
    }
    p.kargo().args(["build", "--help"]).assert().success().stdout(predicate::str::contains("generate sources").not());
    p.kargo().args(["task", "--list"]).assert().success().stdout(predicate::str::contains("gen  generate sources"));
}

#[test]
fn completions() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        Project::new().kargo().args(["completions", shell]).assert().success().stdout(predicate::str::contains("COMPLETE"));
    }
    Project::new().kargo().args(["completions", "tcsh"]).assert().failure();

    // What the scripts ask kargo for on <Tab> (fish's protocol: one `value\thelp` per line).
    let p = Project::new();
    let complete = |words: &[&str]| {
        let out = p.kargo().env("COMPLETE", "fish").arg("--").arg("kargo").args(words).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    let values = |out: &str| out.lines().map(|l| l.split('\t').next().unwrap().to_string()).collect::<Vec<_>>();

    // Outside a project: built-in commands only.
    assert!(values(&complete(&["bu"])).contains(&"build".to_string()));
    assert!(!complete(&[""]).contains("generate sources"));

    p.file(
        "kargo.toml",
        &manifest("tasky", "[tasks]\ngen = { run = \"echo gen\", description = \"generate sources\" }\nlint = \"echo lint\"\n"),
    );
    assert!(complete(&["g"]).contains("gen\tgenerate sources"));
    assert_eq!(values(&complete(&["task", "l"])), ["lint"]);
    assert_eq!(values(&complete(&["build", "-t", ""])), ["jvm", "wasm", "native", "android"]);
    assert!(values(&complete(&["toolchain", ""])).contains(&"install".to_string()));
    assert!(values(&complete(&["add", "--framework", "spr"])).contains(&"spring-boot".to_string()));
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
    // Only the build-directory lock survives a full clean.
    let left: Vec<_> = std::fs::read_dir(p.path().join("target")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, [".kargo-lock"]);
    // Cleaning a project that was never built creates nothing.
    std::fs::remove_dir_all(p.path().join("target")).unwrap();
    p.kargo().arg("clean").assert().success();
    assert!(!p.path().join("target").exists());
}

#[test]
fn build_directory_lock_serializes_kargo_processes() {
    use std::io::Read;
    use std::process::Stdio;

    let p = Project::new();
    p.file("kargo.toml", &manifest("locked", ""));
    p.file("target/debug/jvm/x.txt", "x");
    let lock_path = p.path().join("target/.kargo-lock");
    let held = std::fs::File::create(&lock_path).unwrap();
    held.lock().unwrap();

    // A hook's nested `kargo` is told the lock is its parent's and does not wait.
    p.kargo().arg("clean").env("KARGO_BUILD_LOCK", &lock_path).assert().success();
    p.file("target/debug/jvm/x.txt", "x");

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("kargo"))
        .arg("clean")
        .current_dir(p.path())
        .env("KARGO_HOME", p.home.path())
        .env_remove("KARGO_TARGET_DIR")
        .env_remove("KARGO_BUILD_LOCK")
        .env("NO_COLOR", "1")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stderr.read_to_string(&mut out).unwrap();
        out
    });
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(child.try_wait().unwrap().is_none(), "kargo clean finished while another process held the lock");
    assert!(p.path().join("target/debug/jvm/x.txt").exists());

    drop(held);
    assert!(child.wait().unwrap().success());
    let stderr = reader.join().unwrap();
    assert!(stderr.contains("Blocking waiting for file lock on build directory"), "{stderr}");
    assert!(!p.path().join("target/debug").exists());
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
fn doc_reports_what_it_cannot_document() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("nodocs", "[target.jvm]\n"));
    p.kargo().arg("doc").assert().failure().code(101).stderr(predicate::str::contains("no Kotlin or Java sources to document"));
    p.kargo().args(["doc", "--target", "native"]).assert().failure().stderr(predicate::str::contains("not enabled"));
}

#[test]
fn doc_android_uses_the_generated_gradle_project() {
    // Android's classpath comes from Gradle, so the usual Android checks apply first.
    let p = Project::new();
    p.file("kargo.toml", &manifest("droid", "[target.android]\nnamespace = \"com.example.droid\"\n"));
    p.file("src/Main.kt", "fun main() {}\n");
    p.kargo().arg("doc").assert().failure().code(101).stderr(predicate::str::contains("AndroidManifest.xml"));
}

#[test]
fn doc_section_is_validated() {
    let p = Project::new();
    p.file("src/Lib.kt", "fun f() = 1\n");
    p.file("kargo.toml", &manifest("docs", "[doc]\nsource-link = \"github.com/me/docs\"\n"));
    p.kargo().arg("doc").assert().failure().stderr(predicate::str::contains("`source-link` must be an http(s):// URL"));
    p.file("kargo.toml", &manifest("docs", "[doc]\nformat = \"html\"\n"));
    p.kargo().arg("doc").assert().failure().stderr(predicate::str::contains("unknown field `format`"));
}

#[test]
fn fmt_and_lint_need_kotlin_sources() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("empty", ""));
    // `target/` is never formatted or linted.
    p.file("target/debug/Generated.kt", "fun   generated( ) = 1\n");
    for cmd in ["fmt", "lint"] {
        p.kargo().arg(cmd).assert().failure().code(101).stderr(predicate::str::contains("no Kotlin sources found"));
    }
}

#[test]
fn fmt_and_lint_sections_are_validated() {
    let p = Project::new();
    p.file("src/Main.kt", "fun main() {}\n");
    p.file("kargo.toml", &manifest("style", "[fmt]\nexclude = [\"!src/gen/**\"]\n"));
    p.kargo().args(["fmt", "--check"]).assert().failure().stderr(predicate::str::contains("[fmt] `exclude` entries must be globs"));
    p.file("kargo.toml", &manifest("style", "[lint]\nconfig = [\"config/detekt.yml\"]\n"));
    p.kargo()
        .arg("lint")
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("[lint] config file `config/detekt.yml` does not exist"));
    p.file("kargo.toml", &manifest("style", "[lint]\nbaseline = \"baseline.xml\"\n"));
    p.kargo().arg("lint").assert().failure().stderr(predicate::str::contains("kargo lint --create-baseline"));
    // A task named like the command is reachable through `kargo task`.
    p.file("kargo.toml", &manifest("style", "[tasks]\nlint = \"echo custom\"\n[lint]\nbaseline = \"baseline.xml\"\n"));
    p.kargo().arg("lint").assert().failure().stderr(predicate::str::contains("run the `lint` task with `kargo task lint`"));
    p.file("kargo.toml", &manifest("style", "[lint]\nmax-issues = 3\n"));
    p.kargo().arg("lint").assert().failure().stderr(predicate::str::contains("unknown field `max-issues`"));
}

#[test]
fn missing_cinterop_def_is_reported_before_compiling() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("cdemo", "[target.native.cinterop.sqlite]\npackage = \"sqlite3\"\n"));
    p.file("src/Main.kt", "fun main() {}\n");
    // The default location is interop/<name>.def; nothing is downloaded before the check.
    for cmd in ["build", "check"] {
        p.kargo()
            .args([cmd, "--target", "native"])
            .assert()
            .failure()
            .code(101)
            .stderr(predicate::str::contains("sqlite.def of [target.native.cinterop.sqlite] does not exist"));
    }
    // An explicit `def` must exist even when `headers` are listed.
    p.file("kargo.toml", &manifest("cdemo", "[target.native.cinterop.sqlite]\ndef = \"c/sqlite.def\"\nheaders = [\"sqlite3.h\"]\n"));
    p.kargo()
        .args(["build", "--target", "native"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sqlite.def of [target.native.cinterop.sqlite] does not exist"));
    p.file("kargo.toml", &manifest("cdemo", "[target.native.cinterop.sqlite]\nlibs = [\"x\"]\n"));
    p.kargo().args(["build", "--target", "native"]).assert().failure().stderr(predicate::str::contains("unknown field `libs`"));
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
fn invalid_jvm_target_is_rejected_before_building() {
    let p = Project::new();
    p.file("src/Main.kt", "fun main() {}\n");
    for bad in ["abc", "7", "1.9", "17.0"] {
        p.file("kargo.toml", &manifest("jt", &format!("[target.jvm]\njvm-target = \"{bad}\"\n")));
        p.kargo()
            .args(["build", "--offline"])
            .assert()
            .code(101)
            .stderr(predicate::str::contains(format!("invalid [target.jvm] jvm-target `{bad}`")))
            .stderr(predicate::str::contains("`8` or `1.8`"))
            .stderr(predicate::str::contains("Compiling").not());
    }
    // `8` is Java 8, which Kotlin spells `1.8`; either spelling loads.
    for ok in ["8", "1.8", "17"] {
        p.file("kargo.toml", &manifest("jt", &format!("[target.jvm]\njvm-target = \"{ok}\"\n")));
        p.kargo().arg("info").assert().success();
    }
}

#[test]
fn jvm_target_newer_than_the_system_jdk_fails_early() {
    let p = Project::new();
    // A system JDK Kargo identifies from the `release` file next to `bin/java`, without starting it.
    let jdk = p.home.path().join("system-jdk");
    std::fs::create_dir_all(jdk.join("bin")).unwrap();
    std::fs::write(jdk.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }), "").unwrap();
    std::fs::write(jdk.join("release"), "JAVA_VERSION=\"21.0.4\"\n").unwrap();
    p.file("src/Main.kt", "fun main() {}\n");
    p.file("kargo.toml", &manifest("jt", "[target.jvm]\njvm-target = \"26\"\n"));
    for command in ["build", "check", "test", "run"] {
        p.kargo()
            .args([command, "--offline"])
            .env("JAVA_HOME", &jdk)
            .assert()
            .code(101)
            .stderr(predicate::str::contains("jvm-target 26 is newer than JDK 21"))
            .stderr(predicate::str::contains("[toolchain] jdk = \"26\""))
            .stderr(predicate::str::contains("Compiling").not());
    }
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

#[test]
fn daemon_status_and_stop() {
    let p = Project::new();
    p.kargo().args(["daemon", "status"]).assert().success().stdout(predicate::str::contains("No compiler daemons are running."));
    // A leftover state file of a daemon that is gone (e.g. restored from a CI cache) is not a running daemon.
    let dir = p.home.path().join("daemon").join("0123456789abcdef");
    std::fs::create_dir_all(&dir).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    std::fs::write(dir.join("info"), format!("{port} token 1\n")).unwrap();
    p.kargo().args(["daemon", "status"]).assert().success().stdout(predicate::str::contains("No compiler daemons are running."));
    p.kargo().args(["daemon", "stop"]).assert().success().stderr(predicate::str::contains("Stopped 0 compiler daemons"));
}

#[test]
fn workspace_members_share_one_lockfile_and_resolve_path_dependencies() {
    let repo = LocalRepo::new();
    repo.publish("g:util", "1.0", &[]).publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[]);
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n\n[workspace.dependencies]\n\"g:util\" = \"1.0\"\n")
        .file("app/kargo.toml", &manifest("app", "[dependencies]\nlib = { path = \"../lib\" }\n"))
        // Only `lib` knows the repository; `app` still resolves `lib`'s dependencies from it.
        .file(
            "lib/kargo.toml",
            &manifest("lib", &format!("[dependencies]\n\"g:util\" = {{ workspace = true }}\n[repositories]\nlocal = \"{}\"\n", repo.url())),
        );

    p.kargo().arg("info").assert().success().stdout(predicate::str::contains("members:      app v0.1.0, lib v0.1.0"));
    p.kargo().arg("info").current_dir(p.path().join("app")).assert().success().stdout(predicate::str::contains("path deps:    lib ("));

    let out = p.kargo().args(["tree", "-p", "app", "--offline"]).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("app v0.1.0 (jvm)"), "{text}");
    assert!(text.contains("├── lib v0.1.0 ("), "{text}");
    assert!(text.contains("g:util:1.0"), "{text}");

    // One kargo.lock at the root, with entries per member.
    let lock = p.read("kargo.lock");
    assert!(lock.contains("[resolution.\"app/jvm/runtime\"]"), "{lock}");
    assert!(!p.path().join("app/kargo.lock").exists());

    // Inside a member, commands apply to that member.
    let out = p.kargo().args(["tree", "--offline"]).current_dir(p.path().join("lib")).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("lib v0.1.0 (jvm)") && !text.contains("app v0.1.0"), "{text}");
    assert!(p.read("kargo.lock").contains("[resolution.\"lib/jvm/runtime\"]"));

    p.kargo().args(["tree", "--workspace", "--offline", "--locked"]).assert().success();
    p.kargo().arg("run").assert().failure().code(101).stderr(predicate::str::contains("pick one with `-p <name>` (app, lib)"));
    p.kargo().args(["build", "-p", "nope"]).assert().failure().stderr(predicate::str::contains("its members are: app, lib"));
    p.kargo().args(["add", "g:util:1.0", "--no-verify"]).assert().failure().stderr(predicate::str::contains("-p <name>"));
}

#[test]
fn add_path_dependency_writes_a_relative_path() {
    let p = Project::new();
    p.file("calc/kargo.toml", &manifest("calc", "")).file("mathlib/kargo.toml", &manifest("mathlib", ""));
    p.kargo()
        .args(["add", "--path", "../mathlib"])
        .current_dir(p.path().join("calc"))
        .assert()
        .success()
        .stderr(predicate::str::contains("Adding mathlib (path ../mathlib) to [dependencies]"));
    assert!(p.read("calc/kargo.toml").contains("mathlib = { path = \"../mathlib\" }"), "{}", p.read("calc/kargo.toml"));

    // A cycle is refused and leaves kargo.toml untouched.
    let before = p.read("mathlib/kargo.toml");
    p.kargo().args(["add", "--path", "../calc", "--dev", "--scope", "runtime"]).current_dir(p.path().join("mathlib")).assert().success();
    p.file("mathlib/kargo.toml", &before);
    p.kargo()
        .args(["add", "--path", "../calc"])
        .current_dir(p.path().join("mathlib"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("cycle: mathlib -> calc -> mathlib"));
    assert_eq!(p.read("mathlib/kargo.toml"), before);

    p.kargo()
        .args(["add", "--path", "../nowhere"])
        .current_dir(p.path().join("calc"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no kargo.toml"));
    p.kargo().args(["remove", "mathlib"]).current_dir(p.path().join("calc")).assert().success();
    assert!(!p.read("calc/kargo.toml").contains("mathlib"));
}

#[test]
fn path_dependency_must_support_the_target() {
    let p = Project::new();
    p.file("calc/kargo.toml", &manifest("calc", "[dependencies]\nmathlib = { path = \"../mathlib\" }\n[target.jvm]\n[target.wasm]\n"))
        .file("mathlib/kargo.toml", &manifest("mathlib", ""));
    p.kargo()
        .args(["tree", "-t", "wasm", "--offline"])
        .current_dir(p.path().join("calc"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("`calc` depends on the path dependency `mathlib`, which does not enable the wasm target"));
}

/// A JVM + Android project that fails both targets offline: the Kotlin compiler is not
/// cached and there is no `android/AndroidManifest.xml`.
fn two_failing_targets() -> Project {
    let p = Project::new();
    p.file("kargo.toml", &manifest("twin", "[target.jvm]\n\n[target.android]\nnamespace = \"com.example.twin\"\n"));
    p.file("src/Main.kt", "fun main() {}\n");
    p.file("tests/MainTest.kt", "class MainTest\n");
    p
}

#[test]
fn build_keep_going_reports_every_failed_target() {
    let p = two_failing_targets();
    // By default the first failure is the error.
    p.kargo()
        .args(["--offline", "build"])
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("could not build").not().and(predicate::str::contains("offline mode")));
    for cmd in ["build", "check"] {
        p.kargo()
            .args(["--offline", cmd, "--keep-going"])
            .assert()
            .failure()
            .code(101)
            .stderr(predicate::str::contains(format!("could not {cmd} the jvm target")))
            .stderr(predicate::str::contains(format!("could not {cmd} the android target: Android builds need")))
            .stderr(predicate::str::contains(format!("could not {cmd} 2 of 2 targets (jvm, android)")));
    }
}

#[test]
fn json_message_format_ends_with_build_finished() {
    let p = two_failing_targets();
    for cmd in ["build", "check"] {
        let out = p.kargo().args(["--offline", cmd, "--message-format", "json"]).assert().failure().code(101).get_output().stdout.clone();
        let text = String::from_utf8(out).unwrap();
        // stdout holds only JSON messages.
        let messages: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(messages.last().unwrap(), &serde_json::json!({"reason": "build-finished", "success": false}), "{text}");
    }
}

#[test]
fn test_fail_fast_skips_the_remaining_targets() {
    let p = two_failing_targets();
    p.kargo()
        .args(["--offline", "test"])
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("skipped the android target after a failure; pass `--no-fail-fast`"))
        .stderr(predicate::str::contains("Running Android").not());
    let table =
        || predicate::str::contains("test results by target:\n    jvm      error").and(predicate::str::contains("    android  error"));
    p.kargo()
        .args(["--offline", "test", "--no-fail-fast"])
        .assert()
        .failure()
        .code(101)
        .stdout(table())
        .stderr(predicate::str::contains("could not test the android target: Android builds need"));
    // `[test] fail-fast = false` makes it the default.
    p.file("kargo.toml", &(p.read("kargo.toml") + "\n[test]\nfail-fast = false\n"));
    p.kargo().args(["--offline", "test"]).assert().failure().code(101).stdout(table());
}

#[test]
fn run_selects_bins_and_examples_before_compiling() {
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "tools",
            "[[bin]]\nname = \"migrate\"\nmain = \"com.example.MigrateKt\"\n\n[[bin]]\nname = \"seed\"\npath = \"src/Seed.kt\"\n",
        ),
    );
    p.file("src/Main.kt", "package com.example\n\nfun main() {}\n");
    // Each mistake is reported before any toolchain is downloaded, so this stays offline.
    p.kargo()
        .args(["run", "--bin", "nope"])
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("no bin named `nope` in kargo.toml; available bins: migrate, seed"));
    p.kargo()
        .args(["run", "--example", "demo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no example named `demo`: add examples/demo.kt"));
    p.file("examples/hello.kt", "fun main() {}\n");
    p.kargo().args(["run", "--example", "demo"]).assert().failure().stderr(predicate::str::contains("available examples: hello"));
    p.kargo().args(["package", "--bin", "nope"]).assert().failure().stderr(predicate::str::contains("no bin named `nope`"));
    p.kargo()
        .args(["package", "--bin", "seed"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[[bin]] `seed`").and(predicate::str::contains("Seed.kt does not exist")));
    p.kargo()
        .args(["run", "--bin", "migrate", "--example", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn run_rejects_bins_on_targets_without_them() {
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest("web", "[target.wasm]\n[target.native]\n\n[[bin]]\nname = \"jvm-only\"\nmain = \"com.example.ToolKt\"\n"),
    );
    p.kargo()
        .args(["run", "--target", "wasm", "--main", "x.MainKt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`kargo run --main` only works for the jvm and native targets, not wasm"));
    p.kargo()
        .args(["run", "--target", "native", "--bin", "jvm-only"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[[bin]] `jvm-only` has no Kotlin/Native entry point"));
}

#[test]
fn invalid_bin_sections_are_rejected() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("tools", "[[bin]]\nname = \"x\"\n"));
    p.kargo().arg("build").assert().failure().code(101).stderr(predicate::str::contains("[[bin]] `x` needs an entry point"));
}
