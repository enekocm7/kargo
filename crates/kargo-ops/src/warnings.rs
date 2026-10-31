//! Warnings about projects that are valid but almost certainly not what the user meant:
//! targets with no sources, Kotlin scripts that are never compiled, tasks that a built-in
//! command hides, package versions that do not look like versions, and JavaScript modules
//! that Kotlin gets no declarations for.
//!
//! Each check returns plain messages; [`emit`] prints them with `Shell::warn`.

use std::path::{Path, PathBuf};

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
    messages.extend(npm_publishing(manifest));
    messages.extend(crate::builtins::compose_platform_advice(manifest));
    messages
}

/// Warnings about `[publish.npm]`: a table that has no package to publish, and an npm package without the
/// fields registries show on its page.
fn npm_publishing(manifest: &Manifest) -> Vec<String> {
    if manifest.publish.npm.is_none() {
        return Vec::new();
    }
    if !crate::ops::publish_npm::is_npm_library(manifest) {
        return vec![
            "[publish.npm] is ignored: no npm package is built; set `kind = \"library\"` under [target.wasm] or [target.js] to publish one"
                .to_owned(),
        ];
    }
    let pkg = &manifest.package;
    let missing: Vec<&str> =
        [("description", pkg.description.is_some()), ("license", pkg.license.is_some()), ("repository", pkg.repository.is_some())]
            .into_iter()
            .filter_map(|(key, set)| (!set).then_some(key))
            .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "the npm package has no {}; registries show them on the package's page, so set {} in [package]",
        missing.join(", no "),
        if missing.len() == 1 { "it" } else { "them" }
    )]
}

/// Warns about a `kargo.lock` in the package's own directory that a workspace's shared lockfile has
/// replaced (the package was a standalone project before it joined the workspace).
pub fn stale_lockfile(ws: &Workspace) -> Vec<String> {
    let own = ws.root().join("kargo.lock");
    if ws.lockfile_path() == own || !own.is_file() {
        return Vec::new();
    }
    vec![format!(
        "`{}` is ignored: the package is locked in the workspace's `{}`; delete the stale file",
        slash(&own),
        slash(&ws.lockfile_path())
    )]
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
                TargetKind::Wasm | TargetKind::Js | TargetKind::Native => ("Kotlin", &["kt"]),
                // Gradle reports an Android project without sources itself.
                TargetKind::Android => return None,
            };
            let has_sources = ws.source_dirs(target).iter().any(|dir| !collect_files(dir, extensions).is_empty());
            // A JVM project can be nothing but resources.
            if has_sources || (target == TargetKind::Jvm && ws.resource_dirs().iter().any(|d| has_entries(d))) {
                return None;
            }
            let layout = &ws.manifest().layout;
            let common = layout.src.first().map(|d| d.display().to_string()).unwrap_or_else(|| "src".to_owned());
            let specific =
                layout.target_sources(target).first().cloned().unwrap_or_else(|| std::path::PathBuf::from(target.source_dir_name()));
            Some(format!("no {kinds} sources found in {common}/ or {}/ for the {target} target", specific.display()))
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

/// Warnings about the JavaScript modules of the Wasm and JS targets among `targets` (`js/`) that have
/// no TypeScript types (a `.d.ts` or `.d.mts` next to them), so Kotlin gets no declarations for them.
pub fn untyped_modules(ws: &Workspace, targets: &[TargetKind]) -> Vec<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for &target in targets.iter().filter(|t| matches!(t, TargetKind::Wasm | TargetKind::Js)) {
        if crate::npm::declarations_supported(ws.manifest(), target) {
            dirs.extend(crate::js_sources::dirs(ws, target).into_iter().filter(|d| !dirs.contains(d)).collect::<Vec<_>>());
        }
    }
    let root = ws.root();
    let untyped: Vec<String> = dirs
        .iter()
        .flat_map(|dir| collect_files(dir, &["js", "mjs"]))
        .filter(|file| {
            let name = file.to_string_lossy();
            let stem = name.rsplit_once('.').map_or(&*name, |(stem, _)| stem);
            !["d.ts", "d.mts"].iter().any(|ext| Path::new(&format!("{stem}.{ext}")).is_file())
        })
        .map(|file| slash(file.strip_prefix(root).unwrap_or(&file)))
        .collect();
    let mut messages: Vec<String> = untyped
        .iter()
        .take(LISTED_FILES)
        .map(|file| {
            format!("`{file}` has no TypeScript types, so Kotlin gets no declarations for it; write a .d.ts file next to it, or make it a .ts file")
        })
        .collect();
    if untyped.len() > LISTED_FILES {
        messages.push(format!("{} more JavaScript modules have no TypeScript types too", untyped.len() - LISTED_FILES));
    }
    messages
}

