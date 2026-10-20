//! Warnings about projects that are valid but almost certainly not what the user meant:
//! targets with no sources, Kotlin scripts that are never compiled, tasks that a built-in
//! command hides, and package versions that do not look like versions.
//!
//! Each check returns plain messages; [`emit`] prints them with `Shell::warn`.

use std::path::Path;

use kargo_manifest::{Manifest, TargetKind};
use kargo_util::fs::collect_files;
use kargo_util::process::slash;
use kargo_util::shell::Shell;

use crate::workspace::{Project, Workspace};

/// How many ignored files are named before the rest are counted.
const LISTED_FILES: usize = 5;

/// Prints `messages` as warnings; in a workspace each one names the package it is about.
pub fn emit(shell: &Shell, project: &Project, ws: &Workspace, messages: Vec<String>) {
    for message in messages {
        if project.is_workspace() {
            shell.warn(format!("package `{}`: {message}", ws.name()));
        } else {
            shell.warn(message);
        }
    }
}

/// Warnings about `kargo.toml` itself. `is_builtin_command` tells whether `kargo <name>`
/// runs a built-in command.
pub fn manifest(manifest: &Manifest, is_builtin_command: impl Fn(&str) -> bool) -> Vec<String> {
    let mut messages = Vec::new();
    let version = &manifest.package.version;
    if !looks_like_version(version) {
        messages.push(format!(
            "package version `{version}` does not look like a version (expected something like `1.0.0`); \
             it becomes part of the jar's file name and of its Maven coordinates"
        ));
    }
    for name in manifest.tasks.keys().filter(|name| is_builtin_command(name)) {
        messages.push(format!(
            "task `{name}` is hidden by the built-in `kargo {name}` command; run the task with `kargo task {name}` or rename it"
        ));
    }
    messages
}

