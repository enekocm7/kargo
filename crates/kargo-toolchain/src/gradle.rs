//! Gradle, which drives the Android Gradle Plugin (AGP) for Android builds.
//!
//! Kargo downloads the Gradle distribution an AGP version needs from
//! `services.gradle.org` into `~/.kargo/toolchains/gradle/<version>`, checking the
//! SHA-256 Gradle publishes next to it, and keeps that checksum so the Gradle
//! wrapper it generates for the Android project pins the same distribution.
//!
//! [`AGP_GRADLE`] is the compatibility table from the AGP release notes: the
//! minimum (and default) Gradle version of each AGP release.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use kargo_maven::Version;
use kargo_util::config::Config;
use kargo_util::flock::FileLock;

use crate::archive::{self, ArchiveKind};
use crate::install_lock;

/// Where Gradle distributions are published.
pub const DISTRIBUTIONS_URL: &str = "https://services.gradle.org/distributions";

/// AGP `major.minor` and the Gradle version it requires, which is also the
/// version Android Studio uses by default. Newest first.
pub const AGP_GRADLE: &[(&str, &str)] = &[
    ("9.4", "9.6.0"),
    ("9.3", "9.5.0"),
    ("9.2", "9.4.1"),
    ("9.1", "9.3.1"),
    ("9.0", "9.1.0"),
    ("8.13", "8.13"),
    ("8.12", "8.13"),
    ("8.11", "8.13"),
    ("8.10", "8.11.1"),
    ("8.9", "8.11.1"),
    ("8.8", "8.10.2"),
    ("8.7", "8.9"),
    ("8.6", "8.7"),
    ("8.5", "8.7"),
    ("8.4", "8.6"),
    ("8.3", "8.4"),
    ("8.2", "8.2"),
    ("8.1", "8.0"),
    ("8.0", "8.0"),
    ("7.4", "7.5"),
    ("7.3", "7.4"),
    ("7.2", "7.3.3"),
    ("7.1", "7.2"),
    ("7.0", "7.0"),
];

/// File in a distribution's directory holding the SHA-256 of its archive.
const CHECKSUM_FILE: &str = "kargo-sha256.txt";

/// Directory in a distribution's directory holding its generated Gradle wrapper.
const WRAPPER_DIR: &str = "kargo-wrapper";

/// The `major.minor` of a version, as numbers.
pub(crate) fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// What Kargo knows about the Gradle an AGP version needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgpGradle {
    /// The AGP version is in [`AGP_GRADLE`]: the minimum Gradle version.
    Known(&'static str),
    /// The AGP version is newer than the table: the newest Gradle Kargo knows of,
    /// which may be too old for it.
    Newer(&'static str),
}

impl AgpGradle {
    pub fn version(self) -> &'static str {
        match self {
            AgpGradle::Known(v) | AgpGradle::Newer(v) => v,
        }
    }
}

/// The Gradle version `agp` requires (and Kargo uses by default).
pub fn gradle_for_agp(agp: &str) -> Result<AgpGradle> {
    let Some(wanted) = major_minor(agp) else { bail!("invalid Android Gradle Plugin version `{agp}`") };
    for (row, gradle) in AGP_GRADLE {
        if major_minor(row) == Some(wanted) {
            return Ok(AgpGradle::Known(gradle));
        }
    }
    let (newest, gradle) = AGP_GRADLE[0];
    if Some(wanted) > major_minor(newest) {
        return Ok(AgpGradle::Newer(gradle));
    }
    bail!("Android Gradle Plugin {agp} is not supported; use `agp-version` 7.0 or newer (Kargo's default is the newest)")
}

/// Checks that `gradle` can run `agp`, naming the compatible versions if not.
pub fn check_compatible(agp: &str, gradle: &str) -> Result<()> {
    let required = gradle_for_agp(agp)?;
    let AgpGradle::Known(minimum) = required else { return Ok(()) };
    if Version::parse(gradle) < Version::parse(minimum) {
        bail!(
            "Gradle {gradle} is too old for Android Gradle Plugin {agp}, which needs Gradle {minimum} or newer; \
             set `gradle-version = \"{minimum}\"` under [target.android] (or remove it to use {minimum}), or pick an older `agp-version`"
        );
    }
    Ok(())
}

