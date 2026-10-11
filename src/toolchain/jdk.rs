//! Locating a Java Development Kit.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::util::process::find_on_path;

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

    pub fn has_javac(&self) -> bool {
        self.javac().is_file()
    }

    /// Finds a JDK via `JAVA_HOME`, falling back to `java` on `PATH`.
    pub fn detect() -> Result<Self> {
        if let Some(home) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
            let home = PathBuf::from(home);
            let java = home.join("bin").join(exe("java"));
            if java.is_file() {
                return Self::probe(&java).with_context(|| format!("JAVA_HOME points at an unusable JDK ({})", home.display()));
            }
        }
        let java = find_on_path("java")
            .context("no Java installation found: install a JDK (17 or newer) and set JAVA_HOME or put `java` on PATH")?;
        Self::probe(&java)
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
    fn version_requirement() {
        let jdk = Jdk { home: PathBuf::from("/j"), version: 11 };
        assert!(jdk.require_version(17).is_err());
        assert!(jdk.require_version(11).is_ok());
        assert!(jdk.java().ends_with(exe("java")));
    }
}
