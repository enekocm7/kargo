//! The libraries Kargo's Kotlin server knows: the Kotlin standard library and `kotlin-test` at
//! the project's Kotlin version, from their sources jars (downloaded like any artifact and
//! cached), the sources jars of dependencies that are already in the cache (`kargo fetch
//! --sources`), and the compiled jars and klibs of the dependencies without one (see
//! [`super::compiled`]). Each jar is extracted (or its stubs generated) once under
//! `~/.kargo/cache/lsp-sources`, so go to definition opens real files. Without the standard
//! library's sources (offline, before the first download), a small built-in outline of it
//! stands in.
//!
//! A library file applies to the platforms it is on. The standard library's sources have a folder
//! per source set (`commonMain`, `jvmMain`, `wasmJsMain`, ...), and a file's folder says where it
//! applies. A file that several jars carry, such as the common code each platform's standard
//! library has, is one unit on the platforms of all of them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rayon::prelude::*;

use kargo_maven::{MavenClient, ModuleId, Repository};
use kargo_util::config::Config;

use super::compiled;
use super::index::{Origin, Platforms, Unit};

/// The built-in outline of the standard library, split into files at `// file:` lines.
const STUBS: &str = include_str!("stubs.kt");

/// A sources jar, what to call it, and the platforms it is for: `main` for main code and `test`
/// for test code only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcesJar {
    pub name: String,
    pub jar: PathBuf,
    pub main: Platforms,
    pub test: Platforms,
}

/// A library without sources: a jar of classes, or a klib (a file or an unpacked directory), and
/// the platforms it is for, as for a [`SourcesJar`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLibrary {
    pub name: String,
    pub path: PathBuf,
    pub main: Platforms,
    pub test: Platforms,
    /// For the Kotlin/Native standard library: the top-level declarations (`package.Name`) that
    /// its sources already give Native code, left out of its stubs (see
    /// [`compiled::native_stdlib_stubs`]).
    pub known: Option<Arc<BTreeSet<String>>>,
}

impl CompiledLibrary {
    fn is_klib(&self) -> bool {
        self.path.is_dir() || self.path.extension().is_some_and(|e| e == "klib")
    }
}

/// Bumped when the generated stubs change, so that cached ones are generated again.
const STUBS_FORMAT: &str = "1";

/// The parsed library files, by the path of their first copy and what they apply to. Kept across
/// reloads of the project.
pub type UnitCache = Arc<Mutex<HashMap<(PathBuf, Platforms, Platforms), Arc<Unit>>>>;

/// The sources jars of the standard library and `kotlin-test` at `version` for the platforms of
/// the project: the JVM's for JVM and Android targets, and the Wasm ones (`wasm` is the platform
/// of the Wasm target, `js` or `wasi`) for Wasm. Native code has no sources of its own in
/// Kargo's view, so it takes the Wasm standard library, whose folders shared with Native hold
/// its common code. Downloads them unless offline.
pub fn stdlib_jars(
    config: &Config,
    version: &str,
    repositories: Vec<Repository>,
    platforms: Platforms,
    wasm: Option<&str>,
) -> Vec<SourcesJar> {
    let mut modules: Vec<(String, Platforms, Platforms)> = Vec::new();
    if platforms.intersects(Platforms::JVM) {
        modules.push(("kotlin-stdlib".into(), Platforms::ALL, Platforms::NONE));
        modules.push(("kotlin-test".into(), Platforms::NONE, Platforms::ALL));
    }
    if platforms.intersects(Platforms::WASM.union(Platforms::NATIVE)) {
        let environment = wasm.unwrap_or("js");
        modules.push((format!("kotlin-stdlib-wasm-{environment}"), Platforms::ALL, Platforms::NONE));
        modules.push((format!("kotlin-test-wasm-{environment}"), Platforms::NONE, Platforms::ALL));
    }
    let client = MavenClient::new(config, repositories);
    modules
        .into_iter()
        .filter_map(|(artifact, main, test)| {
            let module = ModuleId::new("org.jetbrains.kotlin", &artifact);
            match client.fetch_sources(&module, version) {
                Ok(Some(jar)) => Some(SourcesJar { name: format!("{artifact} {version}"), jar, main, test }),
                _ => None,
            }
        })
        .collect()
}

