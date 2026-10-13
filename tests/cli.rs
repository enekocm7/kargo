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
    assert!(std::fs::read_to_string(root.join(".gitignore")).unwrap().contains("/workspace.json\n"), "kargo lsp's generated files");
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
    p.kargo()
        .args(["init", "--no-vcs"])
        .arg(&dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already contains").and(predicate::str::contains("kargo target add")));
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
fn classifier_dependencies_keep_both_jars() {
    // Native libraries are declared twice: the module, and its classifier jar. Both belong on the
    // runtime classpath and in kargo.lock, and neither may replace the other.
    let repo = LocalRepo::new();
    repo.publish("g:lib", "1.0", &[("g:util", "1.0")])
        .publish_classifier("g:lib", "1.0", "natives-windows")
        .publish("g:util", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[]);
    let p = Project::new();
    // The classifier dependency's key sorts before the module's, and it does not follow dependencies.
    p.file(
        "kargo.toml",
        &manifest(
            "natives",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n\
                 a-natives = {{ coords = \"g:lib:1.0\", classifier = \"natives-windows\", scope = \"runtime\", transitive = false }}\n\
                 [repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );

    let tree = |args: &[&str]| -> serde_json::Value {
        let out =
            p.kargo().arg("tree").args(args).arg("--offline").args(["--format", "json"]).assert().success().get_output().stdout.clone();
        serde_json::from_slice(&out).unwrap()
    };
    let files_of = |tree: &serde_json::Value, id: &str| -> Vec<String> {
        let module =
            tree["trees"][0]["modules"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("{id}: {tree}"));
        let mut files: Vec<String> = module["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().rsplit(['/', '\\']).next().unwrap().to_owned())
            .collect();
        files.sort();
        files
    };

    let runtime = tree(&[]);
    let roots = runtime["trees"][0]["roots"].as_array().unwrap();
    assert_eq!(roots.iter().filter(|r| *r == "g:lib").count(), 1, "one module, not one root per classifier: {roots:?}");
    assert_eq!(files_of(&runtime, "g:lib"), ["lib-1.0-natives-windows.jar", "lib-1.0.jar"]);
    assert_eq!(files_of(&runtime, "g:util"), ["util-1.0.jar"], "the module's own dependencies are kept");
    // The classifier jar is a runtime dependency, so the compile classpath has the main jar only.
    assert_eq!(files_of(&tree(&["--configuration", "compile"]), "g:lib"), ["lib-1.0.jar"]);

    let lock = kargo_ops::lockfile::Lockfile::load(&p.path().join("kargo.lock")).unwrap();
    let locked: Vec<_> = lock.resolutions["jvm/runtime"].packages.iter().filter(|pkg| pkg.module == "g:lib").collect();
    assert_eq!(locked.len(), 1, "{lock:?}");
    let paths: Vec<&str> = locked[0].files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["g/lib/1.0/lib-1.0.jar", "g/lib/1.0/lib-1.0-natives-windows.jar"]);
    assert!(locked[0].files.iter().all(|f| !f.sha256.is_empty()));

    // The lockfile restores to the same classpath.
    p.kargo().args(["tree", "--locked", "--offline"]).assert().success();
    assert_eq!(files_of(&tree(&[]), "g:lib"), ["lib-1.0-natives-windows.jar", "lib-1.0.jar"]);
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
fn timestamped_snapshots_resolve_through_maven_metadata() {
    let repo = LocalRepo::new();
    repo.publish_snapshot("com.acme:lib", "1.0-SNAPSHOT", "20240101.120000-1", &[("com.acme:util", "2.0-SNAPSHOT")])
        .publish_snapshot("com.acme:util", "2.0-SNAPSHOT", "20240101.110000-4", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", kargo_ops::builtins::JUNIT_VERSION, &[]);
    let p = Project::new();
    p.file("kargo.toml", &manifest("snap", &format!("[kotlin]\nversion = \"2.4.21\"\n[repositories]\nlocal = \"{}\"\n", repo.url())));

    // `kargo add` finds a SNAPSHOT that only exists under its timestamped names.
    p.kargo().args(["add", "com.acme:lib:1.0-SNAPSHOT"]).assert().success();
    assert!(p.read("kargo.toml").contains("\"com.acme:lib\" = \"1.0-SNAPSHOT\""));

    p.kargo()
        .args(["tree", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("com.acme:lib:1.0-SNAPSHOT"))
        .stdout(predicate::str::contains("com.acme:util:2.0-SNAPSHOT"));
    let cached = |rel: &str| p.home.path().join("cache/maven").join(rel);
    assert_eq!(
        std::fs::read_to_string(cached("com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar")).unwrap(),
        "jar com.acme:lib 1.0-20240101.120000-1"
    );
    let lock = p.read("kargo.lock");
    assert!(lock.contains("com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar"), "{lock}");
    assert!(lock.contains("com/acme/util/2.0-SNAPSHOT/util-2.0-20240101.110000-4.jar"), "{lock}");

    // A newer build is not picked up while kargo.lock pins the old one.
    repo.publish_snapshot("com.acme:lib", "1.0-SNAPSHOT", "20240102.080000-2", &[("com.acme:util", "2.0-SNAPSHOT")]);
    p.kargo().args(["tree", "--locked", "--offline"]).assert().success();
    assert_eq!(p.read("kargo.lock"), lock);

    // `kargo update` moves to it, and says so.
    p.kargo()
        .args(["update", "--offline"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Updating com.acme:lib v1.0-SNAPSHOT (20240101.120000-1) -> v1.0-SNAPSHOT (20240102.080000-2)"))
        .stderr(predicate::str::contains("com.acme:util").not());
    let lock = p.read("kargo.lock");
    assert!(lock.contains("com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.080000-2.jar"), "{lock}");
    assert!(!lock.contains("lib-1.0-20240101.120000-1.jar"), "{lock}");
    p.kargo().args(["update", "--offline"]).assert().success().stderr(predicate::str::contains("up to date"));
}

#[test]
fn selective_update_keeps_locked_snapshot_builds() {
    let repo = LocalRepo::new();
    repo.publish_snapshot("com.acme:lib", "1.0-SNAPSHOT", "20240101.120000-1", &[])
        .publish("com.acme:other", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", kargo_ops::builtins::JUNIT_VERSION, &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "snap",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"com.acme:lib\" = \"1.0-SNAPSHOT\"\n\"com.acme:other\" = \"[1.0,2.0)\"\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );
    p.kargo().args(["update", "--offline"]).assert().success();

    repo.publish_snapshot("com.acme:lib", "1.0-SNAPSHOT", "20240102.080000-2", &[]).publish("com.acme:other", "1.1", &[]);
    p.kargo()
        .args(["update", "com.acme:other", "--offline"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Updating com.acme:other v1.0 -> v1.1"))
        .stderr(predicate::str::contains("com.acme:lib").not());
    let lock = p.read("kargo.lock");
    assert!(lock.contains("lib-1.0-20240101.120000-1.jar") && !lock.contains("20240102.080000-2"), "{lock}");
    p.kargo().args(["tree", "--locked", "--offline"]).assert().success();

    p.kargo().args(["update", "--offline"]).assert().success().stderr(predicate::str::contains("v1.0-SNAPSHOT (20240102.080000-2)"));
    assert!(p.read("kargo.lock").contains("lib-1.0-20240102.080000-2.jar"));
}

#[test]
fn snapshots_stored_under_their_plain_name_still_resolve() {
    let repo = LocalRepo::new();
    repo.publish("com.acme:lib", "2.0-SNAPSHOT", &[]).publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[]);
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest(
            "plain",
            &format!(
                "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"com.acme:lib\" = \"2.0-SNAPSHOT\"\n[repositories]\nlocal = \"{}\"\n",
                repo.url()
            ),
        ),
    );
    p.kargo().args(["tree", "--offline"]).assert().success();
    assert!(p.read("kargo.lock").contains("com/acme/lib/2.0-SNAPSHOT/lib-2.0-SNAPSHOT.jar"));
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
fn android_rejects_incompatible_gradle_before_running_it() {
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest("droid", "[target.android]\nnamespace = \"com.example.droid\"\nagp-version = \"9.4.1\"\ngradle-version = \"9.1.0\"\n"),
    );
    p.file("android/AndroidManifest.xml", "<manifest/>");
    for cmd in ["build", "check", "fetch"] {
        p.kargo()
            .args([cmd, "--target", "android", "--offline"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Gradle 9.1.0 is too old for Android Gradle Plugin 9.4.1, which needs Gradle 9.6.0 or newer"))
            .stderr(predicate::str::contains("gradle-version = \"9.6.0\""));
    }
    assert!(!p.path().join("target/android").exists(), "nothing is generated for an incompatible pair");
}

#[test]
fn toolchain_manages_gradle() {
    let p = Project::new();
    let gradle = p.home.path().join("toolchains/gradle/9.6.0/bin").join(if cfg!(windows) { "gradle.bat" } else { "gradle" });
    std::fs::create_dir_all(gradle.parent().unwrap()).unwrap();
    std::fs::write(&gradle, "").unwrap();
    p.file("kargo.toml", &manifest("droid", "[target.android]\nnamespace = \"com.example.droid\"\n"));

    p.kargo()
        .args(["toolchain", "list"])
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Gradle \(.*\):\n    9\.6\.0 .*used by this project").unwrap());
    // Installed already, so no download is needed.
    p.kargo()
        .args(["toolchain", "install", "gradle", "--offline"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Installed Gradle 9.6.0"));
    p.kargo().args(["toolchain", "install", "gradle", "9.5.0", "--offline"]).assert().failure().stderr(predicate::str::contains("offline"));
    p.kargo().args(["toolchain", "remove", "gradle", "9.6.0"]).assert().success().stderr(predicate::str::contains("Removed Gradle 9.6.0"));
    p.kargo().args(["toolchain", "remove", "gradle", "9.6.0"]).assert().failure().stderr(predicate::str::contains("not installed"));
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
    p.kargo().arg("lint").assert().failure().stderr(predicate::str::contains(
        "warning: task `lint` is hidden by the built-in `kargo lint` command; run the task with `kargo task lint`",
    ));
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

/// A Kotlin/Native 2.4.21 distribution with a standard library whose `konanc` always fails,
/// which leaves the arguments Kargo passed in the `.konanc.args` file for the test to read.
fn fake_native(home: &std::path::Path) -> std::path::PathBuf {
    let dist = home.join("toolchains/kotlin-native/2.4.21");
    let konanc = dist.join("bin").join(if cfg!(windows) { "konanc.bat" } else { "konanc" });
    std::fs::create_dir_all(konanc.parent().unwrap()).unwrap();
    std::fs::write(&konanc, if cfg!(windows) { "@exit /b 1\r\n" } else { "#!/bin/sh\nexit 1\n" }).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&konanc, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::create_dir_all(dist.join("klib/common/stdlib")).unwrap();
    dist
}

#[test]
fn native_binaries_link_the_precompiled_stdlib() {
    let p = Project::new();
    let dist = fake_native(p.home.path());
    p.file("kargo.toml", &manifest("nat", "\n[target.native]\nplatforms = [\"linux_x64\"]\n"));
    p.file("src/Main.kt", "fun main() {}\n");
    let build = |profile: &[&str], env: &[(&str, &str)]| {
        let mut cmd = p.kargo();
        cmd.arg("build").args(profile).envs(env.iter().copied());
        String::from_utf8(cmd.assert().failure().code(101).get_output().stderr.clone()).unwrap()
    };
    let argfile = |profile: &str| p.read(&format!("target/{profile}/native/.fingerprint/nat-linux_x64-main.konanc.args"));
    let cache_flag = |args: &str| args.lines().any(|l| l.trim_matches('"').starts_with("-Xauto-cache-from=") && l.contains("klib/common"));

    // The first build says that the compiler is about to build the cache.
    let stderr = build(&[], &[]);
    assert!(stderr.contains("building the Kotlin/Native standard library cache for linux_x64"), "{stderr}");
    assert!(cache_flag(&argfile("debug")), "{}", argfile("debug"));

    // Once the compiler has made it, builds stay quiet but still pass the flag.
    std::fs::create_dir_all(dist.join("klib/cache/linux_x64-gSTATIC-system/stdlib-per-file-cache")).unwrap();
    let stderr = build(&[], &[]);
    assert!(!stderr.contains("standard library cache"), "{stderr}");
    assert!(cache_flag(&argfile("debug")));

    // KARGO_NATIVE_CACHE=false and optimized builds don't use it.
    build(&[], &[("KARGO_NATIVE_CACHE", "false")]);
    assert!(!cache_flag(&argfile("debug")));
    build(&["--release"], &[]);
    assert!(!cache_flag(&argfile("release")), "{}", argfile("release"));
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
fn run_serving_options_need_a_wasm_browser_app() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("web", "[target.jvm]\n[target.wasm]\nenvironment = \"js\"\n"));
    let message = "`--port` and `--open` serve a Wasm browser app; they need `-t wasm` with [target.wasm] environment = \"browser\"";
    // The JVM program, and a Wasm program for Node.js, have nothing to serve.
    p.kargo().args(["run", "--port", "9000"]).assert().failure().code(101).stderr(predicate::str::contains(message));
    p.kargo().args(["run", "-t", "wasm", "--open"]).assert().failure().code(101).stderr(predicate::str::contains(message));
}

#[test]
fn run_rejects_wasm_libraries() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("greeter", "[target.wasm]\nenvironment = \"browser\"\nkind = \"library\"\n"));
    p.kargo()
        .args(["run", "-t", "wasm"])
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("kind = \"library\" builds an npm package and has no program to run"));
}

#[test]
fn test_list_says_which_targets_it_cannot_list() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("web", "[target.wasm]\n"));
    p.file("tests/WebTest.kt", "import kotlin.test.*\nclass WebTest { @Test fun works() {} }\n");
    // Nothing is compiled for a target whose tests can't be listed, so this runs offline.
    p.kargo()
        .args(["--offline", "test", "--list", "--format", "json"])
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("`--list` does not list wasm tests yet"));
    p.kargo().args(["test", "--format", "json"]).assert().failure().stderr(predicate::str::contains("--list"));
    p.kargo().args(["test", "--list", "--no-run"]).assert().failure().stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn wasm_target_options_are_validated() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("lib", "[target.wasm]\nenvironment = \"wasi\"\nkind = \"library\"\n"));
    p.kargo()
        .arg("build")
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("kind = \"library\" builds an npm package, which needs environment = \"js\" or \"browser\""));
    p.file("kargo.toml", &manifest("web", "[target.wasm]\nenvironment = \"desktop\"\n"));
    p.kargo().arg("build").assert().failure().code(101).stderr(predicate::str::contains("expected `js`, `browser` or `wasi`"));
}

#[test]
fn run_help_lists_the_serving_options() {
    let p = Project::new();
    p.kargo()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--port <PORT>").and(predicate::str::contains("--open")));
}

#[test]
fn invalid_bin_sections_are_rejected() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("tools", "[[bin]]\nname = \"x\"\n"));
    p.kargo().arg("build").assert().failure().code(101).stderr(predicate::str::contains("[[bin]] `x` needs an entry point"));
}

// Warnings about projects that are valid but probably not what was meant. Every build below
// runs offline and stops at the toolchain, after the warnings are printed.

#[test]
fn build_and_check_warn_when_a_target_has_no_sources() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("empty", "[target.jvm]\n[target.wasm]\n"));
    for cmd in ["build", "check"] {
        p.kargo()
            .args(["--offline", cmd])
            .assert()
            .failure()
            .code(101)
            .stderr(predicate::str::contains("warning: no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target"))
            .stderr(predicate::str::contains("warning: no Kotlin sources found in src/ or src-wasm/ for the wasm target"));
    }

    // `run` and `package` only build the JVM target.
    for cmd in ["run", "package"] {
        p.kargo()
            .args(["--offline", cmd])
            .assert()
            .failure()
            .stderr(predicate::str::contains("warning: no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target"))
            .stderr(predicate::str::contains("for the wasm target").not());
    }

    // A target with sources is not mentioned, and `-q` silences the rest.
    p.file("src-jvm/Main.kt", "fun main() {}\n");
    p.kargo()
        .args(["--offline", "build"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("for the wasm target"))
        .stderr(predicate::str::contains("for the jvm target").not());
    p.kargo().args(["--offline", "-q", "build"]).assert().failure().stderr(predicate::str::contains("warning:").not());
}

#[test]
fn pre_build_hooks_can_provide_the_sources() {
    let p = Project::new();
    let hook =
        r#"gen = { run = 'mkdir -p src && touch src/Gen.kt', windows-run = 'mkdir src && type nul> src\Gen.kt', hooks = ["pre-build"] }"#;
    p.file("kargo.toml", &manifest("generated", &format!("[tasks]\n{hook}\n")));
    p.kargo().args(["--offline", "build"]).assert().failure().stderr(predicate::str::contains("no Kotlin or Java sources").not());
    assert!(p.path().join("src/Gen.kt").is_file());
}

#[test]
fn a_resources_only_jvm_project_has_nothing_to_warn_about() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("assets", "")).file("resources/config.properties", "a=1\n");
    p.kargo().args(["--offline", "build"]).assert().failure().stderr(predicate::str::contains("no Kotlin or Java sources").not());
}

#[test]
fn kotlin_scripts_are_reported_as_ignored() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("scripts", ""))
        .file("src/Main.kt", "fun main() {}\n")
        .file("src/script.kts", "println(\"script\")\n")
        .file("tests/Check.kts", "");
    for cmd in ["build", "check", "test"] {
        p.kargo()
            .args(["--offline", cmd])
            .assert()
            .failure()
            .stderr(predicate::str::contains("warning: `src/script.kts` is ignored: Kargo does not compile or run Kotlin scripts (.kts)"))
            .stderr(predicate::str::contains("warning: `tests/Check.kts` is ignored"));
    }
}