/// The oldest JDK that can run `agp`.
pub fn min_jdk_for_agp(agp: &str) -> u32 {
    match major_minor(agp) {
        Some((major, _)) if major < 8 => 11,
        _ => 17,
    }
}

/// A Gradle wrapper script (`gradlew` or `gradlew.bat`) in `dir`, if there is one.
pub fn project_wrapper(dir: &Path) -> Option<PathBuf> {
    let wrapper = dir.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
    wrapper.is_file().then_some(wrapper)
}

/// URL of the `-bin` distribution of `version`.
pub fn distribution_url(version: &str) -> String {
    format!("{DISTRIBUTIONS_URL}/gradle-{version}-bin.zip")
}

/// A Kargo-managed Gradle distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradleDistribution {
    pub version: String,
    pub home: PathBuf,
}

impl GradleDistribution {
    fn at(config: &Config, version: &str) -> Result<Self> {
        if version.is_empty()
            || !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            || version.starts_with('.')
        {
            bail!("invalid Gradle version `{version}`");
        }
        Ok(Self { version: version.to_owned(), home: config.gradle_dir().join(version) })
    }

    /// The `gradle` launcher script.
    pub fn executable(&self) -> PathBuf {
        self.home.join("bin").join(if cfg!(windows) { "gradle.bat" } else { "gradle" })
    }

    /// SHA-256 of the distribution archive, as published by Gradle.
    pub fn sha256(&self) -> Option<String> {
        std::fs::read_to_string(self.home.join(CHECKSUM_FILE)).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    }

    /// The installed distribution for `version`, if present.
    pub fn find(config: &Config, version: &str) -> Result<Option<Self>> {
        let dist = Self::at(config, version)?;
        Ok(dist.executable().is_file().then_some(dist))
    }

    /// Locks the installation of `version` against other Kargo processes, after checking the version.
    fn lock(config: &Config, version: &str) -> Result<FileLock> {
        Self::at(config, version)?;
        install_lock(config, &format!("gradle-{version}"))
    }

