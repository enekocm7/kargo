//! Checks of the projects in `examples/`, offline: they stay what `kargo new` writes, so they follow
//! Kargo's default versions, and each one loads and is listed in the index and the docs.
//!
//! `KARGO_UPDATE_EXAMPLES=1 cargo test --test examples` rewrites the examples from the templates.
//! The Examples workflow builds and tests them with the real compilers.

mod common;

use std::path::{Path, PathBuf};

use common::Project;

/// The examples `kargo new` writes: the directory and the arguments after `kargo new <dir>`.
const GENERATED: &[(&str, &[&str])] = &[
    ("hello-jvm", &[]),
    ("ktor-server", &["--with", "ktor,serialization"]),
    ("spring-boot", &["--with", "spring-boot"]),
    ("compose-desktop", &["--with", "compose"]),
    ("compose-multiplatform", &["--with", "compose-multiplatform"]),
    ("compose-html", &["--with", "compose-html"]),
    ("android", &["--target", "android"]),
    ("kotlin-wasm", &["--target", "wasm", "--with", "coroutines"]),
    ("kotlin-js", &["--target", "js"]),
    ("kotlin-native", &["--target", "native"]),
    ("multiplatform-library", &["--lib", "--target", "jvm,js,wasm", "--with", "serialization"]),
];

/// The examples written by hand.
const HAND_WRITTEN: &[&str] = &["workspace"];

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_below(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn names() -> Vec<&'static str> {
    GENERATED.iter().map(|(name, _)| *name).chain(HAND_WRITTEN.iter().copied()).collect()
}

#[test]
fn generated_examples_match_the_templates() {
    let update = std::env::var_os("KARGO_UPDATE_EXAMPLES").is_some();
    let p = Project::new();
    let mut stale = Vec::new();
    for (name, args) in GENERATED {
        p.kargo().args(["new", name, "--no-vcs"]).args(*args).assert().success();
        let generated = p.path().join(name);
        for file in files_below(&generated) {
            let rel = file.strip_prefix(&generated).unwrap();
            let example = examples_dir().join(name).join(rel);
            let want = std::fs::read(&file).unwrap();
            if std::fs::read(&example).ok().as_deref() == Some(want.as_slice()) {
                continue;
            }
            if update {
                std::fs::create_dir_all(example.parent().unwrap()).unwrap();
                std::fs::write(&example, want).unwrap();
            } else {
                stale.push(format!("examples/{name}/{}", rel.display()));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "these examples differ from `kargo new`; run `KARGO_UPDATE_EXAMPLES=1 cargo test --test examples`:\n{}",
        stale.join("\n")
    );
}

#[test]
fn every_directory_is_an_example_with_a_readme_and_a_docs_entry() {
    let mut dirs: Vec<String> = std::fs::read_dir(examples_dir())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    let mut want: Vec<String> = names().iter().map(|n| n.to_string()).collect();
    want.sort();
    assert_eq!(dirs, want, "add a new example to GENERATED or HAND_WRITTEN in tests/examples.rs");

    let index = std::fs::read_to_string(examples_dir().join("README.md")).unwrap();
    let docs = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("website/src/content/docs/guides/examples.md")).unwrap();
    for name in names() {
        assert!(examples_dir().join(name).join("README.md").is_file(), "examples/{name}/README.md is missing");
        assert!(index.contains(&format!("[`{name}`]({name}/)")), "examples/README.md does not list {name}");
        assert!(docs.contains(&format!("`{name}`")), "guides/examples.md does not list {name}");
    }
}

#[test]
fn examples_follow_the_default_versions() {
    for name in names() {
        for file in files_below(&examples_dir().join(name)) {
            if file.file_name().is_some_and(|n| n == "kargo.toml") {
                let text = std::fs::read_to_string(&file).unwrap();
                // A pinned Kotlin version is allowed only as the workspace's, and only the default.
                let pinned = text.contains("[kotlin]") || text.contains("[workspace.kotlin]");
                if pinned {
                    let want = format!("version = \"{}\"", kargo_manifest::DEFAULT_KOTLIN_VERSION);
                    assert!(
                        text.contains(&want),
                        "{} pins a Kotlin version other than {}",
                        file.display(),
                        kargo_manifest::DEFAULT_KOTLIN_VERSION
                    );
                }
                for key in ["jvm-target", "jdk ="] {
                    assert!(!text.contains(key), "{} pins `{key}`; examples use the defaults", file.display());
                }
            }
        }
    }
}

#[test]
fn examples_load() {
    for name in names() {
        let p = Project::new();
        let dir = examples_dir().join(name);
        let mut cmd = p.kargo();
        cmd.current_dir(&dir).args(["metadata", "--no-deps", "--offline"]).assert().success();
        assert!(!dir.join("kargo.lock").exists(), "loading {name} should not write a lockfile; delete examples/{name}/kargo.lock");
    }
}