/// Warnings about suite directories that have sources but are not declared in `kargo.toml`:
/// a directory like `integration-tests/` (or `integration-tests-<target>/`) with Kotlin or Java
/// sources that no `[test.suites.<name>]` names with `sources`.
pub fn undeclared_suites(ws: &Workspace) -> Vec<String> {
    let mut known: Vec<PathBuf> = vec![ws.common_tests()];
    for target in TargetKind::ALL {
        known.push(ws.target_tests(target));
    }
    for suite in ws.manifest().test.suites.values() {
        known.push(ws.root().join(&suite.sources));
        for target in TargetKind::ALL {
            known.push(ws.suite_target_tests(suite, target));
        }
    }
    let mut messages = Vec::new();
    let Ok(entries) = std::fs::read_dir(ws.root()) else { return messages };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if known.iter().any(|d| d == &path) {
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // A test suite directory ends with `-tests` (such as `integration-tests`) or is a
        // per-target `*-tests-<target>` directory (such as `integration-tests-jvm/`).
        let looks_like_suite =
            name.ends_with("-tests") || TargetKind::ALL.iter().any(|t| name.ends_with(&format!("-tests-{}", t.as_str())));
        if !looks_like_suite {
            continue;
        }
        if collect_files(&path, &["kt", "java"]).is_empty() {
            continue;
        }
        candidates.push(path);
    }
    candidates.sort();
    for path in candidates.into_iter().take(LISTED_FILES) {
        let rel = slash(path.strip_prefix(ws.root()).unwrap_or(&path));
        messages.push(format!(
            "`{rel}/` has test sources but no `[test.suites.<name>]` names it with `sources`; declare it to run it with `kargo test --suite <name>`"
        ));
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
    fn npm_packages_without_metadata_warn() {
        let library = "[target.wasm]\nkind = \"library\"\nenvironment = \"js\"\n[publish.npm]\n";
        let messages = manifest(&package("1.0.0", library), |_| false);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("no description, no license, no repository"), "{messages:?}");

        let described = format!("description = \"Greets\"\nlicense = \"MIT\"\n{library}");
        let messages = manifest(&package("1.0.0", &described), |_| false);
        assert!(messages[0].contains("no repository") && messages[0].contains("set it in [package]"), "{messages:?}");

        let complete = format!("description = \"Greets\"\nlicense = \"MIT\"\nrepository = \"https://github.com/acme/app\"\n{library}");
        assert!(manifest(&package("1.0.0", &complete), |_| false).is_empty());
        let without_table = "[target.wasm]\nkind = \"library\"\nenvironment = \"js\"\n";
        assert!(manifest(&package("1.0.0", without_table), |_| false).is_empty(), "only a project that publishes to npm is told");
    }

    #[test]
    fn publish_npm_without_a_library_warns() {
        let messages = manifest(&package("1.0.0", "[publish.npm]\n"), |_| false);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].starts_with("[publish.npm] is ignored"), "{messages:?}");
        let application = "[target.wasm]\nenvironment = \"browser\"\n[publish.npm]\n";
        assert_eq!(manifest(&package("1.0.0", application), |_| false).len(), 1, "an application is not an npm package");
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
    fn sources_of_an_intermediate_source_set_count() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = "[package]
name = \"app\"
version = \"1.0.0\"
[target.jvm]
[target.wasm]
[target.native]
                        [source-sets.web]
targets = [\"jvm\", \"wasm\"]
";
        std::fs::write(dir.path().join("kargo.toml"), manifest).unwrap();
        std::fs::create_dir_all(dir.path().join("src-web")).unwrap();
        std::fs::write(dir.path().join("src-web/Shared.kt"), "").unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        assert_eq!(
            missing_sources(&ws, &[TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native]),
            ["no Kotlin sources found in src/ or src-native/ for the native target"]
        );
    }

    #[test]
    fn a_resources_only_jvm_project_is_fine() {
        let (_dir, ws) = project(&["resources/config.properties"]);
        assert!(missing_sources(&ws, &[TargetKind::Jvm]).is_empty());
        assert_eq!(missing_sources(&ws, &[TargetKind::Wasm]).len(), 1);
    }

    #[test]
    fn undeclared_suite_directories_warn() {
        let (dir, _) = project(&["src/Main.kt", "integration-tests/DatabaseTest.kt", "integration-tests-jvm/JvmTest.kt"]);
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        let messages = undeclared_suites(&ws);
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("integration-tests/"), "{messages:?}");
        assert!(messages[0].contains("[test.suites.<name>]"), "{messages:?}");

        // Declared suites do not warn.
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"1.0.0\"\n[test.suites.integration]\n")
            .unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        assert!(undeclared_suites(&ws).is_empty(), "declared suites are fine");

        // Directories without sources do not warn.
        let (dir, _) = project(&["src/Main.kt"]);
        std::fs::create_dir_all(dir.path().join("empty-tests")).unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        assert!(undeclared_suites(&ws).is_empty());
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
    fn javascript_modules_without_types_are_named() {
        let (dir, _) = project(&["src/Main.kt", "js/plain.js", "js/typed.mjs", "js/typed.d.mts", "js/main.ts"]);
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[target.wasm]\n").unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        assert_eq!(
            untyped_modules(&ws, &[TargetKind::Wasm]),
            [
                "`js/plain.js` has no TypeScript types, so Kotlin gets no declarations for it; write a .d.ts file next to it, or make it a .ts file"
            ]
        );
        assert!(untyped_modules(&ws, &[TargetKind::Jvm]).is_empty(), "only the web targets generate declarations");
    }

    #[test]
    fn a_clean_project_has_no_warnings() {
        let (_dir, ws) = project(&["src/Main.kt", "tests/MainTest.kt"]);
        assert!(missing_sources(&ws, &[TargetKind::Jvm]).is_empty());
        assert!(ignored_files(&ws).is_empty());
    }
}