    /// Ensures the distribution for `version` is installed, downloading it if needed.
    pub fn ensure(config: &Config, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, version)?;
        if let Some(found) = Self::find(config, version)? {
            return Ok(found);
        }
        crate::ensure_auto_install(config, &format!("Gradle {version}"), &format!("kargo toolchain install gradle {version}"))?;
        let dist = Self::at(config, version)?;
        let url = distribution_url(version);
        let sha256 = crate::download::get_text(config, &format!("{url}.sha256"))
            .with_context(|| format!("failed to download Gradle {version}; is it a released version (see {DISTRIBUTIONS_URL})?"))?;
        let sha256 = sha256.trim().to_owned();
        if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("unexpected checksum for Gradle {version} at {url}.sha256");
        }
        let archive_path = config.toolchains_dir().join("downloads").join(format!("gradle-{version}-bin.zip"));
        let cached = archive_path.is_file() && crate::download::verify(&archive_path, &sha256).is_ok();
        if !cached {
            crate::download::download(config, &url, &archive_path, Some(&sha256))?;
        }
        config.shell().status("Installing", format!("Gradle {version}"));
        kargo_util::fs::remove_dir_all_if_exists(&dist.home)?;
        archive::install(&archive_path, ArchiveKind::Zip, &dist.home)?;
        let _ = std::fs::remove_file(&archive_path);
        if !dist.executable().is_file() {
            bail!("the Gradle distribution at {} has no bin/gradle", dist.home.display());
        }
        kargo_util::fs::write_string_atomic(&dist.home.join(CHECKSUM_FILE), &format!("{sha256}\n"))?;
        Ok(dist)
    }

    /// Installed Gradle versions, oldest first.
    pub fn installed(config: &Config) -> Result<Vec<Self>> {
        let Ok(entries) = std::fs::read_dir(config.gradle_dir()) else { return Ok(Vec::new()) };
        let mut out: Vec<Self> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let dist = Self::at(config, &name).ok()?;
                dist.executable().is_file().then_some(dist)
            })
            .collect();
        out.sort_by(|a, b| Version::parse(&a.version).cmp(&Version::parse(&b.version)));
        Ok(out)
    }

    /// Removes an installed distribution.
    pub fn remove(config: &Config, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, version)?;
        let Some(dist) = Self::find(config, version)? else {
            bail!("Gradle {version} is not installed; see `kargo toolchain list`");
        };
        kargo_util::fs::remove_dir_all_if_exists(&dist.home)?;
        Ok(dist)
    }

    /// A directory with a Gradle wrapper (`gradlew`, `gradlew.bat`, `gradle/wrapper/`) for
    /// this version, pinned to its checksum. Generated once, by running this distribution's
    /// own `wrapper` task with the JDK at `java_home`.
    pub fn wrapper(&self, config: &Config, java_home: &Path) -> Result<PathBuf> {
        let dir = self.home.join(WRAPPER_DIR);
        if wrapper_complete(&dir) {
            return Ok(dir);
        }
        // Gradle runs in a scratch project outside the distribution: its daemon keeps files
        // there open (on Windows that blocks moving them), so only the wrapper is copied out.
        let scratch = std::env::temp_dir().join(format!("kargo-gradle-wrapper-{}-{}", self.version, std::process::id()));
        let staging = self.home.join(format!(".staging-wrapper-{}", std::process::id()));
        let result = (|| -> Result<()> {
            kargo_util::fs::remove_dir_all_if_exists(&scratch)?;
            std::fs::create_dir_all(&scratch)?;
            std::fs::write(scratch.join("settings.gradle.kts"), "")?;
            // Output goes to a file, not a pipe: a daemon Gradle starts would hold a pipe open.
            let log = scratch.join("gradle.log");
            let out = std::fs::File::create(&log)?;
            let mut cmd = Command::new(self.executable());
            cmd.args(["--quiet", "--offline", "--no-watch-fs", "--console=plain", "wrapper", "--distribution-type", "bin"]);
            cmd.args(["--gradle-version", &self.version]);
            if let Some(sha256) = self.sha256() {
                cmd.args(["--gradle-distribution-sha256-sum", &sha256]);
            }
            cmd.arg("--project-dir").arg(&scratch).current_dir(&scratch).env("JAVA_HOME", java_home);
            cmd.stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
            config.shell().verbose("Running", kargo_util::process::describe(&cmd));
            let status = cmd.status().with_context(|| format!("failed to run {}", self.executable().display()))?;
            if !status.success() {
                bail!(
                    "`gradle wrapper` failed:
{}",
                    std::fs::read_to_string(&log).unwrap_or_default().trim()
                );
            }
            if !wrapper_complete(&scratch) {
                bail!("`gradle wrapper` did not write a complete wrapper");
            }
            kargo_util::fs::remove_dir_all_if_exists(&staging)?;
            for file in WRAPPER_FILES {
                let to = staging.join(file);
                std::fs::create_dir_all(to.parent().context("wrapper file has no parent")?)?;
                std::fs::copy(scratch.join(file), &to)?;
            }
            match kargo_util::fs::rename_with_retries(&staging, &dir) {
                Ok(()) => Ok(()),
                // Another process generated it meanwhile.
                Err(_) if wrapper_complete(&dir) => Ok(()),
                Err(e) => Err(e).with_context(|| format!("failed to create {}", dir.display())),
            }
        })();
        let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
        let _ = kargo_util::fs::remove_dir_all_if_exists(&scratch);
        result.with_context(|| format!("failed to generate a Gradle {} wrapper", self.version))?;
        Ok(dir)
    }
}

/// The files of a Gradle wrapper, relative to the project directory.
pub const WRAPPER_FILES: &[&str] =
    &["gradlew", "gradlew.bat", "gradle/wrapper/gradle-wrapper.jar", "gradle/wrapper/gradle-wrapper.properties"];