#[test]
fn tasks_named_like_built_in_commands_warn() {
    let p = Project::new();
    p.file("src/Main.kt", "fun main() {}\n");
    p.file("kargo.toml", &manifest("tasks", "[tasks]\nbuild = \"echo built\"\nb = \"echo alias\"\nci = \"echo ci\"\n"));
    p.kargo()
        .args(["--offline", "build"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "warning: task `build` is hidden by the built-in `kargo build` command; run the task with `kargo task build` or rename it",
        ))
        .stderr(predicate::str::contains("warning: task `b` is hidden by the built-in `kargo b` command"))
        .stderr(predicate::str::contains("task `ci`").not());

    // The listing marks them; running one through `kargo task` works and needs no warning.
    p.kargo()
        .arg("task")
        .assert()
        .success()
        .stdout(predicate::str::contains("echo built [hidden by `kargo build`; run it with `kargo task build`]"))
        .stdout(predicate::str::contains("echo ci\n"));
    p.kargo().args(["task", "build"]).assert().success().stderr(predicate::str::contains("hidden by").not());
    // A task a built-in command does not hide is run by name.
    p.kargo().arg("ci").assert().success().stderr(predicate::str::contains("hidden by").not());
}

#[test]
fn odd_package_versions_warn() {
    let p = Project::new();
    p.file("src/Main.kt", "fun main() {}\n");
    p.file("kargo.toml", "[package]\nname = \"odd\"\nversion = \"not.a.version.x\"\n");
    p.kargo()
        .args(["--offline", "build"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("warning: package version `not.a.version.x` does not look like a version"));

    p.file("kargo.toml", &manifest("odd", ""));
    p.kargo().args(["--offline", "build"]).assert().failure().stderr(predicate::str::contains("does not look like a version").not());
}

#[test]
fn warnings_in_a_workspace_name_the_package() {
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n")
        .file("app/kargo.toml", &manifest("app", ""))
        .file("app/src/Main.kt", "fun main() {}\n")
        .file("lib/kargo.toml", "[package]\nname = \"lib\"\nversion = \"one\"\n");
    p.kargo()
        .args(["--offline", "build", "-p", "lib"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("warning: package `lib`: package version `one` does not look like a version"))
        .stderr(predicate::str::contains("warning: package `lib`: no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target"));
    p.kargo().args(["--offline", "build", "-p", "app"]).assert().failure().stderr(predicate::str::contains("warning:").not());
}

#[test]
fn target_add_keeps_an_implicit_jvm_target_and_the_formatting() {
    let p = Project::new();
    let original = "# My app\n[package]\nname = \"multi\"   # the name\nversion = \"0.1.0\"\ngroup = \"org.acme\"\n\n[dependencies]\n";
    p.file("kargo.toml", original);
    p.file(
        "src/Main.kt",
        "package org.acme.multi\n\nfun greeting(name: String) = \"Hello, $name!\"\n\nfun main() = println(greeting(\"x\"))\n",
    );
    p.kargo()
        .args(["target", "add", "wasm", "native"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Adding jvm target"))
        .stderr(predicate::str::contains("no [target] section"))
        .stderr(predicate::str::contains("Adding wasm target"))
        .stderr(predicate::str::contains("Adding native target (executable)"))
        .stderr(predicate::str::contains("created src-wasm/.gitkeep"));
    let toml = p.read("kargo.toml");
    assert!(toml.starts_with(original), "{toml}");
    assert!(
        toml.ends_with(
            "\n[target.jvm]\n\n[target.wasm]\nenvironment = \"js\"\n\n[target.native]\nplatforms = [\"host\"]\nkind = \"executable\"\n"
        ),
        "{toml}"
    );
    assert!(p.path().join("src-native/.gitkeep").is_file());
    p.kargo()
        .args(["info", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"jvm\",\n    \"wasm\",\n    \"native\""));
}

#[test]
fn target_add_is_idempotent_and_infers_libraries() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("lib", "\n[target.jvm]\n"));
    p.file("src/Lib.kt", "package com.example.lib\n\nfun answer() = 42\n");
    p.kargo().args(["target", "add", "native"]).assert().success().stderr(predicate::str::contains("Adding native target (library)"));
    let before = p.read("kargo.toml");
    assert!(before.contains("kind = \"library\""), "{before}");
    p.kargo()
        .args(["target", "add", "native", "jvm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("the native target is already enabled"))
        .stderr(predicate::str::contains("the jvm target is already enabled"));
    assert_eq!(p.read("kargo.toml"), before);
    p.kargo().args(["target", "add", "android", "--app"]).assert().success();
    assert!(p.read("kargo.toml").contains("namespace = \"com.example.lib\"\nkind = \"application\""));
}

#[test]
fn target_add_android_scaffolds_without_overwriting() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("droid", ""));
    p.file("src/Main.kt", "package com.example.droid\n\nfun main() = println(\"hi\")\n");
    p.file("android/AndroidManifest.xml", "<manifest>mine</manifest>\n");
    p.kargo()
        .args(["target", "add", "android"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Adding android target (application)"))
        .stderr(predicate::str::contains("created src-android/MainActivity.kt"));
    assert_eq!(p.read("android/AndroidManifest.xml"), "<manifest>mine</manifest>\n");
    let activity = p.read("src-android/MainActivity.kt");
    assert!(activity.starts_with("package com.example.droid\n"), "{activity}");
    // The project has no `greeting` function to call.
    assert!(activity.contains("\"Hello, Android!\""), "{activity}");
    assert!(!p.path().join("src-android/.gitkeep").exists());
}

#[test]
fn target_add_warns_about_unsupported_frameworks() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("spring", "\n[frameworks]\nspring-boot = true\n"));
    p.kargo()
        .args(["target", "add", "wasm"])
        .assert()
        .success()
        .stderr(predicate::str::contains("the wasm target will not build with the enabled frameworks"));
}

#[test]
fn target_remove_keeps_sources_and_refuses_the_last_target() {
    let p = Project::new();
    p.file(
        "kargo.toml",
        &manifest("multi", "\n[target.jvm]\n\n[target.native]\nkind = \"library\"\n\n[target.native.dependencies]\n\"g:a\" = \"1.0\"\n\n[target.native.cinterop.zlib]\nheaders = [\"zlib.h\"]\n"),
    );
    p.file("src-native/Native.kt", "fun native() = 1\n");
    p.file("tests-native/NativeTest.kt", "\n");
    p.kargo()
        .args(["target", "remove", "native"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Removing native target"))
        .stderr(predicate::str::contains("kept src-native/, tests-native/ on disk"));
    let toml = p.read("kargo.toml");
    assert_eq!(toml, manifest("multi", "\n[target.jvm]\n"));
    assert!(p.path().join("src-native/Native.kt").is_file());
    p.kargo().args(["target", "remove", "wasm"]).assert().success().stderr(predicate::str::contains("the wasm target is not enabled"));
    p.kargo()
        .args(["target", "rm", "jvm"])
        .assert()
        .failure()
        .code(101)
        .stderr(predicate::str::contains("cannot remove the last target (jvm)"))
        .stderr(predicate::str::contains("kargo target add"));
    assert_eq!(p.read("kargo.toml"), toml);

    // An implicit JVM project has a single target, too.
    p.file("kargo.toml", &manifest("plain", ""));
    p.kargo().args(["target", "remove", "jvm"]).assert().failure().stderr(predicate::str::contains("cannot remove the last target"));
}

#[test]
fn target_list_shows_enabled_and_available_targets() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("plain", ""));
    p.kargo()
        .args(["target", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jvm      jvm-target"))
        .stdout(predicate::str::contains("(implicit: kargo.toml has no [target] section)"))
        .stdout(predicate::str::contains("Available targets"))
        .stdout(predicate::str::contains("    wasm\n    native\n    android\n"));
    p.file("kargo.toml", &manifest("multi", "\n[target.wasm]\nenvironment = \"browser\"\n"));
    let out = p.kargo().args(["target", "ls", "--format", "json"]).assert().success().get_output().stdout.clone();
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["enabled"][0]["name"], "wasm");
    assert_eq!(json["enabled"][0]["implicit"], false);
    assert_eq!(json["enabled"][0]["settings"]["environment"], "browser");
    assert_eq!(json["available"], serde_json::json!(["jvm", "native", "android"]));
}

#[test]
fn target_add_edits_the_selected_workspace_member() {
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    p.file("app/kargo.toml", &manifest("app", ""));
    p.file("lib/kargo.toml", &manifest("lib", ""));
    p.kargo().args(["target", "add", "wasm"]).assert().failure();
    p.kargo().args(["target", "add", "wasm", "-p", "lib"]).assert().success();
    assert!(p.read("lib/kargo.toml").contains("[target.wasm]"));
    assert!(!p.read("app/kargo.toml").contains("[target"));
    assert!(p.path().join("lib/src-wasm/.gitkeep").is_file());
}

#[test]
fn publish_checks_where_and_what_before_building() {
    let p = Project::new();
    let maven_local = tempfile::tempdir().unwrap();
    let kargo = || {
        let mut cmd = p.kargo();
        cmd.env("KARGO_MAVEN_LOCAL", maven_local.path());
        cmd
    };
    p.file("kargo.toml", &manifest("mathlib", ""));
    kargo().args(["publish", "--local"]).assert().failure().code(101).stderr(predicate::str::contains("set `group` in [package]"));

    p.file("kargo.toml", &manifest("mathlib", "group = \"com.acme\"\n"));
    kargo()
        .arg("publish")
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass `--repository <name>`").and(predicate::str::contains("--local")));
    kargo()
        .args(["publish", "--repository", "nowhere"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no repository named `nowhere`").and(predicate::str::contains("maven-central, maven-local")));
    kargo().args(["publish", "--local", "--repository", "x"]).assert().failure().stderr(predicate::str::contains("cannot be used with"));

    // Maven Central needs complete POM metadata, a release version and signatures.
    kargo().args(["publish", "--repository", "maven-central"]).assert().failure().stderr(
        predicate::str::contains("Maven Central requires `description`")
            .and(predicate::str::contains("`license`"))
            .and(predicate::str::contains("`authors`"))
            .and(predicate::str::contains("`repository`")),
    );
    let complete = "group = \"com.acme\"\ndescription = \"Math\"\nlicense = \"MIT\"\nauthors = [\"Ada <ada@example.com>\"]\nrepository = \"https://github.com/acme/mathlib\"\n";
    p.file("kargo.toml", &manifest("mathlib", complete).replace("0.1.0", "0.1.0-SNAPSHOT"));
    kargo().args(["publish", "--repository", "maven-central"]).assert().failure().stderr(predicate::str::contains("SNAPSHOT"));
    p.file("kargo.toml", &format!("{}\n[publish]\nsign = false\n", manifest("mathlib", complete)));
    kargo().args(["publish", "--repository", "maven-central"]).assert().failure().stderr(predicate::str::contains("requires signed files"));
    p.file("kargo.toml", &format!("{}\n[publish]\nrepository = \"maven-central\"\nsign = true\n", manifest("mathlib", complete)));
    kargo()
        .arg("publish")
        .env_remove("KARGO_MAVEN_CENTRAL_USERNAME")
        .env_remove("KARGO_MAVEN_CENTRAL_PASSWORD")
        .assert()
        .failure()
        .stderr(predicate::str::contains("KARGO_MAVEN_CENTRAL_USERNAME"));

    p.file("kargo.toml", &manifest("mathlib", "group = \"com.acme\"\npublish = false\n"));
    kargo().args(["publish", "--local"]).assert().failure().stderr(predicate::str::contains("`publish = false`"));
    assert!(std::fs::read_dir(maven_local.path()).unwrap().next().is_none(), "nothing was published");
}

#[test]
fn publish_skips_unpublishable_workspace_members() {
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"app\", \"tool\"]\n");
    p.file("app/kargo.toml", &manifest("app", "publish = false\n"));
    p.file("tool/kargo.toml", &manifest("tool", "publish = false\n"));
    p.kargo()
        .args(["publish", "--workspace", "--local"])
        .env("KARGO_MAVEN_LOCAL", p.path().join("m2"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipping `app`").and(predicate::str::contains("they all have `publish = false`")));
    // At the workspace root every member is selected, so the same applies without `--workspace`.
    p.kargo()
        .args(["publish", "--local"])
        .env("KARGO_MAVEN_LOCAL", p.path().join("m2"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipping `tool`"));
}

#[test]
fn publish_requires_published_path_dependencies() {
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"core\", \"api\"]\n");
    p.file("core/kargo.toml", &manifest("core", "publish = false\n"));
    p.file("api/kargo.toml", &manifest("api", "group = \"com.acme\"\n\n[dependencies]\ncore = { path = \"../core\" }\n"));
    p.kargo()
        .args(["publish", "-p", "api", "--local"])
        .env("KARGO_MAVEN_LOCAL", p.path().join("m2"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("depends on `core`, which has `publish = false`"));
}

#[test]
fn metadata_describes_a_workspace_for_editors() {
    let repo = LocalRepo::new();
    repo.publish("g:util", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", "6.1.3", &[]);
    let p = Project::new();
    p.file("kargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n")
        .file(
            "app/kargo.toml",
            &manifest(
                "app",
                &format!(
                    "[kotlin]\nversion = \"2.4.21\"\nopt-in = [\"kotlin.ExperimentalStdlibApi\"]\n[dependencies]\nlib = {{ path = \"../lib\" }}\n\
                     [repositories]\nlocal = \"{}\"\n[tasks]\ngen = {{ run = \"echo gen\", description = \"Generate\" }}\n",
                    repo.url()
                ),
            ),
        )
        .file("app/src/Main.kt", "package com.example\n\nfun main() {}\n")
        .file("lib/kargo.toml", &manifest("lib", "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:util\" = \"1.0\"\n"))
        .file("lib/src/Lib.kt", "package com.example.lib\n\nfun answer() = 42\n");

    let metadata = |args: &[&str]| -> serde_json::Value {
        let out = p.kargo().args(["--offline", "metadata"]).args(args).assert().success().get_output().stdout.clone();
        serde_json::from_slice(&out).unwrap()
    };
    let doc = metadata(&["--no-deps"]);
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["workspace"], true);
    assert_eq!(doc["resolved"], false);
    assert_eq!(doc["members"], serde_json::json!(["app", "lib"]));
    assert_eq!(doc["toolchain"]["jdk"]["home"], serde_json::Value::Null);
    let watch: Vec<&str> = doc["watch"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(watch.len(), 4, "the workspace manifest, both members' and kargo.lock: {watch:?}");
    let app = &doc["packages"][0];
    assert_eq!(app["name"], "app");
    assert_eq!(app["member"], true);
    assert_eq!(
        app["path-dependencies"][0],
        serde_json::json!({"name": "lib", "package": "lib", "scope": "compile", "dev": false, "target": null})
    );
    assert_eq!(app["tasks"][0]["name"], "gen");
    assert_eq!(app["tasks"][0]["description"], "Generate");
    let jvm = &app["targets"][0];
    assert_eq!(jvm["kind"], "jvm");
    assert_eq!(jvm["main-class"], "com.example.MainKt");
    assert_eq!(jvm["compiler-args"], serde_json::json!(["-opt-in=kotlin.ExperimentalStdlibApi"]));
    assert_eq!(jvm["dependencies"], serde_json::Value::Null);
    let root = |s: &serde_json::Value| std::path::PathBuf::from(s.as_str().unwrap());
    assert!(root(&jvm["source-roots"][0]).ends_with(std::path::Path::new("app").join("src")), "{jvm}");
    assert!(root(&jvm["source-roots"][1]).ends_with(std::path::Path::new("app").join("src-jvm")), "{jvm}");
    assert!(root(&jvm["test-roots"][0]).ends_with(std::path::Path::new("app").join("tests")), "{jvm}");
    assert!(root(&jvm["classes-dir"]).ends_with(std::path::Path::new("target").join("debug").join("app").join("jvm").join("classes")));
    assert_eq!(doc["packages"][1]["name"], "lib");
    assert!(!p.path().join("kargo.lock").exists(), "--no-deps resolves nothing");

    // With dependencies: the path dependency is listed by package, Maven modules by file.
    let doc = metadata(&[]);
    assert_eq!(doc["resolved"], true);
    let compile = doc["packages"][0]["targets"][0]["dependencies"][0]["compile"].as_array().unwrap().clone();
    assert_eq!(compile[0], serde_json::json!({"package": "lib", "scope": "compile"}));
    let util = compile.iter().find(|e| e["module"] == "g:util").unwrap_or_else(|| panic!("{compile:?}"));
    assert_eq!(util["version"], "1.0");
    assert!(util["path"].as_str().unwrap().ends_with("util-1.0.jar"), "{util}");
    assert_eq!(util["sources"], serde_json::Value::Null);
    assert!(p.path().join("kargo.lock").exists());

    // The fingerprint changes with kargo.toml.
    let fingerprint = |p: &Project| {
        String::from_utf8(p.kargo().args(["metadata", "--fingerprint"]).assert().success().get_output().stdout.clone()).unwrap()
    };
    let before = fingerprint(&p);
    assert_eq!(before.trim(), doc["fingerprint"].as_str().unwrap());
    assert_eq!(before, fingerprint(&p));
    p.file("lib/kargo.toml", &manifest("lib", "[kotlin]\nversion = \"2.4.21\"\n"));
    assert_ne!(before, fingerprint(&p));
}

#[test]
fn debug_needs_the_jvm_target() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("dbg", "[target.jvm]\n[target.wasm]\n")).file("src/Main.kt", "fun main() {}\n");
    p.kargo()
        .args(["--offline", "run", "-t", "wasm", "--debug"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`--debug` starts the JVM under a debugger and only works for the jvm target, not wasm"));
    p.kargo()
        .args(["--offline", "test", "--debug=6000"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("select the JVM target with `-t jvm`"));
}

/// LSP messages, framed for `kargo lsp`'s stdin.
fn lsp_input(messages: &[serde_json::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        let body = serde_json::to_vec(message).unwrap();
        out.extend(format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes());
        out.extend(body);
    }
    out
}

/// The messages `kargo lsp` wrote to stdout.
fn lsp_output(stdout: &[u8]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut rest = stdout;
    while let Some(start) = rest.windows(4).position(|w| w == b"\r\n\r\n") {
        let header = std::str::from_utf8(&rest[..start]).unwrap();
        let length: usize = header.trim().strip_prefix("Content-Length: ").unwrap().parse().unwrap();
        let body = &rest[start + 4..start + 4 + length];
        out.push(serde_json::from_slice(body).unwrap());
        rest = &rest[start + 4 + length..];
    }
    out
}

fn file_uri(path: &std::path::Path) -> String {
    let text = path.display().to_string().replace('\\', "/").replace(' ', "%20");
    if text.starts_with('/') { format!("file://{text}") } else { format!("file:///{text}") }
}

#[test]
fn lsp_serves_kargo_toml_and_runnables() {
    use serde_json::json;
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", "[tasks]\nhello = \"echo hi\"\n")).file("src/Main.kt", "fun main() {}\n");
    let uri = file_uri(&p.path().join("kargo.toml"));
    let broken = format!("{}[target.wasm]\nenvironment = \"web\"\n", manifest("app", ""));
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(p.path()), "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": broken } } }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion", "params": { "textDocument": { "uri": uri }, "position": { "line": 4, "character": 14 } } }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "kargo/runnables", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    let out = p.kargo().args(["lsp", "--server", "none"]).write_stdin(input).assert().success().get_output().stdout.clone();
    let messages = lsp_output(&out);
    let reply = |id: i64| messages.iter().find(|m| m["id"] == id && m.get("method").is_none()).unwrap_or_else(|| panic!("{messages:?}"));
    assert_eq!(reply(1)["result"]["capabilities"]["experimental"]["kargo"]["runnables"], true);
    let diagnostics = messages.iter().find(|m| m["method"] == "textDocument/publishDiagnostics").unwrap();
    let diagnostic = &diagnostics["params"]["diagnostics"][0];
    assert_eq!(diagnostic["message"], "`target.wasm.environment` must be one of `js`, `browser`, `wasi`, found `web`");
    assert_eq!(diagnostic["range"]["start"], json!({ "line": 4, "character": 14 }));
    let labels: Vec<&str> = reply(2)["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["js", "browser", "wasi"]);
    let runnables = reply(3)["result"].as_array().unwrap();
    let args: Vec<&serde_json::Value> = runnables.iter().map(|r| &r["args"]).collect();
    assert_eq!(args, [&json!(["run", "-t", "jvm"]), &json!(["test", "-t", "jvm"]), &json!(["task", "hello"])]);
    assert_eq!(runnables[0]["mainClass"], "MainKt");

    // Without `shutdown` first, `exit` is an error, as LSP specifies.
    let input = lsp_input(&[json!({ "jsonrpc": "2.0", "method": "exit" })]);
    p.kargo().args(["lsp", "--server", "none"]).write_stdin(input).assert().code(1);
}

#[test]
fn lsp_code_actions_edit_kargo_toml_like_the_cli() {
    use serde_json::json;
    let body = "# my app\n[package]\nname = \"app\"   # keep this\nversion = \"0.1.0\"\n\n[target.jvm]\n";
    let editor = Project::new();
    editor.file("kargo.toml", body).file("src/Main.kt", "fun main() {}\n");
    let cli = Project::new();
    cli.file("kargo.toml", body).file("src/Main.kt", "fun main() {}\n");
    let uri = file_uri(&editor.path().join("kargo.toml"));
    let typo = body.replace("[target.jvm]", "[target.wasm]\nenvironment = \"browsr\"");
    let line = |text: &str, needle: &str| text.lines().position(|l| l.contains(needle)).unwrap();
    let at = |l: usize| json!({ "start": { "line": l, "character": 0 }, "end": { "line": l, "character": 0 } });
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(editor.path()), "capabilities": { "workspace": { "applyEdit": true } } } }),
        json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": typo } } }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/codeAction", "params": { "textDocument": { "uri": uri }, "range": at(line(&typo, "browsr")), "context": { "diagnostics": [] } } }),
        json!({ "jsonrpc": "2.0", "method": "textDocument/didClose", "params": { "textDocument": { "uri": uri } } }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "workspace/executeCommand", "params": { "command": "kargo/addTarget", "arguments": [{ "uri": uri, "target": "native" }] } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    let out = editor.kargo().args(["lsp", "--server", "none"]).write_stdin(input).assert().success().get_output().stdout.clone();
    let messages = lsp_output(&out);
    let reply = |id: i64| messages.iter().find(|m| m["id"] == id && m.get("method").is_none()).unwrap_or_else(|| panic!("{messages:?}"));
    assert_eq!(reply(1)["result"]["capabilities"]["executeCommandProvider"]["commands"], json!(["kargo/addDependency", "kargo/addTarget"]));

    let found = reply(2)["result"].as_array().unwrap();
    let fix = found.iter().find(|a| a["isPreferred"] == true).unwrap_or_else(|| panic!("{found:?}"));
    assert_eq!(fix["title"], "Change to `browser`");
    // The smallest edit: `browsr` gets its `e`.
    assert_eq!(fix["edit"]["changes"][&uri][0]["newText"], "e");
    let framework = found.iter().find(|a| a["title"] == "Enable the `serialization` framework").unwrap_or_else(|| panic!("{found:?}"));
    assert_eq!(framework["kind"], "source");

    // Not open in the editor: Kargo writes kargo.toml and scaffolds, as `kargo target add` does.
    assert_eq!(reply(3)["result"]["targets"], json!(["native"]), "{messages:?}");
    cli.kargo().args(["target", "add", "native"]).assert().success();
    assert_eq!(editor.read("kargo.toml"), cli.read("kargo.toml"));
    assert!(editor.path().join("src-native").is_dir() && cli.path().join("src-native").is_dir());
}

#[test]
fn classpath_prints_the_resolved_jars_of_each_scope() {
    let repo = LocalRepo::new();
    repo.publish("g:util", "1.0", &[])
        .publish("g:rt", "1.0", &[])
        .publish("g:check", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", "6.1.3", &[]);
    let p = Project::new();
    let extra = format!(
        "[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:util\" = \"1.0\"\n\"g:rt\" = {{ version = \"1.0\", scope = \"runtime\" }}\n\
         [dev-dependencies]\n\"g:check\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n",
        repo.url()
    );
    p.file("kargo.toml", &manifest("app", &extra)).file("src/Main.kt", "fun main() {}\n");
    let lines = |args: &[&str]| -> Vec<String> {
        let out = p.kargo().args(["--offline", "classpath", "--format", "lines"]).args(args).assert().success().get_output().stdout.clone();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| std::path::Path::new(l).file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    let compile = lines(&[]);
    assert!(compile.contains(&"util-1.0.jar".to_owned()) && compile.contains(&"kotlin-stdlib-2.4.21.jar".to_owned()), "{compile:?}");
    assert!(!compile.contains(&"rt-1.0.jar".to_owned()) && !compile.contains(&"check-1.0.jar".to_owned()), "{compile:?}");
    let runtime = lines(&["--scope", "runtime"]);
    assert!(runtime.contains(&"rt-1.0.jar".to_owned()) && !runtime.contains(&"check-1.0.jar".to_owned()), "{runtime:?}");
    let test = lines(&["-s", "test", "-t", "jvm"]);
    assert!(test.contains(&"check-1.0.jar".to_owned()) && test.contains(&"junit-jupiter-6.1.3.jar".to_owned()), "{test:?}");
    assert!(p.path().join("kargo.lock").is_file(), "the resolution is locked");

    // The default format is one line joined with the platform's path separator.
    let out = p.kargo().args(["--offline", "classpath"]).assert().success().get_output().stdout.clone();
    let joined = String::from_utf8(out).unwrap();
    assert_eq!(std::env::split_paths(joined.trim_end()).count(), compile.len(), "{joined}");

    let out = p.kargo().args(["--offline", "classpath", "--format", "json"]).assert().success().get_output().stdout.clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!((doc["package"].as_str(), doc["target"].as_str(), doc["scope"].as_str()), (Some("app"), Some("jvm"), Some("compile")));
    assert_eq!(doc["classpath"].as_array().unwrap().len(), compile.len());

    p.kargo()
        .args(["--offline", "classpath", "-t", "wasm"])
        .assert()
        .code(101)
        .stderr(predicates::str::contains("target `wasm` is not enabled"));
}

#[test]
fn ide_writes_workspace_json_and_keeps_it_current() {
    let repo = LocalRepo::new();
    repo.publish("g:util", "1.0", &[])
        .publish("g:extra", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", "6.1.3", &[]);
    let p = Project::new();
    let extra =
        format!("[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:util\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n", repo.url());
    p.file("kargo.toml", &manifest("app", &extra)).file("src/Main.kt", "fun main() {}\n");
    let libraries = || -> Vec<String> {
        let workspace: serde_json::Value = serde_json::from_str(&p.read("workspace.json")).unwrap();
        workspace["libraries"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap().to_owned()).collect()
    };

    p.kargo().args(["--offline", "ide"]).assert().success().stderr(predicates::str::contains("Wrote workspace.json"));
    assert!(libraries().contains(&"Kargo: g:util:1.0".to_owned()), "{:?}", libraries());
    p.kargo().args(["--offline", "ide"]).assert().success().stderr(predicates::str::contains("workspace.json is up to date"));

    // A command that changes kargo.toml writes it again.
    p.kargo().args(["add", "g:extra:1.0", "--dev"]).assert().success().stderr(predicates::str::contains("Wrote workspace.json"));
    assert!(libraries().contains(&"Kargo: g:extra:1.0".to_owned()), "{:?}", libraries());

    // fwcd's server gets a script printing the test classpath.
    p.kargo().args(["--offline", "ide", "--kls"]).assert().success();
    let script = if cfg!(windows) { "kls-classpath.bat" } else { "kls-classpath" };
    assert!(p.read(script).contains("kargo ide"), "{}", p.read(script));
    let classpath = p.read("target/lsp/classpath.txt");
    assert!(classpath.contains("util-1.0.jar") && classpath.contains("extra-1.0.jar"), "{classpath}");

    // A workspace.json Kargo didn't write is left alone.
    p.file("workspace.json", "{}");
    p.kargo().args(["--offline", "ide"]).assert().success().stderr(predicates::str::contains("was not written by Kargo"));
    assert_eq!(p.read("workspace.json"), "{}");
}

#[test]
fn ide_zed_writes_tasks_for_the_run_buttons_and_keeps_them_current() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", ""))
        .file(
            "src/Main.kt",
            "fun main() {}
",
        )
        .file(
            "examples/demo.kt",
            "fun main() {}
",
        );
    let tasks = || -> serde_json::Value {
        let text = p.read(".zed/tasks.json");
        let json: String = text.lines().filter(|l| !l.starts_with("//")).collect::<Vec<_>>().join("\n");
        serde_json::from_str(&json).unwrap()
    };
    let labels = |tag: Option<&str>| -> Vec<String> {
        tasks()
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| tag.is_none_or(|tag| t["tags"].as_array().is_some_and(|tags| tags.iter().any(|x| x == tag))))
            .map(|t| t["label"].as_str().unwrap().to_owned())
            .collect()
    };

    p.kargo()
        .args(["--offline", "ide", "--zed"])
        .assert()
        .success()
        .stderr(predicates::str::contains("Wrote .zed/tasks.json").or(predicates::str::contains("Wrote .zed\\tasks.json")));
    assert_eq!(labels(Some("kotlin-main")), ["kargo run -t jvm", "kargo run --example demo"]);
    assert_eq!(labels(Some("kotlin-test")).len(), 1);
    assert!(!p.path().join("workspace.json").exists(), "--zed writes only the tasks");
    p.kargo().args(["--offline", "ide", "--zed"]).assert().success().stderr(predicates::str::contains(".zed/tasks.json is up to date"));

    // A command that changes kargo.toml writes it again.
    p.kargo().args(["--offline", "target", "add", "wasm"]).assert().success();
    assert!(labels(None).contains(&"kargo test -t wasm".to_owned()), "{:?}", labels(None));

    // Tasks someone else wrote are left alone.
    p.file(".zed/tasks.json", "[]");
    p.kargo().args(["--offline", "ide", "--zed"]).assert().success().stderr(predicates::str::contains("was not written by Kargo"));
    assert_eq!(p.read(".zed/tasks.json"), "[]");
    p.kargo().args(["ide", "--zed", "--kls"]).assert().failure();
}

#[test]
fn lsp_describes_the_project_for_the_kotlin_lsp() {
    use serde_json::json;
    let repo = LocalRepo::new();
    repo.publish("g:util", "1.0", &[])
        .publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", "6.1.3", &[]);
    let p = Project::new();
    let extra =
        format!("[kotlin]\nversion = \"2.4.21\"\n[dependencies]\n\"g:util\" = \"1.0\"\n[repositories]\nlocal = \"{}\"\n", repo.url());
    p.file("kargo.toml", &manifest("app", &extra)).file("src/Main.kt", "fun main() {}\n");
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(p.path()), "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    // Offline, the Kotlin LSP can't be downloaded: Kargo says so and keeps serving kargo.toml.
    let out = p.kargo().args(["--offline", "lsp"]).write_stdin(input).assert().success().get_output().stdout.clone();
    let messages = lsp_output(&out);
    let shown = messages.iter().find(|m| m["method"] == "window/showMessage").unwrap_or_else(|| panic!("{messages:?}"));
    assert!(shown["params"]["message"].as_str().unwrap().contains("offline"), "{shown}");
    assert_eq!(messages.iter().find(|m| m["id"] == 1).unwrap()["result"]["capabilities"]["hoverProvider"], true);

    let workspace: serde_json::Value = serde_json::from_str(&p.read("workspace.json")).unwrap();
    let modules: Vec<&str> = workspace["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(modules, ["app", "app.main", "app.test"]);
    let libraries: Vec<&str> = workspace["libraries"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert!(libraries.contains(&"Kargo: g:util:1.0"), "{libraries:?}");
    assert!(p.path().join("target/lsp/fingerprint").is_file(), "a complete import is remembered");

    // A workspace.json Kargo didn't write is left alone.
    p.file("workspace.json", "{}");
    p.file("kargo.toml", &format!("{}# changed\n", manifest("app", &extra)));
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(p.path()), "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    p.kargo().args(["--offline", "lsp", "--server", "kls"]).write_stdin(input.clone()).assert().code(1);
    p.kargo().args(["--offline", "lsp"]).write_stdin(input).assert().code(1);
    assert_eq!(p.read("workspace.json"), "{}");
}

#[test]
fn lsp_describes_multiplatform_source_sets() {
    use serde_json::json;
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", "[target.jvm]\n[target.wasm]\n"))
        .file("src/Platform.kt", "expect fun platformName(): String\n")
        .file("src-jvm/Platform.kt", "actual fun platformName(): String = \"JVM\"\n");
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(p.path()), "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    let modules = |args: &[&str]| -> Vec<String> {
        p.kargo().args(args).write_stdin(input.clone()).assert().code(1);
        let workspace: serde_json::Value = serde_json::from_str(&p.read("workspace.json")).unwrap();
        workspace["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap().to_owned()).collect()
    };
    // Offline, nothing resolves: the source sets are described without libraries.
    assert_eq!(modules(&["--offline", "lsp"]), ["app", "app.common.main", "app.jvm.main", "app.common.test", "app.jvm.test"]);
    assert_eq!(
        modules(&["--offline", "lsp", "--klib-targets"]),
        ["app", "app.common.main", "app.jvm.main", "app.wasm.main", "app.common.test", "app.jvm.test", "app.wasm.test"]
    );
}

#[test]
fn lsp_with_kargos_server_writes_nothing_into_the_project() {
    use serde_json::json;
    let repo = LocalRepo::new();
    repo.publish("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", &[])
        .publish("org.jetbrains.kotlin:kotlin-test-junit5", "2.4.21", &[])
        .publish("org.junit.jupiter:junit-jupiter", "6.1.3", &[]);
    let p = Project::new();
    let extra = format!("[kotlin]\nversion = \"2.4.21\"\n[repositories]\nlocal = \"{}\"\n", repo.url());
    p.file("kargo.toml", &manifest("app", &extra)).file("src/Main.kt", "fun main() {}\n");
    let input = lsp_input(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "rootUri": file_uri(p.path()), "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]);
    // The server has no release yet: Kargo says how to run a local build and keeps serving kargo.toml.
    let out =
        p.kargo().args(["--offline", "lsp", "--server", "kargo"]).write_stdin(input.clone()).assert().success().get_output().stdout.clone();
    let messages = lsp_output(&out);
    let shown = messages.iter().find(|m| m["method"] == "window/showMessage").unwrap_or_else(|| panic!("{messages:?}"));
    assert!(shown["params"]["message"].as_str().unwrap().contains("could not start the Kotlin language server"), "{shown}");
    assert_eq!(messages.iter().find(|m| m["id"] == 1).unwrap()["result"]["capabilities"]["hoverProvider"], true);
    assert!(!p.path().join("workspace.json").exists());
    assert!(p.read("target/lsp/fingerprint").starts_with("kargo-kotlin-server "), "a complete import is remembered");

    let missing = p.path().join("no-such-server");
    let args = ["--offline", "lsp", "--server", "kargo", "--server-path", missing.to_str().unwrap()];
    let out = p.kargo().args(args).write_stdin(input).assert().success().get_output().stdout.clone();
    let shown = lsp_output(&out).into_iter().find(|m| m["method"] == "window/showMessage").unwrap();
    assert!(shown["params"]["message"].as_str().unwrap().contains("could not start the Kotlin language server"), "{shown}");
}

#[test]
fn toolchain_lists_language_servers() {
    let p = Project::new();
    p.kargo()
        .args(["toolchain", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Kotlin language servers and debug adapter (").and(predicate::str::contains("(none installed)")));
    p.kargo().args(["--offline", "toolchain", "install", "kotlin-lsp"]).assert().failure().stderr(predicate::str::contains("offline"));
    p.kargo()
        .args(["--offline", "toolchain", "install", "kotlin-debug-adapter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("offline"));
    p.kargo().args(["toolchain", "remove", "kls", "1.3.13"]).assert().failure().stderr(predicate::str::contains("not installed"));
    p.kargo()
        .args(["--offline", "toolchain", "install", "kargo-kotlin-server"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--server-path"));
}