/// The parsed Kotlin files of `jars`, each on the platforms it applies to (see [`merge`]). Files
/// parsed before are taken from `cache`.
pub fn units(config: &Config, cache: &UnitCache, jars: &[SourcesJar]) -> Vec<Arc<Unit>> {
    let listed: Vec<(&SourcesJar, PathBuf, Vec<PathBuf>)> = jars
        .iter()
        .filter_map(|jar| {
            let dir = extract(config, &jar.jar).ok()?;
            let files = kargo_util::fs::collect_files(&dir, &["kt"]);
            Some((jar, dir, files))
        })
        .collect();
    let wanted = merge(listed.iter().map(|(jar, dir, files)| (*jar, dir.as_path(), files.as_slice())));
    wanted
        .par_iter()
        .filter_map(|file| {
            let key = (file.path.clone(), file.main, file.test);
            if let Some(unit) = cache.lock().expect("lock poisoned").get(&key) {
                return Some(Arc::clone(unit));
            }
            let text = std::fs::read_to_string(&file.path).ok()?;
            let origin = Origin::Library { name: file.name.clone(), main: file.main, test: file.test };
            let unit = Arc::new(Unit::new(file.path.clone(), text, origin));
            cache.lock().expect("lock poisoned").insert(key, Arc::clone(&unit));
            Some(unit)
        })
        .collect()
}

/// The Kotlin files under `dir` as a library called `name` on the platforms `main` and `test`
/// (as for a [`SourcesJar`]). For generated declarations, which change, so they are parsed each time.
pub fn directory_units(name: &str, dir: &Path, main: Platforms, test: Platforms) -> Vec<Arc<Unit>> {
    kargo_util::fs::collect_files(dir, &["kt"])
        .par_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let origin = Origin::Library { name: name.to_owned(), main, test };
            Some(Arc::new(Unit::new(path.clone(), text, origin)))
        })
        .collect()
}

/// A library file to parse: the path of its first copy, what its jar calls it, and the platforms
/// it applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Wanted {
    path: PathBuf,
    name: String,
    main: Platforms,
    test: Platforms,
}

/// The Kotlin files of `listed` jars, each with its extracted directory and its files, one per
/// path inside the jar, sorted by that path. A file several jars carry applies to the platforms of
/// all of them. A file applies to the platforms its jar is for, narrowed to those of its top-level
/// folder (see [`folder_platforms`]); a file no platform of its jar takes is left out.
fn merge<'a>(listed: impl IntoIterator<Item = (&'a SourcesJar, &'a Path, &'a [PathBuf])>) -> Vec<Wanted> {
    let mut by_path: BTreeMap<String, Wanted> = BTreeMap::new();
    for (jar, dir, files) in listed {
        for path in files {
            let Ok(rel) = path.strip_prefix(dir) else { continue };
            let key = rel.to_string_lossy().replace('\\', "/");
            let folder = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            let folder = folder_platforms(&folder);
            let file = by_path.entry(key).or_insert_with(|| Wanted {
                path: path.clone(),
                name: jar.name.clone(),
                main: Platforms::NONE,
                test: Platforms::NONE,
            });
            file.main = file.main.union(jar.main.intersect(folder));
            file.test = file.test.union(jar.test.intersect(folder));
        }
    }
    by_path.into_values().filter(|file| !(file.main.is_empty() && file.test.is_empty())).collect()
}

/// The platforms of a top-level folder of a sources jar. The standard library's sources, and
/// those of multiplatform libraries, have a folder per source set, named as Gradle names them:
/// `commonMain`, `jvmMain`, `wasmJsMain`, `nativeWasmMain`, and so on. Common code is on every
/// platform, except `commonNonJvm` code, which is on Wasm and Native. Any other folder is on
/// every platform too, so nothing is hidden from it.
pub fn folder_platforms(folder: &str) -> Platforms {
    let set = match folder.strip_suffix("Main").or_else(|| folder.strip_suffix("Test")) {
        Some(set) => set.to_ascii_lowercase(),
        None => return Platforms::ALL,
    };
    if set.contains("nonjvm") {
        return Platforms::WASM.union(Platforms::NATIVE);
    }
    let mut platforms = Platforms::NONE;
    if set.starts_with("jvm") || set.starts_with("jdk") {
        platforms = platforms.union(Platforms::JVM);
    }
    if set.contains("wasm") || set.starts_with("web") {
        platforms = platforms.union(Platforms::WASM);
    }
    if set.contains("native") {
        platforms = platforms.union(Platforms::NATIVE);
    }
    if platforms.is_empty() { Platforms::ALL } else { platforms }
}

/// The parsed stubs of the compiled `libraries`, generated the first time. Files parsed before
/// are taken from `cache`.
pub fn compiled_units(config: &Config, cache: &UnitCache, libraries: &[CompiledLibrary]) -> Vec<Arc<Unit>> {
    libraries.iter().flat_map(|library| library_units(config, cache, library).unwrap_or_default()).collect()
}

