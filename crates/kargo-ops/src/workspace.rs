//! A loaded project: its manifest and on-disk layout.
//!
//! ```text
//! kargo.toml
//! kargo.lock
//! src/                 common sources (Kotlin, and Java for JVM)
//! src-<target>/        target-specific sources (src-jvm, src-wasm, src-native, src-android)
//! tests/               common tests
//! tests-<target>/      target-specific tests
//! resources/           JVM resources
//! test-resources/      JVM test resources
//! target/              build output
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use kargo_manifest::{LOCKFILE_NAME, MANIFEST_NAME, Manifest, Profile, TargetKind};

/// Environment variable overriding the build output directory.
pub const TARGET_DIR_ENV: &str = "KARGO_TARGET_DIR";

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    manifest: Manifest,
    target_dir: PathBuf,
}

/// Build profile selected on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileKind {
    #[default]
    Dev,
    Release,
}

impl ProfileKind {
    pub fn dir_name(self) -> &'static str {
        match self {
            ProfileKind::Dev => "debug",
            ProfileKind::Release => "release",
        }
    }
}

impl Workspace {
    /// Finds `kargo.toml` in `start` or any parent directory.
    pub fn find_manifest(start: &Path) -> Result<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            let candidate = d.join(MANIFEST_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
            dir = d.parent();
        }
        bail!("could not find `{MANIFEST_NAME}` in `{}` or any parent directory", start.display())
    }

    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest = Manifest::load(manifest_path)?;
        let root = manifest_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let root = dunce_canonicalize(&root);
        let target_dir = match std::env::var_os(TARGET_DIR_ENV) {
            Some(dir) if !dir.is_empty() => {
                let dir = PathBuf::from(dir);
                if dir.is_absolute() { dir } else { root.join(dir) }
            }
            _ => root.join("target"),
        };
        Ok(Self { root, manifest, target_dir })
    }

    /// Loads the workspace containing `cwd` (or the explicit manifest path).
    pub fn discover(cwd: &Path, manifest_path: Option<&Path>) -> Result<Self> {
        let path = match manifest_path {
            Some(p) => {
                if !p.is_file() {
                    bail!("manifest path `{}` does not exist", p.display());
                }
                p.to_path_buf()
            }
            None => Self::find_manifest(cwd)?,
        };
        Self::load(&path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_NAME)
    }

    pub fn lockfile_path(&self) -> PathBuf {
        self.root.join(LOCKFILE_NAME)
    }

    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }

    pub fn profile(&self, kind: ProfileKind) -> &Profile {
        match kind {
            ProfileKind::Dev => &self.manifest.dev_profile,
            ProfileKind::Release => &self.manifest.release_profile,
        }
    }

    /// Output directory for one target and profile, e.g. `target/debug/jvm`.
    pub fn output_dir(&self, profile: ProfileKind, target: TargetKind) -> PathBuf {
        self.target_dir.join(profile.dir_name()).join(target.as_str())
    }

    /// Common source directory.
    pub fn common_sources(&self) -> PathBuf {
        self.root.join("src")
    }

    /// Target-specific source directory.
    pub fn target_sources(&self, target: TargetKind) -> PathBuf {
        self.root.join(target.source_dir_name())
    }

    pub fn common_tests(&self) -> PathBuf {
        self.root.join("tests")
    }

    pub fn target_tests(&self, target: TargetKind) -> PathBuf {
        self.root.join(format!("tests-{}", target.as_str()))
    }

    /// Every existing source and test directory (`src/`, `src-<target>/`, `tests/`,
    /// `tests-<target>/`) for any target, enabled or not, relative to the root.
    /// Used by `kargo fmt` and `kargo lint`, which never look inside `target/`.
    pub fn source_roots(&self) -> Vec<PathBuf> {
        let mut dirs = vec![PathBuf::from("src")];
        dirs.extend(TargetKind::ALL.iter().map(|t| PathBuf::from(t.source_dir_name())));
        dirs.push(PathBuf::from("tests"));
        dirs.extend(TargetKind::ALL.iter().map(|t| PathBuf::from(format!("tests-{}", t.as_str()))));
        dirs.into_iter().filter(|d| self.root.join(d).is_dir()).collect()
    }

    pub fn resources(&self) -> PathBuf {
        self.root.join("resources")
    }

    pub fn test_resources(&self) -> PathBuf {
        self.root.join("test-resources")
    }
}

/// Canonicalizes without Windows' `\\?\` prefix, which confuses many tools.
pub fn dunce_canonicalize(path: &Path) -> PathBuf {
    let Ok(canonical) = path.canonicalize() else { return path.to_path_buf() };
    #[cfg(windows)]
    {
        let s = canonical.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            if !stripped.starts_with("UNC\\") {
                return PathBuf::from(stripped);
            }
        }
    }
    canonical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_manifest_in_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_NAME), "[package]\nname = \"w\"\nversion = \"0.1.0\"\n").unwrap();
        let nested = dir.path().join("src/a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let ws = Workspace::discover(&nested, None).unwrap();
        assert_eq!(ws.name(), "w");
        assert_eq!(ws.root(), dunce_canonicalize(dir.path()));
        assert!(ws.output_dir(ProfileKind::Release, TargetKind::Jvm).ends_with("target/release/jvm"));
        assert!(ws.target_sources(TargetKind::Native).ends_with("src-native"));
        assert!(ws.target_tests(TargetKind::Jvm).ends_with("tests-jvm"));
        assert!(!ws.root().to_string_lossy().starts_with(r"\\?\"));
        std::fs::create_dir_all(dir.path().join("tests-jvm")).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        assert_eq!(ws.source_roots(), [PathBuf::from("src"), PathBuf::from("tests-jvm")]);
    }

    #[test]
    fn missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = Workspace::discover(dir.path(), None).unwrap_err();
        assert!(err.to_string().contains("could not find"));
        assert!(Workspace::discover(dir.path(), Some(&dir.path().join("nope.toml"))).is_err());
    }
}
