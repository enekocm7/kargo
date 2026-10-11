//! Global, per-invocation configuration (home directory, network mode, output).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::shell::Shell;

/// Environment variable overriding the Kargo home directory (default `~/.kargo`).
pub const HOME_ENV: &str = "KARGO_HOME";

#[derive(Debug, Clone)]
pub struct Config {
    home: PathBuf,
    offline: bool,
    refresh: bool,
    shell: Shell,
}

impl Config {
    pub fn new(home: PathBuf, shell: Shell) -> Self {
        Self { home, offline: false, refresh: false, shell }
    }

    /// Builds a configuration from the environment.
    pub fn from_env(shell: Shell) -> Result<Self> {
        let home = match std::env::var_os(HOME_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => dirs::home_dir().context("could not determine the home directory; set KARGO_HOME")?.join(".kargo"),
        };
        Ok(Self::new(home, shell))
    }

    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    pub fn with_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Cache of downloaded Maven artifacts.
    pub fn maven_cache(&self) -> PathBuf {
        self.home.join("cache").join("maven")
    }

    /// Extracted toolchains (e.g. Kotlin/Native distributions).
    pub fn toolchains_dir(&self) -> PathBuf {
        self.home.join("toolchains")
    }

    /// Kargo-managed JDKs, one directory per release.
    pub fn jdks_dir(&self) -> PathBuf {
        self.toolchains_dir().join("jdk")
    }

    /// Kotlin/Native distributions, one directory per version.
    pub fn native_dir(&self) -> PathBuf {
        self.toolchains_dir().join("kotlin-native")
    }

    /// Kotlin/Native's own dependency cache (LLVM, sysroots), normally `~/.konan`.
    pub fn konan_data_dir(&self) -> PathBuf {
        match std::env::var_os("KONAN_DATA_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => self.toolchains_dir().join("konan"),
        }
    }

    /// Directory searched for `kargo-<name>` plugin executables.
    pub fn bin_dir(&self) -> PathBuf {
        self.home.join("bin")
    }

    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Ignore cached metadata (e.g. `maven-metadata.xml`) and re-check remotes.
    pub fn refresh(&self) -> bool {
        self.refresh
    }

    pub fn shell(&self) -> &Shell {
        &self.shell
    }
}
