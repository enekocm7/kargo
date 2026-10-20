//! Gradle's source sets and where their files go in Kargo's layout.

use std::path::{Path, PathBuf};

use kargo_manifest::TargetKind;

use crate::ops::import::model::DirMove;

/// Whether Kargo can build a source set at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetKind {
    Supported,
    /// Kotlin/JS, and Android's instrumented tests.
    Unsupported,
}

/// A source set classified by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetInfo {
    /// `None` for the common source sets.
    pub target: Option<TargetKind>,
    pub test: bool,
    pub kind: SetKind,
}

/// Prefixes of the names of Kotlin/Native source sets (`linuxX64Main`, `iosMain`, `appleTest`).
const NATIVE_PREFIXES: &[&str] = &["native", "linux", "mingw", "macos", "ios", "tvos", "watchos", "apple", "androidNative", "windows"];

/// What the source set called `set` (`commonMain`, `jvmTest`, `linuxX64Main`) is. `None` for source
/// sets of the project's own (`jvmAndNativeMain`) and for names that are no source sets.
pub fn classify(set: &str, jvm_targets: &[String]) -> Option<SetInfo> {
    // Android's own `androidTest` holds instrumented tests, which Kargo does not run.
    if set == "androidTest" {
        return Some(SetInfo { target: Some(TargetKind::Android), test: true, kind: SetKind::Unsupported });
    }
    let (base, test, instrumented) = if let Some(base) = set.strip_suffix("Main") {
        (base, false, false)
    } else if let Some(base) = set.strip_suffix("InstrumentedTest") {
        (base, true, true)
    } else {
        let base = set.strip_suffix("UnitTest").or_else(|| set.strip_suffix("Test"))?;
        (base, true, false)
    };
    let (target, kind) = match base {
        "" => return None,
        "common" => (None, SetKind::Supported),
        "jvm" => (Some(TargetKind::Jvm), SetKind::Supported),
        name if jvm_targets.iter().any(|t| t == name) => (Some(TargetKind::Jvm), SetKind::Supported),
        "android" => (Some(TargetKind::Android), SetKind::Supported),
        "js" => (None, SetKind::Unsupported),
        name if name.starts_with("wasm") => (Some(TargetKind::Wasm), SetKind::Supported),
        name if NATIVE_PREFIXES
            .iter()
            .any(|p| name == *p || name.strip_prefix(p).is_some_and(|rest| rest.starts_with(char::is_uppercase))) =>
        {
            (Some(TargetKind::Native), SetKind::Supported)
        }
        _ => return None,
    };
    let kind = if instrumented { SetKind::Unsupported } else { kind };
    Some(SetInfo { target, test, kind })
}

/// The directory a source set's Kotlin and Java files go to, relative to the package.
fn code_dir(info: &SetInfo) -> String {
    let base = if info.test { "tests" } else { "src" };
    match info.target {
        None => base.to_owned(),
        Some(target) => format!("{base}-{}", target.as_str()),
    }
}

fn mv(dir: &Path, from: &str, to: &str) -> DirMove {
    DirMove { from: dir.join(from), to: dir.join(to) }
}

/// The moves for the conventional layout of a JVM or Android module in `dir`: `src/main` and `src/test`.
/// `main_dirs` and `test_dirs` are source directories the build adds with `srcDir`.
pub fn standard_moves(dir: &Path, android: bool, main_dirs: &[PathBuf], test_dirs: &[PathBuf]) -> Vec<DirMove> {
    let mut moves = vec![
        mv(dir, "src/main/kotlin", "src"),
        mv(dir, "src/main/java", "src"),
        mv(dir, "src/test/kotlin", "tests"),
        mv(dir, "src/test/java", "tests"),
        mv(dir, "src/main/resources", "resources"),
        mv(dir, "src/test/resources", "test-resources"),
    ];
    if android {
        moves.extend(android_moves(dir, "src/main"));
    }
    for from in main_dirs {
        moves.push(DirMove { from: dir.join(from), to: dir.join("src") });
    }
    for from in test_dirs {
        moves.push(DirMove { from: dir.join(from), to: dir.join("tests") });
    }
    moves
}

/// The Android manifest, resources and assets of the source set at `set_dir` (relative to the package).
fn android_moves(dir: &Path, set_dir: &str) -> Vec<DirMove> {
    vec![
        mv(dir, &format!("{set_dir}/AndroidManifest.xml"), "android/AndroidManifest.xml"),
        mv(dir, &format!("{set_dir}/res"), "android/res"),
        mv(dir, &format!("{set_dir}/assets"), "android/assets"),
    ]
}

