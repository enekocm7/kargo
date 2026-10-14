//! The Kotlin/Native distribution (`kotlin-native-prebuilt`), downloaded from
//! Maven Central into `~/.kargo/toolchains/kotlin-native/<version>`.
//!
//! Kotlin/Native downloads further dependencies (LLVM, sysroots) on first use.
//! Kargo points `KONAN_DATA_DIR` at `~/.kargo/toolchains/konan` so those are
//! managed alongside the distributions instead of living in `~/.konan`.
//!
//! The compiler can link against a precompiled Kotlin standard library instead of
//! lowering it again on every build, which roughly halves the time of a debug build. It
//! builds that cache inside the distribution (`klib/cache`) the first time it is asked to.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use kargo_maven::{Artifact, MavenClient, ModuleId, Version};
use kargo_util::config::Config;
use kargo_util::flock::FileLock;
use kargo_util::process::slash;

use crate::archive::{self, ArchiveKind};
use crate::install_lock;

/// Environment variable that turns the precompiled standard library off (`false`, `0`, `no` or `off`).
pub const CACHE_ENV: &str = "KARGO_NATIVE_CACHE";

/// Whether builds may use the precompiled standard library (on unless [`CACHE_ENV`] says otherwise).
pub fn cache_enabled() -> bool {
    std::env::var(CACHE_ENV).map_or(true, |v| !is_off(&v))
}

fn is_off(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off")
}

/// The host as Kotlin/Native names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// Classifier of the prebuilt distribution, e.g. `linux-x86_64`.
    pub classifier: &'static str,
    /// Archive format of the distribution.
    pub extension: &'static str,
    /// Konan target producing binaries for this host.
    pub konan_target: &'static str,
}

impl Host {
    pub fn current() -> Result<Self> {
        Self::for_platform(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn for_platform(os: &str, arch: &str) -> Result<Self> {
        Ok(match (os, arch) {
            ("windows", "x86_64") => Host { classifier: "windows-x86_64", extension: "zip", konan_target: "mingw_x64" },
            ("linux", "x86_64") => Host { classifier: "linux-x86_64", extension: "tar.gz", konan_target: "linux_x64" },
            ("macos", "aarch64") => Host { classifier: "macos-aarch64", extension: "tar.gz", konan_target: "macos_arm64" },
            ("macos", "x86_64") => Host { classifier: "macos-x86_64", extension: "tar.gz", konan_target: "macos_x64" },
            _ => bail!("Kotlin/Native does not support {os}/{arch} as a build host"),
        })
    }

    fn archive_kind(&self) -> ArchiveKind {
        if self.extension == "zip" { ArchiveKind::Zip } else { ArchiveKind::TarGz }
    }
}

/// Resolves `host` in a platform list to the concrete Konan target.
pub fn resolve_platform(name: &str) -> Result<String> {
    if name == "host" { Ok(Host::current()?.konan_target.to_owned()) } else { Ok(name.to_owned()) }
}

/// Extension of executables produced for a Konan target.
pub fn executable_extension(konan_target: &str) -> &'static str {
    if konan_target == "mingw_x64" { "exe" } else { "kexe" }
}

fn konanc_in(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "konanc.bat" } else { "konanc" })
}

fn cinterop_in(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "cinterop.bat" } else { "cinterop" })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeToolchain {
    pub home: PathBuf,
    pub version: String,
    /// `KONAN_DATA_DIR` used when running the compiler.
    pub data_dir: PathBuf,
}

impl NativeToolchain {
    pub fn konanc(&self) -> PathBuf {
        konanc_in(&self.home)
    }

