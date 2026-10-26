//! Incremental Kotlin/JVM compilation: only the sources that changed, plus whatever
//! depends on them, are recompiled. It runs in the compiler daemon through the Kotlin
//! Build Tools API (`daemon/KargoIc.java`), the same incremental compiler Gradle uses.
//!
//! Kargo tells the compiler which sources changed since the last successful build
//! (from size and modification-time stamps kept in the unit's state directory), and
//! hands it a snapshot of each classpath entry so that dependency changes invalidate
//! only what uses them. Anything else that changed (compiler options, plugins, the
//! compiler version) starts over from a clean build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::daemon::{self, Tool};
use kargo_toolchain::Jdk;
use kargo_util::fs::{remove_dir_all_if_exists, write_string_atomic};
use kargo_util::hash::Fingerprint;
use kargo_util::shell::Shell;

/// Bumped when the format of the state below changes, which forces one clean build.
const STATE_VERSION: &str = "1";

/// One compilation unit's incremental state lives in `state_dir`:
/// `caches/` (the compiler's), `snapshots/` (classpath directories), `sources` and `config`.
pub struct IncrementalUnit<'a> {
    pub state_dir: PathBuf,
    pub output: &'a Path,
    /// Kotlin and Java sources.
    pub sources: Vec<PathBuf>,
    pub classpath: &'a [PathBuf],
    /// Compiler arguments without `-d` and the source files.
    pub options: Vec<String>,
    /// Fingerprint of every other input that requires a clean build when it changes.
    pub config: String,
    /// The Kotlin version, whose classpath snapshots can't be shared with other versions.
    pub compiler_version: &'a str,
}

/// What an incremental compilation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcOutcome {
    /// Compiled successfully; the output directory is up to date.
    Compiled,
    /// The compiler reported errors (already printed).
    Errors,
    /// Incremental compilation is not available here; compile from scratch instead.
    Unavailable,
}