fn wrapper_complete(dir: &Path) -> bool {
    WRAPPER_FILES.iter().all(|f| dir.join(f).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn fake_dist(config: &Config, version: &str) -> GradleDistribution {
        let dist = GradleDistribution::at(config, version).unwrap();
        std::fs::create_dir_all(dist.executable().parent().unwrap()).unwrap();
        std::fs::write(dist.executable(), "").unwrap();
        dist
    }

    #[test]
    fn compatibility_table() {
        assert_eq!(gradle_for_agp("9.4.1").unwrap(), AgpGradle::Known("9.6.0"));
        assert_eq!(gradle_for_agp("9.0.0-rc01").unwrap(), AgpGradle::Known("9.1.0"));
        assert_eq!(gradle_for_agp("8.13.2").unwrap(), AgpGradle::Known("8.13"));
        assert_eq!(gradle_for_agp("7.4.2").unwrap(), AgpGradle::Known("7.5"));
        assert_eq!(gradle_for_agp("10.0.0").unwrap(), AgpGradle::Newer("9.6.0"));
        assert!(gradle_for_agp("4.2.0").unwrap_err().to_string().contains("not supported"));
        assert!(gradle_for_agp("x").is_err());
        // The table is sorted newest first, which `gradle_for_agp` relies on for newer versions.
        for pair in AGP_GRADLE.windows(2) {
            assert!(major_minor(pair[0].0) > major_minor(pair[1].0), "{pair:?}");
            assert!(Version::parse(pair[0].1) >= Version::parse(pair[1].1), "{pair:?}");
        }
    }

    #[test]
    fn checks_compatibility() {
        check_compatible("9.4.1", "9.6.0").unwrap();
        check_compatible("9.4.1", "9.7").unwrap();
        check_compatible("8.13.0", "8.13").unwrap();
        let err = check_compatible("9.4.1", "9.1").unwrap_err().to_string();
        assert!(err.contains("Gradle 9.1 is too old for Android Gradle Plugin 9.4.1"), "{err}");
        assert!(err.contains("gradle-version = \"9.6.0\""), "{err}");
        // Unknown newer AGP versions are not rejected.
        check_compatible("11.0.0", "9.0").unwrap();
        assert_eq!(min_jdk_for_agp("7.4.2"), 11);
        assert_eq!(min_jdk_for_agp("9.4.1"), 17);
    }

    #[test]
    fn lists_finds_and_removes_installations() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::new(tmp.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
        assert!(GradleDistribution::installed(&config).unwrap().is_empty());
        fake_dist(&config, "9.6.0");
        fake_dist(&config, "8.13");
        std::fs::create_dir_all(config.gradle_dir().join("9.1.0")).unwrap(); // incomplete
        let versions: Vec<_> = GradleDistribution::installed(&config).unwrap().into_iter().map(|d| d.version).collect();
        assert_eq!(versions, ["8.13", "9.6.0"]);

        let found = GradleDistribution::find(&config, "9.6.0").unwrap().unwrap();
        assert!(found.sha256().is_none());
        std::fs::write(found.home.join(CHECKSUM_FILE), "abc\n").unwrap();
        assert_eq!(found.sha256().as_deref(), Some("abc"));
        assert_eq!(GradleDistribution::ensure(&config, "9.6.0").unwrap(), found, "installed versions need no download");
        assert!(GradleDistribution::find(&config, "../x").is_err());
        let err = format!("{:#}", GradleDistribution::ensure(&config, "9.5.0").unwrap_err());
        assert!(err.contains("offline"), "{err}");

        GradleDistribution::remove(&config, "8.13").unwrap();
        assert!(GradleDistribution::remove(&config, "8.13").is_err());
        assert_eq!(GradleDistribution::installed(&config).unwrap().len(), 1);
    }

    #[test]
    fn finds_project_wrappers() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(project_wrapper(tmp.path()).is_none());
        let name = if cfg!(windows) { "gradlew.bat" } else { "gradlew" };
        std::fs::write(tmp.path().join(name), "").unwrap();
        assert_eq!(project_wrapper(tmp.path()), Some(tmp.path().join(name)));
        assert_eq!(distribution_url("9.6.0"), "https://services.gradle.org/distributions/gradle-9.6.0-bin.zip");
    }
}
