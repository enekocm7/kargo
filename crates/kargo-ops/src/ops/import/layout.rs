//! Moving the sources of a project into Kargo's layout: planning every file move before any file
//! moves, and doing them with `git mv` inside a git repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use super::model::DirMove;

/// One file that moves, both paths relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Expands the directory moves into file moves, merging directories that several moves share (the
/// `com/` of `src/main/kotlin` and of `src/main/java`). Fails, before anything has moved, when a file
/// would land on one that exists or on another move's destination.
pub fn plan(root: &Path, moves: &[DirMove]) -> Result<Vec<FileMove>> {
    let mut files: Vec<FileMove> = Vec::new();
    for dir_move in moves {
        let from = root.join(&dir_move.from);
        // A move can also name a single file, such as `AndroidManifest.xml`, and then `to` is the new path of the file.
        if from.is_file() {
            files.push(FileMove { from: dir_move.from.clone(), to: dir_move.to.clone() });
            continue;
        }
        if !from.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&from).sort_by_file_name() {
            let entry = entry.with_context(|| format!("failed to read {}", from.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&from).expect("walkdir yields paths below its root");
            files.push(FileMove { from: dir_move.from.join(rel), to: dir_move.to.join(rel) });
        }
    }
    files.retain(|m| m.from != m.to);

    let mut destinations: BTreeMap<&Path, &Path> = BTreeMap::new();
    for m in &files {
        if let Some(other) = destinations.insert(&m.to, &m.from) {
            bail!(
                "{} and {} would both become {}; move one of them by hand and run `kargo import` again",
                other.display(),
                m.from.display(),
                m.to.display()
            );
        }
        if root.join(&m.to).exists() {
            bail!("{} would overwrite {}, which exists; move or delete it and run `kargo import` again", m.from.display(), m.to.display());
        }
    }
    // A directory of the old layout cannot also be a file of the new one.
    for m in &files {
        if m.to.ancestors().skip(1).any(|dir| destinations.contains_key(dir)) {
            bail!(
                "{} would need {} to be a directory, but a file moves there",
                m.from.display(),
                m.to.parent().unwrap_or(Path::new("")).display()
            );
        }
    }
    Ok(files)
}

/// Whether `root` is inside a git work tree and `git` can be run.
pub fn in_git_repository(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
}

/// How many files one `git mv` command moves.
const GIT_BATCH: usize = 64;

/// Performs the moves, then removes the directories they left empty. Uses `git mv` (so the renames
/// are staged) when `git` is set; files git does not track are renamed on disk.
pub fn apply(root: &Path, moves: &[FileMove], git: bool) -> Result<()> {
    let mut by_directory: BTreeMap<&Path, Vec<&FileMove>> = BTreeMap::new();
    for m in moves {
        by_directory.entry(m.to.parent().unwrap_or(Path::new(""))).or_default().push(m);
    }
    for (directory, group) in by_directory {
        let target = root.join(directory);
        std::fs::create_dir_all(&target).with_context(|| format!("failed to create {}", target.display()))?;
        if git {
            // `git mv <files> <directory>` keeps the names, which is all these moves do.
            let same_name: Vec<&&FileMove> = group.iter().filter(|m| m.from.file_name() == m.to.file_name()).collect();
            for batch in same_name.chunks(GIT_BATCH) {
                // `-k` skips what git does not track; those files are renamed below.
                let _ = Command::new("git")
                    .args(["mv", "-k"])
                    .args(batch.iter().map(|m| &m.from))
                    .arg(directory)
                    .current_dir(root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        for m in group {
            let from = root.join(&m.from);
            if from.exists() {
                std::fs::rename(&from, root.join(&m.to))
                    .with_context(|| format!("failed to move {} to {}", m.from.display(), m.to.display()))?;
            }
        }
    }
    for m in moves {
        remove_empty_ancestors(root, &m.from);
    }
    Ok(())
}

/// Removes the directories above `file` that are empty now, up to (not including) the root.
fn remove_empty_ancestors(root: &Path, file: &Path) {
    for dir in file.ancestors().skip(1) {
        if dir.as_os_str().is_empty() || std::fs::remove_dir(root.join(dir)).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rel).unwrap();
    }

    fn dir_move(from: &str, to: &str) -> DirMove {
        DirMove { from: PathBuf::from(from), to: PathBuf::from(to) }
    }

    #[test]
    fn merges_directories_that_share_packages() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main/kotlin/com/acme/A.kt");
        write(dir.path(), "src/main/java/com/acme/B.java");
        write(dir.path(), "src/test/kotlin/com/acme/ATest.kt");
        let moves = [dir_move("src/main/kotlin", "src"), dir_move("src/main/java", "src"), dir_move("src/test/kotlin", "tests")];
        let planned = plan(dir.path(), &moves).unwrap();
        assert_eq!(planned.len(), 3);
        apply(dir.path(), &planned, false).unwrap();
        assert!(dir.path().join("src/com/acme/A.kt").is_file());
        assert!(dir.path().join("src/com/acme/B.java").is_file());
        assert!(dir.path().join("tests/com/acme/ATest.kt").is_file());
        assert!(!dir.path().join("src/main").exists(), "the emptied directories are removed");
        assert!(!dir.path().join("src/test").exists());
    }

    #[test]
    fn refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main/kotlin/A.kt");
        write(dir.path(), "src/main/java/A.kt");
        let err = plan(dir.path(), &[dir_move("src/main/kotlin", "src"), dir_move("src/main/java", "src")]).unwrap_err();
        assert!(err.to_string().contains("would both become src/A.kt") || err.to_string().contains("would both become src\\A.kt"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main/kotlin/A.kt");
        write(dir.path(), "tests/A.kt");
        let err = plan(dir.path(), &[dir_move("src/main/kotlin", "tests")]).unwrap_err();
        assert!(err.to_string().contains("exists"), "{err}");
    }

    #[test]
    fn a_file_cannot_take_the_place_of_the_old_layout() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main/kotlin/main");
        // `src/main` is a directory of the old layout and a file of the new one.
        let err = plan(dir.path(), &[dir_move("src/main/kotlin", "src")]).unwrap_err();
        assert!(err.to_string().contains("exists"), "{err}");
    }
}