impl IncrementalUnit<'_> {
    /// Compiles in the daemon, falling back to [`IcOutcome::Unavailable`] when the daemon or
    /// the Build Tools API can't be used.
    pub fn compile(&self, shell: &Shell, jdk: &Jdk, compiler_classpath: &[PathBuf]) -> Result<IcOutcome> {
        if !enabled() {
            return Ok(IcOutcome::Unavailable);
        }
        let Some(snapshot_cache) = daemon::snapshot_cache_dir() else { return Ok(IcOutcome::Unavailable) };
        if !daemon::supports_incremental(compiler_classpath) {
            return Ok(IcOutcome::Unavailable);
        }

        let config_path = self.state_dir.join("config");
        let sources_path = self.state_dir.join("sources");
        let config = format!("{STATE_VERSION}\n{}", self.config);
        let output_missing = std::fs::read_dir(self.output).map_or(true, |mut d| d.next().is_none());
        let same_config = std::fs::read_to_string(&config_path).is_ok_and(|c| c == config);
        let previous = if same_config && !output_missing { read_stamps(&sources_path) } else { None };
        if previous.is_none() {
            // Start over: stale caches must not survive a configuration change. The daemon empties
            // `caches/` itself (for `--unknown-changes`), once a compilation of this unit that an
            // earlier, cancelled build left running there has closed them.
            remove_file_if_exists(&sources_path)?;
            remove_file_if_exists(&config_path)?;
        }
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::create_dir_all(self.output)?;

        let current = stamps(&self.sources);
        let mut args: Vec<String> = vec![
            "--working-dir".into(),
            self.state_dir.join("caches").to_string_lossy().into_owned(),
            "--dest".into(),
            self.output.to_string_lossy().into_owned(),
        ];
        for s in &self.sources {
            args.push("--source".into());
            args.push(s.to_string_lossy().into_owned());
        }
        match &previous {
            None => args.push("--unknown-changes".into()),
            Some(previous) => {
                let (modified, removed) = changes(previous, &current);
                for m in modified {
                    args.push("--modified".into());
                    args.push(m.to_owned());
                }
                for r in removed {
                    args.push("--removed".into());
                    args.push(r.to_owned());
                }
            }
        }
        for (entry, snapshot, granularity) in self.snapshots(&snapshot_cache)? {
            args.extend(["--snapshot".to_owned(), entry, snapshot, granularity.to_owned()]);
        }
        args.push("--".into());
        args.extend(self.options.iter().cloned());

        match daemon::run(shell, jdk, compiler_classpath, Tool::KotlinIncremental, &args) {
            None => Ok(IcOutcome::Unavailable),
            Some(0) => {
                write_string_atomic(&sources_path, &render_stamps(&current))?;
                write_string_atomic(&config_path, &config)?;
                Ok(IcOutcome::Compiled)
            }
            Some(daemon::IC_FAILED) => {
                shell.verbose("Rebuilding", "incremental compilation failed (see the daemon log); compiling everything");
                remove_dir_all_if_exists(&self.state_dir)?;
                Ok(IcOutcome::Unavailable)
            }
            Some(_) => Ok(IcOutcome::Errors),
        }
    }

    /// `(classpath entry, snapshot file, granularity)` for each existing classpath entry.
    /// Jars are snapshotted once per file version into a shared cache; directories (the main
    /// classes of a test compilation) per content into this unit's state.
    fn snapshots(&self, cache: &Path) -> Result<Vec<(String, String, &'static str)>> {
        let dir_snapshots = self.state_dir.join("snapshots");
        let mut keep = Vec::new();
        let mut out = Vec::new();
        for entry in self.classpath {
            let Ok(meta) = std::fs::metadata(entry) else { continue };
            let stamp = Fingerprint::new().str(self.compiler_version).file_stamp(entry).finish();
            let (file, granularity) = if meta.is_dir() {
                let name = format!("{}.bin", &stamp[..24]);
                keep.push(name.clone());
                (dir_snapshots.join(name), "member")
            } else {
                (cache.join(format!("{}.bin", &stamp[..32])), "class")
            };
            out.push((entry.to_string_lossy().into_owned(), file.to_string_lossy().into_owned(), granularity));
        }
        // Snapshots of earlier versions of the directories are never used again.
        if let Ok(entries) = std::fs::read_dir(&dir_snapshots) {
            for e in entries.flatten() {
                if !keep.iter().any(|k| *k == e.file_name().to_string_lossy()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(out)
    }
}

/// Environment variable that turns incremental compilation off when set to `false`, `0`, `no`
/// or `off` (like Cargo's `CARGO_INCREMENTAL`); targets then recompile from scratch.
pub const INCREMENTAL_ENV: &str = "KARGO_INCREMENTAL";

fn enabled() -> bool {
    std::env::var(INCREMENTAL_ENV).map_or(true, |v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off"))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("failed to remove {}", path.display())),
        _ => Ok(()),
    }
}

/// `path -> "size mtime"` for every source.
fn stamps(sources: &[PathBuf]) -> BTreeMap<String, String> {
    sources
        .iter()
        .map(|p| {
            let stamp = std::fs::metadata(p)
                .map(|m| {
                    let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
                    format!("{} {mtime}", m.len())
                })
                .unwrap_or_default();
            (p.to_string_lossy().into_owned(), stamp)
        })
        .collect()
}

fn render_stamps(stamps: &BTreeMap<String, String>) -> String {
    stamps.iter().map(|(path, stamp)| format!("{stamp}\t{path}\n")).collect()
}

fn read_stamps(path: &Path) -> Option<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().map(|l| l.split_once('\t').map(|(stamp, path)| (path.to_owned(), stamp.to_owned()))).collect()
}

/// Sources that are new or changed, and sources that are gone.
fn changes<'a>(previous: &'a BTreeMap<String, String>, current: &'a BTreeMap<String, String>) -> (Vec<&'a str>, Vec<&'a str>) {
    let modified = current.iter().filter(|(p, s)| previous.get(*p) != Some(s)).map(|(p, _)| p.as_str()).collect();
    let removed = previous.keys().filter(|p| !current.contains_key(*p)).map(String::as_str).collect();
    (modified, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_modified_added_and_removed_sources() {
        let map = |entries: &[(&str, &str)]| entries.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect::<BTreeMap<_, _>>();
        let previous = map(&[("a.kt", "1 1"), ("b.kt", "2 2"), ("c.kt", "3 3")]);
        let current = map(&[("a.kt", "1 1"), ("b.kt", "2 9"), ("d.kt", "4 4")]);
        let (modified, removed) = changes(&previous, &current);
        assert_eq!(modified, ["b.kt", "d.kt"]);
        assert_eq!(removed, ["c.kt"]);
    }

    #[test]
    fn stamps_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("A.kt");
        std::fs::write(&file, "class A").unwrap();
        let current = stamps(std::slice::from_ref(&file));
        let path = dir.path().join("sources");
        std::fs::write(&path, render_stamps(&current)).unwrap();
        assert_eq!(read_stamps(&path).unwrap(), current);
        assert!(read_stamps(&dir.path().join("missing")).is_none());
    }
}
