//! Locating a Java Development Kit.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use kargo_util::fs::dunce_canonicalize;
use kargo_util::process::find_on_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jdk {
    pub home: PathBuf,
    /// Major version (`17`, `21`, ...).
    pub version: u32,
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

impl Jdk {
    pub fn java(&self) -> PathBuf {
        self.home.join("bin").join(exe("java"))
    }

    pub fn javac(&self) -> PathBuf {
        self.home.join("bin").join(exe("javac"))
    }

    pub fn javap(&self) -> PathBuf {
        self.home.join("bin").join(exe("javap"))
    }

    pub fn has_javac(&self) -> bool {
        self.javac().is_file()
    }

    /// Options `kargo run` and `kargo test` put before `[target.jvm] jvm-args`, so the program's output
    /// stays free of JVM warnings: JDK 21 warns when a library (mockk, ByteBuddy) loads an agent
    /// dynamically, and newer JDKs when native code is loaded without `--enable-native-access`
    /// (Compose, Skiko). Users can still override them in `jvm-args`.
    pub fn default_run_args(&self) -> Vec<&'static str> {
        if self.version >= 21 { vec!["-XX:+EnableDynamicAgentLoading", "--enable-native-access=ALL-UNNAMED"] } else { Vec::new() }
    }

    /// Finds a JDK via `JAVA_HOME`, falling back to `java` on `PATH`.
    pub fn detect() -> Result<Self> {
        Self::find()?.context("no Java installation found: install a JDK (17 or newer) and set JAVA_HOME or put `java` on PATH")
    }

    /// Like [`Jdk::detect`], with `None` when there is no JDK on `JAVA_HOME` or `PATH`. A JDK
    /// that is found but unusable is still an error.
    pub fn find() -> Result<Option<Self>> {
        if let Some(home) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
            let home = PathBuf::from(home);
            let java = home.join("bin").join(exe("java"));
            if java.is_file() {
                return Self::from_java(&java)
                    .map(Some)
                    .with_context(|| format!("JAVA_HOME points at an unusable JDK ({})", home.display()));
            }
        }
        find_on_path("java").map(|java| Self::from_java(&java)).transpose()
    }

    /// Identifies the JDK of a `java` executable, from the `release` file next to its `bin`
    /// directory when there is one (no JVM start), otherwise by asking `java` itself.
    pub fn from_java(java: &Path) -> Result<Self> {
        // Without Windows' `\\?\` prefix: the home is passed to the Kotlin compiler, which rejects it.
        let real = dunce_canonicalize(java);
        let home = real.parent().and_then(Path::parent);
        if let Some((home, version)) =
            home.and_then(|h| Some((h, std::fs::read_to_string(h.join("release")).ok().as_deref().and_then(parse_release)?)))
        {
            return Ok(Self { home: home.to_path_buf(), version });
        }
        Self::probe(java)
    }

    /// Asks `java` for its real home and specification version.
    pub fn probe(java: &Path) -> Result<Self> {
        let output = Command::new(java)
            .args(["-XshowSettings:properties", "-version"])
            .output()
            .with_context(|| format!("failed to run {}", java.display()))?;
        let text = String::from_utf8_lossy(&output.stderr);
        let (home, version) =
            parse_properties(&text).with_context(|| format!("could not determine the JDK version of {}", java.display()))?;
        Ok(Self { home, version })
    }

    pub fn require_version(&self, minimum: u32) -> Result<()> {
        if self.version < minimum {
            bail!("JDK {} at {} is too old; Kargo needs JDK {minimum} or newer", self.version, self.home.display());
        }
        Ok(())
    }
}

/// The major version from the `JAVA_VERSION="..."` line of a JDK's `release` file.
pub(crate) fn parse_release(text: &str) -> Option<u32> {
    let line = text.lines().find_map(|l| l.trim().strip_prefix("JAVA_VERSION="))?;
    let v = line.trim().trim_matches('"');
    // Java 8 reports `1.8.0_392`; early-access builds `26-ea`.
    let v = v.strip_prefix("1.").unwrap_or(v);
    v.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Extracts `java.home` and `java.specification.version` from `-XshowSettings:properties`.
fn parse_properties(text: &str) -> Option<(PathBuf, u32)> {
    let mut home = None;
    let mut version = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("java.home = ") {
            home = Some(PathBuf::from(v.trim()));
        } else if let Some(v) = line.strip_prefix("java.specification.version = ") {
            let v = v.trim();
            // Java 8 reports `1.8`.
            let major = v.strip_prefix("1.").unwrap_or(v);
            version = major.split('.').next().and_then(|m| m.parse().ok());
        }
    }
    Some((home?, version?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_run_args_need_jdk_21() {
        let jdk = |version| Jdk { home: PathBuf::from("/j"), version };
        assert!(jdk(17).default_run_args().is_empty());
        assert_eq!(jdk(21).default_run_args(), ["-XX:+EnableDynamicAgentLoading", "--enable-native-access=ALL-UNNAMED"]);
        assert_eq!(jdk(25).default_run_args().len(), 2);
    }

    #[test]
    fn parses_show_settings_output() {
        let text = "Property settings:\n    java.class.path = \n    java.home = /usr/lib/jvm/jdk-21\n    java.specification.version = 21\n    java.vendor = X\n";
        let (home, version) = parse_properties(text).unwrap();
        assert_eq!(home, PathBuf::from("/usr/lib/jvm/jdk-21"));
        assert_eq!(version, 21);
        let (_, legacy) = parse_properties("java.home = /j\njava.specification.version = 1.8\n").unwrap();
        assert_eq!(legacy, 8);
        assert!(parse_properties("nothing here").is_none());
    }

    #[test]
    fn parses_release_file() {
        assert_eq!(parse_release("IMPLEMENTOR=\"X\"\nJAVA_VERSION=\"21.0.12.1\"\nOS_NAME=\"Linux\"\n"), Some(21));
        assert_eq!(parse_release("JAVA_VERSION=\"1.8.0_392\""), Some(8));
        assert_eq!(parse_release("JAVA_VERSION=\"26-ea\""), Some(26));
        assert_eq!(parse_release("JAVA_VERSION=\"25\""), Some(25));
        assert_eq!(parse_release("JAVA_RUNTIME_VERSION=\"21\""), None);
    }

    #[test]
    fn version_requirement() {
        let jdk = Jdk { home: PathBuf::from("/j"), version: 11 };
        assert!(jdk.require_version(17).is_err());
        assert!(jdk.require_version(11).is_ok());
        assert!(jdk.java().ends_with(exe("java")));
    }
}
