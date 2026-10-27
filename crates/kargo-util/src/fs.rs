//! Filesystem helpers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Creates a temporary sibling file, lets `write` fill it, then renames it over
/// `path` so readers never observe a partially written file. A file that can't be
/// replaced is an error: the content the caller wrote must not be silently dropped.
pub fn write_atomic(path: &Path, write: impl FnOnce(&mut fs::File) -> io::Result<()>) -> Result<()> {
    write_via_temp(path, write, |tmp| rename_with_retries(tmp, path))
}

/// Like [`write_atomic`], for content addressed by its name, such as a download cache entry.
/// If the rename fails because another process has already put a file at `path`, that file is
/// kept: it holds the same content, so the write has still succeeded.
pub fn write_atomic_if_absent(path: &Path, write: impl FnOnce(&mut fs::File) -> io::Result<()>) -> Result<()> {
    write_via_temp(path, write, |tmp| match rename_with_retries(tmp, path) {
        Err(_) if path.exists() => {
            let _ = fs::remove_file(tmp);
            Ok(())
        }
        result => result,
    })
}

pub fn write_string_atomic(path: &Path, contents: &str) -> Result<()> {
    write_atomic(path, |f| f.write_all(contents.as_bytes()))
}

/// Fills a temporary sibling of `path` with `write`, then hands it to `place` to move into position.
/// The temporary file is removed when anything fails.
fn write_via_temp(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
    place: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let file_name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.{}.{}.part", std::process::id(), unique_suffix()));
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        write(&mut file).with_context(|| format!("failed to write {}", tmp.display()))?;
        file.flush()?;
        drop(file);
        place(&tmp).with_context(|| format!("failed to move {} to {}", tmp.display(), path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// How many times [`rename_with_retries`] tries again after a rename that failed because a file was in use.
const RENAME_RETRIES: u32 = 6;

/// Renames `from` to `to`, retrying briefly when Windows refuses because a file in the tree is
/// in use. Antivirus and the search indexer hold handles on freshly written files, which fails
/// the rename with "Access is denied" (or a sharing violation) for a moment; rustup retries the
/// same way. A destination another program holds open is retried too, then returned as an error.
pub fn rename_with_retries(from: &Path, to: &Path) -> io::Result<()> {
    let mut retry = 0;
    loop {
        match fs::rename(from, to) {
            // A directory in the way is never replaced by a rename, so waiting won't help: the caller decides.
            Err(e) if retry < RENAME_RETRIES && !to.is_dir() && is_in_use(&e) => {
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

/// The bytes free for this user on the file system holding `path` (the nearest existing ancestor
/// of it), if the platform tells.
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    free_space_imp::free_space(existing)
}

#[cfg(windows)]
mod free_space_imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }

    pub(super) fn free_space(path: &Path) -> Option<u64> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path; the call writes only to `free`, the other outputs may be null.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) } != 0;
        ok.then_some(free)
    }
}

#[cfg(not(windows))]
mod free_space_imp {
    use std::path::Path;
    use std::process::Command;

    /// Asks POSIX `df`, whose `-P -k` output is the same on Linux and macOS.
    pub(super) fn free_space(path: &Path) -> Option<u64> {
        let output = Command::new("df").args(["-P", "-k"]).arg(path).output().ok()?;
        super::df_available(&String::from_utf8_lossy(&output.stdout))
    }
}

/// The `Available` column, in bytes, of the output of `df -P -k`.
#[cfg_attr(any(windows, not(test)), allow(dead_code))]
fn df_available(output: &str) -> Option<u64> {
    let line = output.lines().nth(1)?;
    let kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_df_output() {
        let out = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n/dev/sda1        102400000  51200000  40960000      56% /\n";
        assert_eq!(df_available(out), Some(40960000 * 1024));
        assert_eq!(df_available(""), None);
    }

    #[test]
    fn free_space_of_a_missing_path_uses_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(&dir.path().join("not/there")).is_some_and(|b| b > 0));
    }

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
    fn atomic_write_reports_a_destination_it_cannot_replace() {
        let dir = tempfile::tempdir().unwrap();
        // A directory in the way makes the rename fail on every platform. The write must fail
        // with the path, not report success while the old content stays.
        let path = dir.path().join("kargo.toml");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), "").unwrap();
        let err = write_string_atomic(&path, "new").unwrap_err();
        assert!(format!("{err:#}").contains(&path.display().to_string()), "{err:#}");
        assert!(path.join("inside").is_file());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "the temporary file is removed");
    }

    #[test]
    fn cache_writes_keep_a_destination_another_writer_filled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache/entry.jar");
        write_atomic_if_absent(&path, |f| f.write_all(b"one")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");

        // A directory in the way stands in for a file another process renamed into place first:
        // the rename fails, and the write still succeeds because that file holds the same content.
        let taken = dir.path().join("cache/taken.jar");
        fs::create_dir(&taken).unwrap();
        write_atomic_if_absent(&taken, |f| f.write_all(b"two")).unwrap();
        assert!(taken.is_dir());
        assert_eq!(fs::read_dir(dir.path().join("cache")).unwrap().count(), 2, "no temporary files left behind");
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

    /// An editor or antivirus that holds `kargo.toml` open without sharing deletes used to make the
    /// write report success while the old file stayed; the replacement now waits for the handle.
    #[cfg(windows)]
    #[test]
    fn atomic_write_waits_for_a_destination_that_is_in_use() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kargo.toml");
        fs::write(&path, "old").unwrap();
        let busy = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(busy);
        });
        write_string_atomic(&path, "new").unwrap();
        releaser.join().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }
}
