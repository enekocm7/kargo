//! `--watch` and `kargo watch`: runs a step, then again after every change to the files the project depends on.

use std::path::{Path, PathBuf};

use anyhow::Result;
use kargo_manifest::{MANIFEST_NAME, TargetKind};
use kargo_util::shell::Shell;
use kargo_util::watch::Watcher;

use crate::workspace::Project;

/// The options of a watched run.
#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// Clear the terminal before each run (`--clear`).
    pub clear: bool,
    /// Gitignore-style patterns of files to leave out, on top of `.gitignore` (`--ignore`).
    pub ignore: Vec<String>,
}

/// The files a watched command depends on: the manifests, sources, tests, examples, resources, `android/` and `interop/`
/// directories and C sources of every package, the `kargo.lock` and the `.env` files at each package's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSet {
    /// The project root, whose `.gitignore` decides which files are left out.
    pub root: PathBuf,
    /// The files and directories to watch.
    pub paths: Vec<PathBuf>,
    /// The `kargo.lock` the build may rewrite; see [`Watcher::settle`].
    pub lockfile: PathBuf,
}

impl WatchSet {
    /// The files `project` depends on, as they are now. Directories that do not exist yet are included, so that files added
    /// to them later are seen.
    pub fn of(project: &Project) -> Self {
        let root = project.root().to_path_buf();
        let lockfile = project.lockfile_path();
        let mut paths = vec![root.join(MANIFEST_NAME), lockfile.clone()];
        for package in project.packages() {
            paths.push(package.manifest_path());
            paths.push(package.common_sources());
            paths.extend(TargetKind::ALL.iter().map(|t| package.target_sources(*t)));
            paths.push(package.common_tests());
            paths.extend(TargetKind::ALL.iter().map(|t| package.target_tests(*t)));
            paths.push(package.common_test_fixtures());
            paths.extend(TargetKind::ALL.iter().map(|t| package.target_test_fixtures(*t)));
            paths.push(package.common_benches());
            paths.extend(TargetKind::ALL.iter().map(|t| package.target_benches(*t)));
            paths.push(package.examples_dir());
            paths.push(package.resources());
            paths.push(package.root().join(crate::compose::resources::DIR));
            paths.push(package.test_resources());
            paths.push(package.root().join("android"));
            paths.push(package.root().join("interop"));
            if let Some(native) = &package.manifest().targets.native {
                paths.extend(native.cinterop.iter().map(|c| package.root().join(c.def_path())));
                if let Some(c) = &native.c_sources {
                    paths.extend(c.include_dirs.iter().map(|d| package.root().join(d)));
                    paths.extend(c.sources.iter().map(|s| package.root().join(literal_prefix(s))));
                }
            }
            // What the `[[generate]]` steps read: a change makes the next run generate again.
            for generator in &package.manifest().generators {
                paths.extend(generator.inputs.iter().map(|input| package.root().join(literal_prefix(input))));
            }
            paths.extend(env_files(package.root()));
        }
        paths.sort();
        paths.dedup();
        Self { root, paths, lockfile }
    }
}

/// A glob is watched from the directory before its first wildcard.
fn literal_prefix(glob: &str) -> PathBuf {
    Path::new(glob.trim_start_matches("./"))
        .components()
        .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
        .collect()
}

/// The `.env` files a package's programs read their environment from: `.env`, `.env.<profile>` and their `.local` variants.
/// `.env.example` is only a template, so it is left out.
fn env_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (name == ".env" || name.starts_with(".env.")) && !name.ends_with(".example")
        })
        .collect()
}

/// Runs `step` now, and again after every change to the files of the project it loads, until the process is stopped.
///
/// `step` loads the project afresh, does its work and shows its own errors. It returns the [`WatchSet`] of the project it
/// loaded, or `None` when the project could not be loaded; then the files of the last load stay watched, so fixing the
/// manifest triggers another run.
///
/// Changes made while `step` runs do not wait for the next change: they start another run as soon as this one ends.
pub fn repeatedly(shell: &Shell, options: &WatchOptions, first: WatchSet, mut step: impl FnMut() -> Option<WatchSet>) -> Result<()> {
    let WatchSet { root, paths, .. } = first;
    let mut watcher = Watcher::new(&root, &options.ignore, paths)?;
    shell.status("Watching", format!("{} files for changes (press Ctrl+C to stop)", watcher.file_count()));
    let mut changed = Vec::new();
    loop {
        if options.clear {
            shell.clear();
        }
        if !changed.is_empty() {
            shell.status("Changed", describe(&root, &changed));
        }
        let pending = match step() {
            Some(next) => {
                // The build may have rewritten kargo.lock; that is not a change to answer with another run.
                watcher.settle(&next.lockfile);
                watcher.update(next.paths)
            }
            None => Vec::new(),
        };
        changed = if pending.is_empty() { watcher.wait() } else { pending };
    }
}

/// Names the changed files for the status line: the first one relative to the project, and how many more there are.
fn describe(root: &Path, changed: &[PathBuf]) -> String {
    let name = |path: &PathBuf| path.strip_prefix(root).unwrap_or(path).display().to_string();
    match changed {
        [] => String::new(),
        [only] => name(only),
        [first, rest @ ..] => format!("{} and {} more", name(first), rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_one_or_more_changed_files() {
        let root = Path::new("app");
        let file = |name: &str| root.join("src").join(name);
        assert_eq!(describe(root, &[file("Main.kt")]), Path::new("src").join("Main.kt").display().to_string());
        let changed = [file("A.kt"), file("B.kt"), root.join("kargo.toml")];
        assert_eq!(describe(root, &changed), format!("{} and 2 more", Path::new("src").join("A.kt").display()));
    }

    #[test]
    fn globs_are_watched_from_their_literal_directory() {
        assert_eq!(literal_prefix("proto/**/*.proto"), Path::new("proto"));
        assert_eq!(literal_prefix("./openapi.yaml"), Path::new("openapi.yaml"));
        assert_eq!(literal_prefix("api/v1/*.yaml"), Path::new("api").join("v1"));
        assert_eq!(literal_prefix("**/*.proto"), Path::new(""));
    }

    #[test]
    fn env_files_are_the_dotenv_files_but_not_the_template() {
        let dir = tempfile::tempdir().unwrap();
        for name in [".env", ".env.dev", ".env.local", ".env.example", "README.md", ".gitignore"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let mut found: Vec<String> = env_files(dir.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        found.sort();
        assert_eq!(found, [".env", ".env.dev", ".env.local"]);
    }
}
