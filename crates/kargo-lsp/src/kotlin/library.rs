//! The libraries Kargo's Kotlin server knows: the Kotlin standard library and `kotlin-test` at
//! the project's Kotlin version, from their sources jars (downloaded like any artifact and
//! cached), and the sources jars of dependencies that are already in the cache (`kargo fetch
//! --sources`). Each jar is extracted once under `~/.kargo/cache/lsp-sources`, so go to definition
//! opens real files. Without the standard library's sources (offline, before the first
//! download), a small built-in outline of it stands in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;

use kargo_maven::{MavenClient, ModuleId, Repository};
use kargo_util::config::Config;

use super::index::{Origin, Unit};

/// The built-in outline of the standard library, split into files at `// file:` lines.
const STUBS: &str = include_str!("stubs.kt");

/// A sources jar and what to call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcesJar {
    pub name: String,
    pub jar: PathBuf,
}

/// The sources jars of the standard library and `kotlin-test` at `version`, downloading them
/// unless offline.
pub fn stdlib_jars(config: &Config, version: &str, repositories: Vec<Repository>) -> Vec<SourcesJar> {
    let client = MavenClient::new(config, repositories);
    let mut out = Vec::new();
    for artifact in ["kotlin-stdlib", "kotlin-test"] {
        let module = ModuleId::new("org.jetbrains.kotlin", artifact);
        if let Ok(Some(jar)) = client.fetch_sources(&module, version) {
            out.push(SourcesJar { name: format!("{artifact} {version}"), jar });
        }
    }
    out
}

/// The parsed Kotlin files of a sources jar.
pub fn jar_units(config: &Config, jar: &SourcesJar) -> Result<Vec<Arc<Unit>>> {
    let dir = extract(config, &jar.jar)?;
    let files = kargo_util::fs::collect_files(&dir, &["kt"]);
    Ok(files
        .par_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let rel = path.strip_prefix(&dir).unwrap_or(path);
            let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).unwrap_or_default();
            let jvm = first.starts_with("jvm") || first.starts_with("jdk");
            Some(Arc::new(Unit::new(path.clone(), text, Origin::Library { name: jar.name.clone(), jvm })))
        })
        .collect())
}

/// Where the sources of `jar` are extracted, extracting them the first time.
fn extract(config: &Config, jar: &Path) -> Result<PathBuf> {
    let stem = jar.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sources".into());
    let mut fp = kargo_util::hash::Fingerprint::new();
    fp.file_stamp(jar);
    let hash = fp.finish();
    let dir = config.home().join("cache").join("lsp-sources").join(format!("{stem}-{}", &hash[..12]));
    let marker = dir.join(".kargo-complete");
    if marker.is_file() {
        return Ok(dir);
    }
    let staging = dir.with_extension(format!("tmp-{}", std::process::id()));
    let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
    std::fs::create_dir_all(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    kargo_toolchain::archive::extract_zip(jar, &staging)?;
    std::fs::write(staging.join(".kargo-complete"), "")?;
    if kargo_util::fs::rename_with_retries(&staging, &dir).is_err() {
        // Another `kargo lsp` extracted it first.
        let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
        if !marker.is_file() {
            anyhow::bail!("failed to extract {}", jar.display());
        }
    }
    Ok(dir)
}

/// The built-in outline of the standard library, written out once so go to definition opens it.
pub fn stub_units(config: &Config) -> Vec<Arc<Unit>> {
    let dir = config.home().join("cache").join("lsp-sources").join(format!("kotlin-stdlib-outline-{}", env!("CARGO_PKG_VERSION")));
    stub_files()
        .into_iter()
        .map(|(rel, text)| {
            let path = dir.join(rel);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                let _ = kargo_util::fs::write_string_atomic(&path, &text);
            }
            Arc::new(Unit::new(path, text, Origin::Library { name: "Kotlin standard library outline".into(), jvm: false }))
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

    #[test]
    fn splits_the_outline_into_files() {
        let files = stub_files();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"kotlin/Core.kt") && names.contains(&"kotlin/io/Console.kt"), "{names:?}");
        for (name, text) in &files {
            let unit = Unit::new(PathBuf::from(name), text.clone(), Origin::Library { name: "outline".into(), jvm: false });
            assert!(!unit.file.package.is_empty(), "{name} has a package");
            assert!(!unit.file.top.is_empty(), "{name} has declarations");
        }
    }
}
