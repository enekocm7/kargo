//! Filesystem helpers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Creates a temporary sibling file, lets `write` fill it, then renames it into
/// place so readers never observe a partially written file.
pub fn write_atomic(path: &Path, write: impl FnOnce(&mut fs::File) -> io::Result<()>) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let file_name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.{}.{}.part", std::process::id(), unique_suffix()));
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        write(&mut file).with_context(|| format!("failed to write {}", tmp.display()))?;
        file.flush()?;
        drop(file);
        rename_replacing(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn write_string_atomic(path: &Path, contents: &str) -> Result<()> {
    write_atomic(path, |f| f.write_all(contents.as_bytes()))
}

fn rename_replacing(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        // On Windows a concurrent process may hold or have produced the same
        // file; if it now exists, theirs is as good as ours.
        Err(_) if to.exists() => {
            let _ = fs::remove_file(from);
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("failed to move {} to {}", from.display(), to.display())),
    }
}

/// How many times [`rename_with_retries`] tries again after a rename that failed because a file was in use.
const RENAME_RETRIES: u32 = 6;

/// Renames `from` to `to`, retrying briefly when Windows refuses because a file in the tree is
/// in use. Antivirus and the search indexer hold handles on freshly written files, which fails
/// the rename with "Access is denied" (or a sharing violation) for a moment; rustup retries the
/// same way.
pub fn rename_with_retries(from: &Path, to: &Path) -> io::Result<()> {
    let mut retry = 0;
    loop {
        match fs::rename(from, to) {
            // Once the destination exists the rename is hopeless: the caller decides what that means.
            Err(e) if retry < RENAME_RETRIES && !to.exists() && is_in_use(&e) => {
                std::thread::sleep(Duration::from_millis(100 << retry));
                retry += 1;
            }
            result => return result,
        }
    }
}

/// Whether a failed rename means a file in the tree is in use: `ERROR_ACCESS_DENIED` (5) or
/// `ERROR_SHARING_VIOLATION` (32). Other platforms don't refuse renames for this reason.
fn is_in_use(err: &io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32))
}

fn unique_suffix() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    nanos ^ u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Recursively collects files under `root` whose extension is in `extensions`,
/// sorted for deterministic compiler invocations.
pub fn collect_files(root: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.contains(&e)))
        .collect();
    files.sort();
    files
}

/// Copies a directory tree, creating destination directories as needed.
pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(from)?;
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &dest).with_context(|| format!("failed to copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Removes a directory tree if it exists.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// Joins classpath entries with the platform separator.
pub fn join_paths(paths: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(sep)
}

/// Canonicalizes without Windows' `\\?\` prefix, which confuses many tools.
pub fn dunce_canonicalize(path: &Path) -> PathBuf {
    let Ok(canonical) = path.canonicalize() else { return path.to_path_buf() };
    #[cfg(windows)]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\")
            && !stripped.starts_with("UNC\\")
        {
            return PathBuf::from(stripped);
        }
    }
    canonical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parents_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");
        write_string_atomic(&path, "one").unwrap();
        write_string_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary files left behind");
    }

    #[test]
    fn failed_write_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        let err = write_atomic(&path, |_| Err(io::Error::other("boom")));
        assert!(err.is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn collects_sorted_files_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["b.kt", "a/c.kt", "a/d.java", "e.txt"] {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let files = collect_files(dir.path(), &["kt", "java"]);
        let names: Vec<_> = files.iter().map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(names, ["a/c.kt", "a/d.java", "b.kt"]);
        assert!(collect_files(&dir.path().join("missing"), &["kt"]).is_empty());
    }

    #[test]
    fn copies_trees() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("x")).unwrap();
        fs::write(src.join("x/y.txt"), "hi").unwrap();
        copy_dir(&src, &dir.path().join("dst")).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("dst/x/y.txt")).unwrap(), "hi");
        remove_dir_all_if_exists(&dir.path().join("dst")).unwrap();
        remove_dir_all_if_exists(&dir.path().join("dst")).unwrap();
        assert!(!dir.path().join("dst").exists());
    }

    #[test]
    fn joins_classpaths() {
        let joined = join_paths(&[PathBuf::from("a.jar"), PathBuf::from("b.jar")]);
        assert!(joined == "a.jar;b.jar" || joined == "a.jar:b.jar");
        assert_eq!(join_paths(&[]), "");
    }

    #[test]
    fn renames_directory_trees() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("from/sub")).unwrap();
        fs::write(dir.path().join("from/sub/f"), "x").unwrap();
        rename_with_retries(&dir.path().join("from"), &dir.path().join("to")).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("to/sub/f")).unwrap(), "x");
        assert!(!dir.path().join("from").exists());
        // A missing source is not in use, so it fails at once rather than after the retries.
        assert!(rename_with_retries(&dir.path().join("from"), &dir.path().join("other")).is_err());
    }

    /// Windows refuses to rename a directory while a file in it is open without sharing deletes,
    /// the situation antivirus creates; the rename goes through once the handle is released.
    #[cfg(windows)]
    #[test]
    fn retries_a_rename_while_a_file_is_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("busy"), "x").unwrap();
        let busy = fs::File::open(from.join("busy")).unwrap();
        assert!(fs::rename(&from, dir.path().join("plain")).is_err(), "the open file should block a plain rename");

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(busy);
        });
        rename_with_retries(&from, &dir.path().join("to")).unwrap();
        releaser.join().unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("to/busy")).unwrap(), "x");
    }
}
