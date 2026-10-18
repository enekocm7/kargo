//! A polling file watcher for `--watch`: it remembers what each file looked like at the last check and reports the files
//! whose contents changed since.
//!
//! Files are polled rather than watched with OS notifications, which differ on each platform and miss some editors'
//! saves. A file is only read again when its size or modification time changed, and a rewrite with the same contents (an
//! editor saving an unchanged file, or Kargo writing `kargo.lock` back as it was) is not reported.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::Hasher;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::{DirEntry, WalkDir};

/// How often the watched files are checked.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long the files must stay unchanged after a change is seen before it is reported. The writes of one save (an editor
/// writing a file in several steps, a formatter touching several files) then come out as one change.
pub const SETTLE_TIME: Duration = Duration::from_millis(250);

/// What a file looked like at the last check.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    modified: Option<SystemTime>,
    len: u64,
    digest: u64,
}

/// The files under a set of paths, as they were at the last check.
#[derive(Debug)]
pub struct Watcher {
    root: PathBuf,
    ignore: Gitignore,
    paths: Vec<PathBuf>,
    files: BTreeMap<PathBuf, Seen>,
}

impl Watcher {
    /// Starts watching `paths` as they are now. Directories are walked recursively. Files are left out when `root/.gitignore`
    /// or one of `globs` (in gitignore syntax) matches them, when their name starts with a dot, or when they are under
    /// `root/target`. A path given directly is watched even when a pattern matches it.
    pub fn new(root: &Path, globs: &[String], paths: Vec<PathBuf>) -> Result<Self> {
        let mut builder = GitignoreBuilder::new(root);
        // Lines git itself would reject are left out, as git leaves them out.
        let _ = builder.add(root.join(".gitignore"));
        for glob in globs {
            builder.add_line(None, glob).with_context(|| format!("invalid --ignore pattern `{glob}`"))?;
        }
        let ignore = builder.build().context("failed to read .gitignore")?;
        let mut watcher = Self { root: root.to_path_buf(), ignore, paths: Vec::new(), files: BTreeMap::new() };
        // What is there now is the baseline, so nothing is reported until something changes.
        watcher.update(paths);
        Ok(watcher)
    }

    /// The number of files being watched.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Checks the watched paths once and returns the files that changed since the last check, sorted. A file that was
    /// deleted counts as changed, and so does one that was not watched before, so a file created while a command ran is
    /// not missed.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let paths = self.paths.clone();
        self.update(paths)
    }

    /// Sets the paths to watch and returns the files that changed since the last check, as [`Watcher::poll`] does.
    pub fn update(&mut self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut files = BTreeMap::new();
        let mut changed = Vec::new();
        for path in &paths {
            for file in self.files_under(path) {
                let Some((modified, len)) = stat(&file) else { continue };
                let previous = self.files.get(&file);
                let seen = match previous {
                    Some(seen) if seen.modified == modified && seen.len == len => seen.clone(),
                    _ => {
                        // Unreadable right now, as when another program is still writing it: it is reported once it can be read.
                        let Some(digest) = digest(&file) else { continue };
                        if previous.is_none_or(|seen| seen.digest != digest) {
                            changed.push(file.clone());
                        }
                        Seen { modified, len, digest }
                    }
                };
                files.insert(file, seen);
            }
        }
        // A file that was watched and is gone now, under a path that is still watched.
        for file in self.files.keys() {
            if !files.contains_key(file) && paths.iter().any(|p| file.starts_with(p)) {
                changed.push(file.clone());
            }
        }
        self.files = files;
        self.paths = paths;
        changed.sort();
        changed.dedup();
        changed
    }

    /// Takes the current contents of `path` as what was last seen, so that a write the watched command made to it is not
    /// reported as a change. Used for `kargo.lock`, which the build itself may rewrite.
    pub fn settle(&mut self, path: &Path) {
        match (stat(path), digest(path)) {
            (Some((modified, len)), Some(digest)) => {
                self.files.insert(path.to_path_buf(), Seen { modified, len, digest });
            }
            _ => {
                self.files.remove(path);
            }
        }
    }

    /// Blocks until a watched file changes, then until the changes stop for [`SETTLE_TIME`], and returns the changed files.
    pub fn wait(&mut self) -> Vec<PathBuf> {
        loop {
            thread::sleep(POLL_INTERVAL);
            let mut changed = self.poll();
            if changed.is_empty() {
                continue;
            }
            loop {
                thread::sleep(SETTLE_TIME);
                let more = self.poll();
                if more.is_empty() {
                    break;
                }
                changed.extend(more);
            }
            changed.sort();
            changed.dedup();
            return changed;
        }
    }

    /// The regular files under `path`, leaving out the ignored ones.
    fn files_under(&self, path: &Path) -> Vec<PathBuf> {
        WalkDir::new(path)
            .into_iter()
            .filter_entry(|entry| !self.excluded(entry))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(DirEntry::into_path)
            .collect()
    }

    fn excluded(&self, entry: &DirEntry) -> bool {
        // The paths a caller names are the ones it wants, even when a pattern would match them.
        if entry.depth() == 0 {
            return false;
        }
        if entry.file_name().to_string_lossy().starts_with('.') {
            return true;
        }
        let is_dir = entry.file_type().is_dir();
        if is_dir && entry.path() == self.root.join("target") {
            return true;
        }
        self.ignore.matched_path_or_any_parents(entry.path(), is_dir).is_ignore()
    }
}

