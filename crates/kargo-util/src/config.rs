//! Global, per-invocation configuration (home directory, network mode, output).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::http::Network;
use crate::settings::Settings;
use crate::shell::Shell;

/// Environment variable overriding the Kargo home directory (default `~/.kargo`).
pub const HOME_ENV: &str = "KARGO_HOME";

/// Environment variable that turns automatic toolchain installs off (`0`) or on (`1`), over
/// `[toolchain] auto-install` of `config.toml`.
pub const AUTO_INSTALL_ENV: &str = "KARGO_AUTO_INSTALL";

#[derive(Debug, Clone)]
pub struct Config {
    home: PathBuf,
    offline: bool,
    refresh: bool,
    system_gradle: bool,
    auto_install: bool,
    locked: bool,
    require_checksums: bool,
    shell: Shell,
    settings: Arc<Settings>,
    network: Arc<Network>,
    vendor_dir: Option<PathBuf>,
}

impl Config {
    pub fn new(home: PathBuf, shell: Shell) -> Self {
        Self {
            home,
            offline: false,
            refresh: false,
            system_gradle: false,
            auto_install: true,
            locked: false,
            require_checksums: false,
            shell,
            settings: Arc::default(),
            network: Arc::default(),
            vendor_dir: None,
        }
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

    pub fn with_system_gradle(mut self, system_gradle: bool) -> Self {
        self.system_gradle = system_gradle;
        self
    }

    /// Whether a command may download a toolchain it needs (see [`Config::auto_install`]).
    pub fn with_auto_install(mut self, auto_install: bool) -> Self {
        self.auto_install = auto_install;
        self
    }

    /// Records `--locked`, which also keeps Kargo from choosing a JDK for the project by itself.
    pub fn with_locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Fails on a downloaded file whose repository publishes no checksum (`--require-checksums`).
    pub fn with_require_checksums(mut self, require: bool) -> Self {
        self.require_checksums = require;
        self
    }

    /// Applies the settings of the configuration files (see [`Settings::load`]), checking the
    /// proxy and certificate bundle they name.
    pub fn with_settings(mut self, settings: Settings) -> Result<Self> {
        self.network = Arc::new(Network::from_settings(&settings)?);
        self.settings = Arc::new(settings);
        Ok(self)
    }

    /// Resolves dependencies only from the vendored repository at `dir` (`--vendored`).
    pub fn with_vendor_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.vendor_dir = dir;
        self
    }

    /// The settings of `~/.kargo/config.toml` and the project's `.kargo/config.toml`.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The proxy and certificates HTTP clients use (see [`crate::http::agent`]).
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// The directory of `kargo vendor`'s repository when dependencies come only from it.
    pub fn vendor_dir(&self) -> Option<&Path> {
        self.vendor_dir.as_deref()
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Cache of downloaded Maven artifacts.
    pub fn maven_cache(&self) -> PathBuf {
        self.home.join("cache").join("maven")
    }

    /// Compiled single-file scripts (`kargo script`), one directory per script and settings.
    pub fn scripts_cache(&self) -> PathBuf {
        self.home.join("cache").join("scripts")
    }

    /// Extracted toolchains (e.g. Kotlin/Native distributions).
    pub fn toolchains_dir(&self) -> PathBuf {
        self.home.join("toolchains")
    }

    /// Kargo-managed JDKs, one directory per release.
    pub fn jdks_dir(&self) -> PathBuf {
        self.toolchains_dir().join("jdk")
    }

    /// Kargo-managed GraalVM distributions, one directory per distribution and version.
    pub fn graalvm_dir(&self) -> PathBuf {
        self.toolchains_dir().join("graalvm")
    }

    /// Kotlin/Native distributions, one directory per version.
    pub fn native_dir(&self) -> PathBuf {
        self.toolchains_dir().join("kotlin-native")
    }

    /// Kargo-managed Gradle distributions, one directory per version.
    pub fn gradle_dir(&self) -> PathBuf {
        self.toolchains_dir().join("gradle")
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

    /// Where `kargo install` keeps the programs it installed, one directory per program and version.
    pub fn install_dir(&self) -> PathBuf {
        self.home.join("install")
    }

    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Ignore cached metadata (e.g. `maven-metadata.xml`) and re-check remotes.
    pub fn refresh(&self) -> bool {
        self.refresh
    }

    /// Run Android builds with the Gradle on `PATH` instead of a Kargo-managed one (`--system-gradle`).
    pub fn system_gradle(&self) -> bool {
        self.system_gradle
    }

    /// Whether a command may download a toolchain it needs but doesn't find installed (a JDK,
    /// GraalVM, Kotlin/Native, Gradle, a language server). `kargo toolchain install` always may.
    pub fn auto_install(&self) -> bool {
        self.auto_install
    }

    /// Whether the command runs with `--locked`.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Whether a file downloaded without a checksum published next to it is an error rather than
    /// accepted unverified (`--require-checksums`). Files `kargo.lock` pins are checked against it instead.
    pub fn require_checksums(&self) -> bool {
        self.require_checksums
    }

    pub fn shell(&self) -> &Shell {
        &self.shell
    }
}