fn library_units(config: &Config, cache: &UnitCache, library: &CompiledLibrary) -> Result<Vec<Arc<Unit>>> {
    let klib = library.is_klib();
    let mut suffix = format!("-stubs{STUBS_FORMAT}-{}", env!("CARGO_PKG_VERSION"));
    if let Some(known) = &library.known {
        let mut fp = kargo_util::hash::Fingerprint::new();
        for name in known.iter() {
            fp.str(name);
        }
        suffix = format!("{suffix}-{}", &fp.finish()[..12]);
    }
    let dir = cached_dir(config, &library.path, &suffix, |staging| {
        let stubs = match &library.known {
            Some(known) => compiled::native_stdlib_stubs(&library.path, known)?,
            None if klib => compiled::klib_stubs(&library.path)?,
            None => compiled::jar_stubs(&library.path)?,
        };
        for (rel, text) in stubs {
            let path = staging.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
        }
        Ok(())
    })?;
    let files = kargo_util::fs::collect_files(&dir, &["kt", "java"]);
    Ok(files
        .par_iter()
        .filter_map(|path| {
            let key = (path.clone(), library.main, library.test);
            if let Some(unit) = cache.lock().expect("lock poisoned").get(&key) {
                return Some(Arc::clone(unit));
            }
            let text = std::fs::read_to_string(path).ok()?;
            let origin = Origin::Library { name: library.name.clone(), main: library.main, test: library.test };
            let unit = Arc::new(Unit::new(path.clone(), text, origin));
            cache.lock().expect("lock poisoned").insert(key, Arc::clone(&unit));
            Some(unit)
        })
        .collect())
}

/// Where the sources of `jar` are extracted, extracting them the first time.
fn extract(config: &Config, jar: &Path) -> Result<PathBuf> {
    cached_dir(config, jar, "", |staging| kargo_toolchain::archive::extract_zip(jar, staging))
}

/// A directory under `~/.kargo/cache/lsp-sources` for what is made from `source` (a jar, a
/// klib or an unpacked klib), filled by `fill` the first time.
fn cached_dir(config: &Config, source: &Path, suffix: &str, fill: impl FnOnce(&Path) -> Result<()>) -> Result<PathBuf> {
    let stem = if source.is_dir() { source.file_name() } else { source.file_stem() };
    let stem = stem.map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sources".into());
    let mut fp = kargo_util::hash::Fingerprint::new();
    // An unpacked klib changes with its metadata; its IR is large and irrelevant here.
    let linkdata = source.join("default").join("linkdata");
    fp.file_stamp(if linkdata.is_dir() { &linkdata } else { source });
    let hash = fp.finish();
    let dir = config.home().join("cache").join("lsp-sources").join(format!("{stem}-{}{suffix}", &hash[..12]));
    let marker = dir.join(".kargo-complete");
    if marker.is_file() {
        return Ok(dir);
    }
    let staging = dir.with_extension(format!("tmp-{}", std::process::id()));
    let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
    std::fs::create_dir_all(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    if let Err(e) = fill(&staging) {
        let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
        return Err(e);
    }
    std::fs::write(staging.join(".kargo-complete"), "")?;
    if kargo_util::fs::rename_with_retries(&staging, &dir).is_err() {
        // Another `kargo lsp` extracted it first.
        let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
        if !marker.is_file() {
            anyhow::bail!("failed to prepare {}", source.display());
        }
    }
    Ok(dir)
}

/// The built-in outline of the standard library, written out once so go to definition opens it.
/// It is common code, so it applies to every platform.
pub fn stub_units(config: &Config) -> Vec<Arc<Unit>> {
    let dir = config.home().join("cache").join("lsp-sources").join(format!("kotlin-stdlib-outline-{}", env!("CARGO_PKG_VERSION")));
    stub_files()
        .into_iter()
        .map(|(rel, text)| {
            let path = dir.join(rel);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                let _ = kargo_util::fs::write_string_atomic(&path, &text);
            }
            let origin = Origin::Library { name: "Kotlin standard library outline".into(), main: Platforms::ALL, test: Platforms::NONE };
            Arc::new(Unit::new(path, text, origin))
        })
        .collect()
}