    /// A `konanc` command with Kargo's Kotlin/Native data directory.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(self.konanc());
        cmd.env("KONAN_DATA_DIR", &self.data_dir);
        cmd
    }

    /// A `cinterop` command (C library bindings) with Kargo's Kotlin/Native data directory.
    pub fn cinterop_command(&self) -> Command {
        let mut cmd = Command::new(cinterop_in(&self.home));
        cmd.env("KONAN_DATA_DIR", &self.data_dir);
        cmd
    }

    /// `konanc` options that link the precompiled standard library, or none when this
    /// distribution has no standard library to precompile.
    ///
    /// `-Xauto-cache-from` names the klibs the compiler may precompile on demand; it puts the
    /// result in the distribution's own `klib/cache`, so removing the distribution removes it.
    pub fn cache_args(&self) -> Vec<String> {
        let stdlib = self.home.join("klib").join("common");
        if stdlib.join("stdlib").is_dir() { vec![format!("-Xauto-cache-from={}", slash(&stdlib))] } else { Vec::new() }
    }

    /// Where the compiler keeps the precompiled standard library for `konan_target`.
    /// Builds with debug info (`-g`) use a separate one. The directory names are the compiler's:
    /// `linux_x64-gSTATIC-system` with debug info, `linux_x64STATIC-system` without.
    pub fn stdlib_cache_dir(&self, konan_target: &str, debug: bool) -> PathBuf {
        let flavor = if debug { "-gSTATIC" } else { "STATIC" };
        self.home.join("klib").join("cache").join(format!("{konan_target}{flavor}-system")).join("stdlib-per-file-cache")
    }

    fn at(config: &Config, version: &str) -> Result<Self> {
        if version.is_empty() || version.contains(['/', '\\', ':']) || version.starts_with('.') {
            bail!("invalid Kotlin/Native version `{version}`");
        }
        Ok(Self { home: config.native_dir().join(version), version: version.to_owned(), data_dir: config.konan_data_dir() })
    }

    /// The installed distribution for `version`, if present.
    pub fn find(config: &Config, version: &str) -> Result<Option<Self>> {
        let toolchain = Self::at(config, version)?;
        migrate_legacy(config, &toolchain);
        Ok(toolchain.konanc().is_file().then_some(toolchain))
    }

    /// Locks the installation of `version` against other Kargo processes, after checking the version.
    fn lock(config: &Config, version: &str) -> Result<FileLock> {
        Self::at(config, version)?;
        install_lock(config, &format!("kotlin-native-{version}"))
    }

    /// Ensures the distribution for `version` is installed, downloading it if needed.
    pub fn ensure(config: &Config, client: &MavenClient, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, version)?;
        if let Some(found) = Self::find(config, version)? {
            return Ok(found);
        }
        let toolchain = Self::at(config, version)?;
        let host = Host::current()?;
        let artifact = Artifact::new(ModuleId::new("org.jetbrains.kotlin", "kotlin-native-prebuilt"), version)
            .with_classifier(Some(host.classifier.to_owned()))
            .with_extension(host.extension);
        let archive_path = client
            .fetch_required(&artifact.repo_path())
            .with_context(|| format!("failed to download Kotlin/Native {version} for {}", host.classifier))?;

        config.shell().status("Installing", format!("Kotlin/Native {version} ({})", host.classifier));
        kargo_util::fs::remove_dir_all_if_exists(&toolchain.home)?;
        archive::install(&archive_path, host.archive_kind(), &toolchain.home)?;
        // The archive is large and no longer needed once extracted.
        let _ = std::fs::remove_file(&archive_path);
        if !toolchain.konanc().is_file() {
            bail!("the Kotlin/Native distribution at {} has no konanc", toolchain.home.display());
        }
        Ok(toolchain)
    }

    /// Installed Kotlin/Native versions, oldest first.
    pub fn installed(config: &Config) -> Result<Vec<Self>> {
        migrate_all_legacy(config);
        let Ok(entries) = std::fs::read_dir(config.native_dir()) else { return Ok(Vec::new()) };
        let mut out: Vec<Self> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    return None;
                }
                let t = Self::at(config, &name).ok()?;
                t.konanc().is_file().then_some(t)
            })
            .collect();
        out.sort_by(|a, b| Version::parse(&a.version).cmp(&Version::parse(&b.version)));
        Ok(out)
    }

    /// Removes an installed distribution.
    pub fn remove(config: &Config, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, version)?;
        let Some(toolchain) = Self::find(config, version)? else {
            bail!("Kotlin/Native {version} is not installed; see `kargo toolchain list`");
        };
        kargo_util::fs::remove_dir_all_if_exists(&toolchain.home)?;
        Ok(toolchain)
    }
}

/// Migrates every legacy `toolchains/kotlin-native-<version>-<host>` directory.
fn migrate_all_legacy(config: &Config) {
    let Ok(host) = Host::current() else { return };
    let Ok(entries) = std::fs::read_dir(config.toolchains_dir()) else { return };
    let suffix = format!("-{}", host.classifier);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(version) = name.strip_prefix("kotlin-native-").and_then(|r| r.strip_suffix(&suffix))
            && let Ok(toolchain) = NativeToolchain::at(config, version)
        {
            migrate_legacy(config, &toolchain);
        }
    }
}