/// Maven accepts any version string, but versions that start with a number and use only
/// letters, digits and `.-_+` (`1.0`, `2.1.0-SNAPSHOT`, `2024.03`) work everywhere.
fn looks_like_version(version: &str) -> bool {
    version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Warnings about each of `targets` that has nothing to compile. Call it after the
/// pre-build hooks, which may generate sources.
pub fn missing_sources(ws: &Workspace, targets: &[TargetKind]) -> Vec<String> {
    targets
        .iter()
        .filter_map(|&target| {
            let (kinds, extensions): (&str, &[&str]) = match target {
                TargetKind::Jvm => ("Kotlin or Java", &["kt", "java"]),
                TargetKind::Wasm | TargetKind::Native => ("Kotlin", &["kt"]),
                // Gradle reports an Android project without sources itself.
                TargetKind::Android => return None,
            };
            let has_sources = [ws.common_sources(), ws.target_sources(target)].iter().any(|dir| !collect_files(dir, extensions).is_empty());
            // A JVM project can be nothing but resources.
            if has_sources || (target == TargetKind::Jvm && has_entries(&ws.resources())) {
                return None;
            }
            Some(format!("no {kinds} sources found in src/ or {}/ for the {target} target", target.source_dir_name()))
        })
        .collect()
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// Warnings about files in the source directories that Kargo never compiles: Kotlin scripts.
pub fn ignored_files(ws: &Workspace) -> Vec<String> {
    let root = ws.root();
    let scripts: Vec<String> = ws
        .source_roots()
        .iter()
        .flat_map(|dir| collect_files(&root.join(dir), &["kts"]))
        .map(|file| slash(file.strip_prefix(root).unwrap_or(&file)))
        .collect();
    let mut messages: Vec<String> = scripts
        .iter()
        .take(LISTED_FILES)
        .map(|file| format!("`{file}` is ignored: Kargo does not compile or run Kotlin scripts (.kts); put the code in a .kt file, or run it with `kargo script <file>`"))
        .collect();
    if scripts.len() > LISTED_FILES {
        messages.push(format!("{} more Kotlin scripts (.kts) are ignored too", scripts.len() - LISTED_FILES));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str, extra: &str) -> Manifest {
        Manifest::parse(&format!("[package]\nname = \"app\"\nversion = \"{version}\"\n{extra}")).unwrap()
    }

    /// A project in a scratch directory with the given files (and `kargo.toml` for `[package] app`).
    fn project(files: &[&str]) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        for file in files.iter().copied().chain(["kargo.toml"]) {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let contents = if file == "kargo.toml" { "[package]\nname = \"app\"\nversion = \"1.0.0\"\n" } else { "" };
            std::fs::write(path, contents).unwrap();
        }
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        (dir, ws)
    }

    #[test]
    fn versions_that_look_like_versions_pass() {
        for version in ["0.1.0", "1", "1.0-SNAPSHOT", "2.1.0-rc.1", "2024.03", "1.0.0+build.5", "1_0"] {
            assert!(manifest(&package(version, ""), |_| false).is_empty(), "{version}");
        }
    }

    #[test]
    fn odd_versions_warn() {
        for version in ["not.a.version.x", "v1.0", "latest", "1/2"] {
            let messages = manifest(&package(version, ""), |_| false);
            assert_eq!(messages.len(), 1, "{version}");
            assert!(messages[0].contains(&format!("package version `{version}` does not look like a version")), "{messages:?}");
        }
    }

    #[test]
    fn tasks_hidden_by_built_in_commands_warn() {
        let tasks = "[tasks]\nbuild = \"echo b\"\nci = \"echo c\"\nfmt = \"echo f\"\n";
        let messages = manifest(&package("1.0.0", tasks), |name| matches!(name, "build" | "fmt"));
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("task `build` is hidden by the built-in `kargo build` command"), "{messages:?}");
        assert!(messages[0].contains("kargo task build"), "{messages:?}");
        assert!(messages[1].contains("task `fmt`"), "{messages:?}");
    }

    #[test]
    fn empty_targets_warn() {
        let (_dir, ws) = project(&[]);
        let all = [TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native, TargetKind::Android];
        assert_eq!(
            missing_sources(&ws, &all),
            [
                "no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target",
                "no Kotlin sources found in src/ or src-wasm/ for the wasm target",
                "no Kotlin sources found in src/ or src-native/ for the native target",
            ]
        );
    }

    #[test]
    fn sources_in_either_directory_count() {
        let (_dir, ws) = project(&["src-jvm/Main.kt", "src-wasm/Wasm.kt", "src-native/Notes.txt"]);
        let messages = missing_sources(&ws, &[TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native]);
        assert_eq!(messages, ["no Kotlin sources found in src/ or src-native/ for the native target"]);

        let (_dir, ws) = project(&["src/Common.kt"]);
        assert!(
            missing_sources(&ws, &[TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native]).is_empty(),
            "src/ is shared by every target"
        );

        let (_dir, ws) = project(&["src-jvm/Util.java"]);
        assert!(missing_sources(&ws, &[TargetKind::Jvm]).is_empty(), "Java alone is enough on the JVM");
        let (_dir, ws) = project(&["src/Util.java"]);
        assert_eq!(missing_sources(&ws, &[TargetKind::Wasm]).len(), 1, "Java is no source for Wasm");
    }

    #[test]
    fn a_resources_only_jvm_project_is_fine() {
        let (_dir, ws) = project(&["resources/config.properties"]);
        assert!(missing_sources(&ws, &[TargetKind::Jvm]).is_empty());
        assert_eq!(missing_sources(&ws, &[TargetKind::Wasm]).len(), 1);
    }

    #[test]
    fn kotlin_scripts_are_reported() {
        let (_dir, ws) = project(&["src/Main.kt", "src/script.kts", "src-jvm/deep/more.kts", "tests/Check.kts", "resources/data.kts"]);
        let messages = ignored_files(&ws);
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].starts_with("`src/script.kts` is ignored: Kargo does not compile or run Kotlin scripts"), "{messages:?}");
        assert!(messages[1].starts_with("`src-jvm/deep/more.kts` is ignored"), "{messages:?}");
        assert!(messages[2].starts_with("`tests/Check.kts` is ignored"), "resources are not source directories: {messages:?}");
    }

    #[test]
    fn many_scripts_are_summarized() {
        let files: Vec<String> = (0..8).map(|i| format!("src/s{i}.kts")).collect();
        let (_dir, ws) = project(&files.iter().map(String::as_str).collect::<Vec<_>>());
        let messages = ignored_files(&ws);
        assert_eq!(messages.len(), LISTED_FILES + 1);
        assert_eq!(messages.last().unwrap(), "3 more Kotlin scripts (.kts) are ignored too");
    }

    #[test]
    fn a_clean_project_has_no_warnings() {
        let (_dir, ws) = project(&["src/Main.kt", "tests/MainTest.kt"]);
        assert!(missing_sources(&ws, &[TargetKind::Jvm]).is_empty());
        assert!(ignored_files(&ws).is_empty());
    }
}