/// What the layout of a multiplatform module (or a module with variants) leaves for the user.
#[derive(Debug, Default)]
pub struct MultiplatformLayout {
    pub moves: Vec<DirMove>,
    /// Source sets that have files but no place in Kargo's layout, with the reason.
    pub left: Vec<String>,
    /// Source sets of Kotlin/Native platforms that now share `src-native/`.
    pub merged_native: Vec<String>,
}

/// The moves for the source sets in `root/<dir>/src`.
pub fn multiplatform_moves(root: &Path, dir: &Path, jvm_targets: &[String]) -> MultiplatformLayout {
    let mut layout = MultiplatformLayout::default();
    let Ok(entries) = std::fs::read_dir(root.join(dir).join("src")) else { return layout };
    let mut sets: Vec<String> =
        entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    sets.sort();
    for set in sets {
        let Some(info) = classify(&set, jvm_targets) else {
            layout.left.push(format!("the source set `src/{set}` is not one Kargo has a directory for"));
            continue;
        };
        if info.kind == SetKind::Unsupported {
            layout.left.push(format!("the source set `src/{set}` (Kotlin/JS and Android instrumented tests are not supported)"));
            continue;
        }
        let from = format!("src/{set}");
        let to = code_dir(&info);
        layout.moves.push(mv(dir, &format!("{from}/kotlin"), &to));
        layout.moves.push(mv(dir, &format!("{from}/java"), &to));
        match (info.test, info.target) {
            (false, None | Some(TargetKind::Jvm | TargetKind::Wasm)) => {
                layout.moves.push(mv(dir, &format!("{from}/resources"), "resources"))
            }
            (true, None | Some(TargetKind::Jvm)) => layout.moves.push(mv(dir, &format!("{from}/resources"), "test-resources")),
            _ => {
                if root.join(dir).join(&from).join("resources").is_dir() {
                    layout.left.push(format!("the resources of `src/{set}` have no place in Kargo's layout"));
                }
            }
        }
        if info.target == Some(TargetKind::Android) && !info.test {
            layout.moves.extend(android_moves(dir, &from));
        }
        if info.target == Some(TargetKind::Native) && set != "nativeMain" && set != "nativeTest" {
            layout.merged_native.push(set);
        }
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_source_sets() {
        let c = |name: &str| classify(name, &["desktop".to_owned()]).map(|i| (i.target, i.test, i.kind == SetKind::Supported));
        assert_eq!(c("commonMain"), Some((None, false, true)));
        assert_eq!(c("commonTest"), Some((None, true, true)));
        assert_eq!(c("jvmMain"), Some((Some(TargetKind::Jvm), false, true)));
        assert_eq!(c("desktopTest"), Some((Some(TargetKind::Jvm), true, true)));
        assert_eq!(c("wasmJsMain"), Some((Some(TargetKind::Wasm), false, true)));
        assert_eq!(c("linuxX64Main"), Some((Some(TargetKind::Native), false, true)));
        assert_eq!(c("iosMain"), Some((Some(TargetKind::Native), false, true)));
        assert_eq!(c("nativeTest"), Some((Some(TargetKind::Native), true, true)));
        assert_eq!(c("androidMain"), Some((Some(TargetKind::Android), false, true)));
        assert_eq!(c("androidUnitTest"), Some((Some(TargetKind::Android), true, true)));
        assert_eq!(c("androidInstrumentedTest"), Some((Some(TargetKind::Android), true, false)));
        assert_eq!(c("jsMain"), Some((None, false, false)));
        assert_eq!(c("jvmAndNativeMain"), None);
        assert_eq!(c("main"), None);
    }

    #[test]
    fn multiplatform_layout_follows_the_source_sets() {
        let dir = tempfile::tempdir().unwrap();
        for set in ["commonMain", "commonTest", "jvmMain", "wasmJsMain", "linuxX64Main", "jsMain", "jvmAndNativeMain"] {
            std::fs::create_dir_all(dir.path().join("src").join(set).join("kotlin")).unwrap();
        }
        let layout = multiplatform_moves(dir.path(), Path::new(""), &[]);
        let has = |from: &str, to: &str| layout.moves.iter().any(|m| m.from == Path::new(from) && m.to == Path::new(to));
        assert!(has("src/commonMain/kotlin", "src"));
        assert!(has("src/commonTest/kotlin", "tests"));
        assert!(has("src/jvmMain/kotlin", "src-jvm"));
        assert!(has("src/wasmJsMain/kotlin", "src-wasm"));
        assert!(has("src/linuxX64Main/kotlin", "src-native"));
        assert_eq!(layout.merged_native, ["linuxX64Main"]);
        assert_eq!(layout.left.len(), 2, "{:?}", layout.left);
    }
}