/// The files of [`STUBS`]: relative path and contents.
pub(crate) fn stub_files() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in STUBS.lines() {
        if let Some(name) = line.strip_prefix("// file: ") {
            out.push((name.trim().to_owned(), String::new()));
        } else if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jar(name: &str, main: Platforms, test: Platforms) -> SourcesJar {
        SourcesJar { name: name.into(), jar: PathBuf::from(name), main, test }
    }

    #[test]
    fn splits_the_outline_into_files() {
        let files = stub_files();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"kotlin/Core.kt") && names.contains(&"kotlin/io/Console.kt"), "{names:?}");
        for (name, text) in &files {
            let origin = Origin::Library { name: "outline".into(), main: Platforms::ALL, test: Platforms::NONE };
            let unit = Unit::new(PathBuf::from(name), text.clone(), origin);
            assert!(!unit.file.package.is_empty(), "{name} has a package");
            assert!(!unit.file.top.is_empty(), "{name} has declarations");
        }
    }

    #[test]
    fn folders_name_their_platforms() {
        assert_eq!(folder_platforms("commonMain"), Platforms::ALL);
        assert_eq!(folder_platforms("assertionsCommonMain"), Platforms::ALL);
        assert_eq!(folder_platforms("jvmMain"), Platforms::JVM);
        assert_eq!(folder_platforms("jdk8Main"), Platforms::JVM);
        assert_eq!(folder_platforms("wasmCommonMain"), Platforms::WASM);
        assert_eq!(folder_platforms("wasmJsMain"), Platforms::WASM);
        assert_eq!(folder_platforms("webMain"), Platforms::WASM);
        assert_eq!(folder_platforms("commonNonJvmMain"), Platforms::WASM.union(Platforms::NATIVE));
        assert_eq!(folder_platforms("nativeWasmMain"), Platforms::WASM.union(Platforms::NATIVE));
        assert_eq!(folder_platforms("nativeWasmWasiMain"), Platforms::WASM.union(Platforms::NATIVE));
        assert_eq!(folder_platforms("kotlin"), Platforms::ALL, "a folder that isn't a source set");
    }

    #[test]
    fn a_file_several_jars_carry_applies_to_all_of_them() {
        let jvm = jar("kotlin-stdlib", Platforms::ALL, Platforms::NONE);
        let wasm = jar("kotlin-stdlib-wasm-js", Platforms::ALL, Platforms::NONE);
        let jvm_dir = Path::new("/cache/jvm");
        let wasm_dir = Path::new("/cache/wasm");
        let jvm_files = [jvm_dir.join("commonMain/kotlin/A.kt"), jvm_dir.join("jvmMain/kotlin/io/File.kt")];
        let wasm_files = [wasm_dir.join("commonMain/kotlin/A.kt"), wasm_dir.join("wasmJsMain/kotlin/js/Any.kt")];
        let files = merge([(&jvm, jvm_dir, jvm_files.as_slice()), (&wasm, wasm_dir, wasm_files.as_slice())]);
        let find = |key: &str| files.iter().find(|f| f.path.ends_with(key)).unwrap_or_else(|| panic!("{key} is in the merge"));
        // Common code is one file on both, from the first jar.
        assert_eq!(files.iter().filter(|f| f.path.ends_with("commonMain/kotlin/A.kt")).count(), 1);
        assert_eq!(find("commonMain/kotlin/A.kt").main, Platforms::ALL);
        assert_eq!(find("commonMain/kotlin/A.kt").path, jvm_dir.join("commonMain/kotlin/A.kt"));
        assert_eq!(find("jvmMain/kotlin/io/File.kt").main, Platforms::JVM);
        assert_eq!(find("wasmJsMain/kotlin/js/Any.kt").main, Platforms::WASM);
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn a_test_only_jar_stays_out_of_main_code() {
        let test = jar("kotlin-test", Platforms::NONE, Platforms::ALL);
        let dir = Path::new("/cache/test");
        let jvm_files = [dir.join("assertionsCommonMain/kotlin/test/Assertions.kt"), dir.join("jvmMain/kotlin/test/Jvm.kt")];
        let files = merge([(&test, dir, jvm_files.as_slice())]);
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.main.is_empty()), "{files:?}");
        let common = files.iter().find(|f| f.path.ends_with("assertionsCommonMain/kotlin/test/Assertions.kt")).unwrap();
        assert_eq!(common.test, Platforms::ALL);
        let jvm = files.iter().find(|f| f.path.ends_with("jvmMain/kotlin/test/Jvm.kt")).unwrap();
        assert_eq!(jvm.test, Platforms::JVM);
    }

    #[test]
    fn a_dependency_jar_applies_to_its_own_platforms() {
        let lib = jar("lib-jvm", Platforms::JVM, Platforms::NONE);
        let dir = Path::new("/cache/lib");
        let files = [dir.join("lib/Api.kt"), dir.join("jvmMain/lib/Jvm.kt")];
        let merged = merge([(&lib, dir, files.as_slice())]);
        // A folder that isn't a source set leaves the jar's platforms as they are.
        assert_eq!(merged.iter().find(|f| f.path.ends_with("lib/Api.kt")).unwrap().main, Platforms::JVM);
        assert_eq!(merged.iter().find(|f| f.path.ends_with("jvmMain/lib/Jvm.kt")).unwrap().main, Platforms::JVM);
    }
}