/// Moves a distribution installed by older Kargo versions
/// (`toolchains/kotlin-native-<version>-<host>`) to the current layout.
fn migrate_legacy(config: &Config, toolchain: &NativeToolchain) {
    if toolchain.konanc().is_file() {
        return;
    }
    let Ok(host) = Host::current() else { return };
    let legacy = config.toolchains_dir().join(format!("kotlin-native-{}-{}", toolchain.version, host.classifier));
    if konanc_in(&legacy).is_file() {
        if let Some(parent) = toolchain.home.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::rename(&legacy, &toolchain.home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn fake_dist(home: &Path) {
        let konanc = konanc_in(home);
        std::fs::create_dir_all(konanc.parent().unwrap()).unwrap();
        std::fs::write(konanc, "").unwrap();
    }

    #[test]
    fn host_mapping() {
        assert_eq!(Host::for_platform("windows", "x86_64").unwrap().konan_target, "mingw_x64");
        assert_eq!(Host::for_platform("macos", "aarch64").unwrap().classifier, "macos-aarch64");
        assert_eq!(Host::for_platform("linux", "x86_64").unwrap().archive_kind(), ArchiveKind::TarGz);
        assert!(Host::for_platform("freebsd", "x86_64").is_err());
    }

    #[test]
    fn platform_names() {
        assert_eq!(resolve_platform("linux_x64").unwrap(), "linux_x64");
        assert_eq!(executable_extension("mingw_x64"), "exe");
        assert_eq!(executable_extension("linux_x64"), "kexe");
    }

    #[test]
    fn lists_finds_and_removes_installations() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::new(tmp.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        assert!(NativeToolchain::installed(&config).unwrap().is_empty());
        fake_dist(&config.native_dir().join("2.4.21"));
        fake_dist(&config.native_dir().join("2.4.3"));
        std::fs::create_dir_all(config.native_dir().join("2.0.0")).unwrap(); // incomplete
        let versions: Vec<_> = NativeToolchain::installed(&config).unwrap().into_iter().map(|t| t.version).collect();
        assert_eq!(versions, ["2.4.3", "2.4.21"]);

        let found = NativeToolchain::find(&config, "2.4.21").unwrap().unwrap();
        assert_eq!(found.data_dir, config.konan_data_dir());
        let cmd = found.command();
        assert!(cmd.get_envs().any(|(k, _)| k == "KONAN_DATA_DIR"));
        assert!(NativeToolchain::find(&config, "9.9.9").unwrap().is_none());
        assert!(NativeToolchain::find(&config, "../x").is_err());

        NativeToolchain::remove(&config, "2.4.3").unwrap();
        assert!(NativeToolchain::remove(&config, "2.4.3").is_err());
        assert_eq!(NativeToolchain::installed(&config).unwrap().len(), 1);
    }

    #[test]
    fn stdlib_cache_options() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::new(tmp.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let home = config.native_dir().join("2.4.21");
        fake_dist(&home);
        let toolchain = NativeToolchain::find(&config, "2.4.21").unwrap().unwrap();
        // A distribution without a standard library has nothing to precompile.
        assert!(toolchain.cache_args().is_empty());

        std::fs::create_dir_all(home.join("klib/common/stdlib")).unwrap();
        let args = toolchain.cache_args();
        assert_eq!(args.len(), 1);
        let dir = args[0].strip_prefix("-Xauto-cache-from=").unwrap();
        assert!(dir.ends_with("2.4.21/klib/common"), "{dir}");
        assert!(!dir.contains('\\'), "argfiles use forward slashes: {dir}");

        // Debug-info builds and others each get their own cache.
        let debug = toolchain.stdlib_cache_dir("linux_x64", true);
        let plain = toolchain.stdlib_cache_dir("linux_x64", false);
        assert!(debug.ends_with("klib/cache/linux_x64-gSTATIC-system/stdlib-per-file-cache"), "{debug:?}");
        assert!(plain.ends_with("klib/cache/linux_x64STATIC-system/stdlib-per-file-cache"), "{plain:?}");
    }

    #[test]
    fn cache_switch_values() {
        for off in ["false", "FALSE", " 0 ", "no", "Off"] {
            assert!(is_off(off), "{off}");
        }
        for on in ["true", "1", "yes", "on", ""] {
            assert!(!is_off(on), "{on}");
        }
    }

    #[test]
    fn migrates_legacy_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::new(tmp.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let host = Host::current().unwrap();
        let legacy = config.toolchains_dir().join(format!("kotlin-native-2.4.0-{}", host.classifier));
        fake_dist(&legacy);
        let other = config.toolchains_dir().join(format!("kotlin-native-2.3.0-{}", host.classifier));
        fake_dist(&other);
        let versions: Vec<_> = NativeToolchain::installed(&config).unwrap().into_iter().map(|t| t.version).collect();
        assert_eq!(versions, ["2.3.0", "2.4.0"]);
        let found = NativeToolchain::find(&config, "2.4.0").unwrap().unwrap();
        assert!(found.konanc().is_file());
        assert!(!legacy.exists() && !other.exists());
    }
}
