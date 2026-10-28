//! Advisory file locks that keep concurrent Kargo processes from writing the
//! same build directory at the same time.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::shell::Shell;

/// Set for processes Kargo starts while it holds a lock (hooks, for example) to
/// the path of that lock, so a nested `kargo` call does not wait on its parent.
pub const HELD_LOCK_ENV: &str = "KARGO_BUILD_LOCK";

/// The Kargo command this process runs, for [`Holder`].
static COMMAND: OnceLock<String> = OnceLock::new();

/// Records the Kargo command this process runs (`run`, `build`, ...), so a lock it holds can say who holds it.
pub fn set_command(name: &str) {
    let _ = COMMAND.set(name.to_owned());
}

/// An exclusive lock on a file, released when dropped.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    // `None` when a parent process already holds this lock.
    _file: Option<File>,
    // Whether this lock wrote `holder_path(path)`, which it removes again when dropped.
    recorded: bool,
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
            return Ok(FileLock { path: path.to_path_buf(), _file: None, recorded: false });
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                shell.status("Blocking", format!("waiting for file lock on {what}"));
                file.lock().with_context(|| format!("failed to lock {}", path.display()))?;
            }
            Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("failed to lock {}", path.display())),
        }
        Ok(FileLock { path: path.to_path_buf(), _file: Some(file), recorded: false })
    }

    /// Like [`FileLock::exclusive`], and writes which process holds the lock to [`holder_path`], so [`holder`] can name it
    /// (the lock file itself cannot be read while it is locked on Windows).
    pub fn exclusive_recorded(path: &Path, shell: &Shell, what: &str) -> Result<FileLock> {
        let mut lock = FileLock::exclusive(path, shell, what)?;
        if lock._file.is_some() {
            let command = COMMAND.get().map(String::as_str).unwrap_or_default();
            // Only an aid to `kargo doctor`; a build must not fail because it cannot be written.
            lock.recorded = fs::write(holder_path(path), format!("{}\n{command}\n", std::process::id())).is_ok();
        }
        Ok(lock)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Still locked here: a process that locks next writes its own record after this one is gone.
        if self.recorded {
            let _ = fs::remove_file(holder_path(&self.path));
        }
    }
}

/// The file next to the lock `path` that names the process holding it.
pub fn holder_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".holder");
    PathBuf::from(name)
}

/// The process that holds a lock taken with [`FileLock::exclusive_recorded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// The Kargo command it runs (`run`), when it said.
    pub command: Option<String>,
}

impl fmt::Display for Holder {
    /// `kargo run` (process 1234)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.command {
            Some(command) => write!(f, "`kargo {command}` (process {})", self.pid),
            None => write!(f, "`kargo` (process {})", self.pid),
        }
    }
}

/// Who holds the lock on `path`, from the record its holder wrote. Only meaningful while [`is_held`] is true.
pub fn holder(path: &Path) -> Option<Holder> {
    let text = fs::read_to_string(holder_path(path)).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let command = lines.next().map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned);
    Some(Holder { pid, command })
}

/// Whether another process holds the lock on `path` now. A missing lock file is not held. The lock
/// goes away with the process that held it, so a held lock means a Kargo process is still running.
pub fn is_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else { return false };
    matches!(file.try_lock(), Err(TryLockError::WouldBlock))
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

    #[test]
    fn a_recorded_lock_names_its_holder_until_it_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        assert_eq!(holder(&path), None);
        let lock = FileLock::exclusive_recorded(&path, &Shell::new(Verbosity::Quiet), "test").unwrap();
        let held = holder(&path).unwrap();
        assert_eq!(held.pid, std::process::id());
        assert!(held.to_string().contains(&std::process::id().to_string()));
        drop(lock);
        assert_eq!(holder(&path), None);
        assert!(!holder_path(&path).exists());
    }

    #[test]
    fn holder_text_names_the_command() {
        let with = Holder { pid: 7, command: Some("run".to_owned()) };
        assert_eq!(with.to_string(), "`kargo run` (process 7)");
        assert_eq!(Holder { pid: 7, command: None }.to_string(), "`kargo` (process 7)");
    }

    #[test]
    fn is_held_while_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        assert!(!is_held(&path));
        let lock = FileLock::exclusive(&path, &Shell::new(Verbosity::Quiet), "test").unwrap();
        assert!(is_held(&path));
        drop(lock);
        assert!(!is_held(&path));
    }
}
