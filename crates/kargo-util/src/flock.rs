//! Advisory file locks that keep concurrent Kargo processes from writing the
//! same build directory at the same time.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::shell::Shell;

/// Set for processes Kargo starts while it holds a lock (hooks, for example) to
/// the path of that lock, so a nested `kargo` call does not wait on its parent.
pub const HELD_LOCK_ENV: &str = "KARGO_BUILD_LOCK";

/// An exclusive lock on a file, released when dropped.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    // `None` when a parent process already holds this lock.
    _file: Option<File>,
}

impl FileLock {
    /// Locks `path` (creating it and its parent directories), printing a
    /// `Blocking` status naming `what` while another process holds it.
    pub fn exclusive(path: &Path, shell: &Shell, what: &str) -> Result<FileLock> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        if held_by_parent(path) {
            return Ok(FileLock { path: path.to_path_buf(), _file: None });
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                shell.status("Blocking", format!("waiting for file lock on {what}"));
                file.lock().with_context(|| format!("failed to lock {}", path.display()))?;
            }
            Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("failed to lock {}", path.display())),
        }
        Ok(FileLock { path: path.to_path_buf(), _file: Some(file) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn held_by_parent(path: &Path) -> bool {
    let Some(held) = std::env::var_os(HELD_LOCK_ENV) else {
        return false;
    };
    let held = PathBuf::from(held);
    match (fs::canonicalize(&held), fs::canonicalize(path)) {
        (Ok(a), Ok(b)) => a == b,
        _ => held == path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Verbosity;

    #[test]
    fn second_lock_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/.lock");
        let shell = Shell::new(Verbosity::Quiet);
        let first = FileLock::exclusive(&path, &shell, "test").unwrap();
        assert!(path.is_file());

        let other = File::options().read(true).write(true).open(&path).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        drop(first);
        other.try_lock().unwrap();
    }
}