/// The modification time and size of `path`, or `None` when it does not exist (or cannot be read).
fn stat(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.modified().ok(), meta.len()))
}

/// A hash of the contents of `path`, or `None` when it cannot be read.
fn digest(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut hasher = DefaultHasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Some(hasher.finish()),
            Ok(n) => hasher.write(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn names(root: &Path, changed: &[PathBuf]) -> Vec<String> {
        changed.iter().map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")).collect()
    }

    #[test]
    fn reports_edits_creations_and_deletions_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/Main.kt"), "fun main() {}");
        let mut watcher = Watcher::new(root, &[], vec![root.join("src")]).unwrap();
        assert_eq!(watcher.file_count(), 1);
        assert!(watcher.poll().is_empty(), "nothing changed yet");

        write(&root.join("src/Main.kt"), "fun main() { println() }");
        assert_eq!(names(root, &watcher.poll()), ["src/Main.kt"]);
        assert!(watcher.poll().is_empty(), "a change is reported once");

        write(&root.join("src/Other.kt"), "class Other");
        assert_eq!(names(root, &watcher.poll()), ["src/Other.kt"]);

        fs::remove_file(root.join("src/Other.kt")).unwrap();
        assert_eq!(names(root, &watcher.poll()), ["src/Other.kt"]);
        assert_eq!(watcher.file_count(), 1);
    }

    #[test]
    fn a_rewrite_with_the_same_contents_is_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("kargo.lock"), "version = 1\n");
        let mut watcher = Watcher::new(root, &[], vec![root.join("kargo.lock")]).unwrap();
        // Same size, later modification time: the content check is what keeps this quiet.
        std::thread::sleep(Duration::from_millis(20));
        write(&root.join("kargo.lock"), "version = 1\n");
        assert!(watcher.poll().is_empty());
    }

    #[test]
    fn leaves_out_hidden_target_gitignored_and_glob_matched_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "generated/\n*.tmp\n");
        write(&root.join("kargo.toml"), "[package]");
        write(&root.join("target/classes/Main.class"), "bytes");
        write(&root.join("src/Main.kt"), "fun main() {}");
        write(&root.join("src/Main.kt~"), "backup");
        write(&root.join("src/target/Thing.kt"), "not the build directory");
        write(&root.join("src/generated/Gen.kt"), "class Gen");
        let mut watcher = Watcher::new(root, &["*.bak".to_owned()], vec![root.to_path_buf()]).unwrap();
        assert_eq!(names(root, &watcher.update(vec![root.to_path_buf()])), Vec::<String>::new());
        assert_eq!(watcher.file_count(), 4, "kargo.toml, Main.kt, Main.kt~ and src/target/Thing.kt");

        write(&root.join("src/.Main.kt.swp"), "swap");
        write(&root.join("src/notes.tmp"), "scratch");
        write(&root.join("src/old.bak"), "backup");
        write(&root.join("src/generated/Gen.kt"), "class Gen again");
        write(&root.join("target/classes/Main.class"), "more bytes");
        assert!(watcher.poll().is_empty(), "only ignored files changed");

        write(&root.join("src/Main.kt"), "fun main() { println() }");
        assert_eq!(names(root, &watcher.poll()), ["src/Main.kt"]);
    }

    #[test]
    fn a_path_named_directly_is_watched_even_when_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "kargo.lock\n");
        write(&root.join("kargo.lock"), "a");
        let mut watcher = Watcher::new(root, &[], vec![root.join("kargo.lock")]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write(&root.join("kargo.lock"), "b");
        assert_eq!(names(root, &watcher.poll()), ["kargo.lock"]);
    }

    #[test]
    fn settle_absorbs_the_writes_of_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lock = root.join("kargo.lock");
        let mut watcher = Watcher::new(root, &[], vec![lock.clone()]).unwrap();
        write(&lock, "resolved");
        watcher.settle(&lock);
        assert!(watcher.update(vec![lock.clone()]).is_empty());
        write(&lock, "edited by hand");
        assert_eq!(names(root, &watcher.poll()), ["kargo.lock"]);
    }

    #[test]
    fn a_file_created_while_a_command_ran_is_reported_by_the_next_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/Main.kt"), "fun main() {}");
        let mut watcher = Watcher::new(root, &[], vec![root.join("src")]).unwrap();
        // The command runs; meanwhile a file appears in a watched directory.
        write(&root.join("src/Late.kt"), "class Late");
        let changed = watcher.update(vec![root.join("src")]);
        assert_eq!(names(root, &changed), ["src/Late.kt"]);
    }

    #[test]
    fn invalid_globs_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        // A reversed range is an error; an unclosed `[` is taken literally, as git does.
        assert!(Watcher::new(dir.path(), &["[z-a]".to_owned()], Vec::new()).is_err());
    }
}
