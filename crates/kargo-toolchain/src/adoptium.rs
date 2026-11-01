//! The Eclipse Adoptium (Temurin) API, used to find JDK builds.
//!
//! <https://api.adoptium.net/q/swagger-ui/>

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use kargo_util::config::Config;

use crate::archive::ArchiveKind;

pub const API: &str = "https://api.adoptium.net/v3";

/// A downloadable JDK build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdkPackage {
    /// Release name, e.g. `jdk-21.0.5+11`.
    pub release: String,
    pub major: u32,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub archive: ArchiveKind,
}

/// Adoptium's names for the current OS and CPU.
pub fn platform() -> Result<(&'static str, &'static str)> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn platform_for(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    let os = match os {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "mac",
        other => bail!("Temurin JDKs are not available for {other}; set [toolchain] jdk = \"system\""),
    };
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => bail!("Temurin JDKs are not available for the {other} architecture; set [toolchain] jdk = \"system\""),
    };
    Ok((os, arch))
}

#[derive(Debug, Deserialize)]
struct Package {
    link: String,
    checksum: String,
    name: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Binary {
    package: Package,
    image_type: String,
}

#[derive(Debug, Deserialize)]
struct LatestAsset {
    binary: Binary,
    release_name: String,
    version: VersionData,
}

#[derive(Debug, Deserialize)]
struct VersionData {
    major: u32,
}

#[derive(Debug, Deserialize)]
struct Release {
    binaries: Vec<Binary>,
    release_name: String,
    version_data: VersionData,
}

#[derive(Debug, Deserialize)]
pub struct AvailableReleases {
    pub available_releases: Vec<u32>,
    pub available_lts_releases: Vec<u32>,
    pub most_recent_lts: u32,
}

fn package(binary: Binary, release: String, major: u32) -> Result<JdkPackage> {
    Ok(JdkPackage {
        archive: ArchiveKind::from_name(&binary.package.name)?,
        release,
        major,
        url: binary.package.link,
        sha256: binary.package.checksum,
        size: binary.package.size,
    })
}

/// Parses `/v3/assets/latest/{major}/hotspot`.
pub fn parse_latest(json: &str) -> Result<Option<JdkPackage>> {
    let assets: Vec<LatestAsset> = serde_json::from_str(json).context("unexpected response from the Adoptium API")?;
    let Some(asset) = assets.into_iter().find(|a| a.binary.image_type == "jdk") else { return Ok(None) };
    package(asset.binary, asset.release_name, asset.version.major).map(Some)
}

/// Parses `/v3/assets/release_name/eclipse/{release}`.
pub fn parse_release(json: &str) -> Result<Option<JdkPackage>> {
    let release: Release = serde_json::from_str(json).context("unexpected response from the Adoptium API")?;
    let Some(binary) = release.binaries.into_iter().find(|b| b.image_type == "jdk") else { return Ok(None) };
    package(binary, release.release_name, release.version_data.major).map(Some)
}

/// The newest JDK build of a feature version.
pub fn latest(config: &Config, major: u32) -> Result<JdkPackage> {
    let (os, arch) = platform()?;
    latest_for(config, major, os, arch)
}

/// The newest JDK build of a feature version for `os`/`arch` (Adoptium names, see
/// [`platform_for`]).
pub fn latest_for(config: &Config, major: u32, os: &str, arch: &str) -> Result<JdkPackage> {
    let url = format!("{API}/assets/latest/{major}/hotspot?architecture={arch}&image_type=jdk&os={os}&vendor=eclipse");
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up Temurin JDK {major}"))?;
    parse_latest(&body)?.with_context(|| format!("no Temurin JDK {major} build is available for {os}/{arch}"))
}

/// A specific JDK release such as `jdk-21.0.5+11`.
pub fn release(config: &Config, name: &str) -> Result<JdkPackage> {
    let (os, arch) = platform()?;
    release_for(config, name, os, arch)
}

/// A specific JDK release such as `jdk-21.0.5+11` for `os`/`arch` (Adoptium names).
pub fn release_for(config: &Config, name: &str, os: &str, arch: &str) -> Result<JdkPackage> {
    let encoded = name.replace('+', "%2B");
    let url = format!("{API}/assets/release_name/eclipse/{encoded}?architecture={arch}&image_type=jdk&os={os}&project=jdk");
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up Temurin release `{name}`"))?;
    parse_release(&body)?.with_context(|| format!("Temurin release `{name}` has no JDK build for {os}/{arch}"))
}

/// Feature versions that can be installed.
pub fn available(config: &Config) -> Result<AvailableReleases> {
    let body = crate::download::get_text(config, &format!("{API}/info/available_releases"))?;
    serde_json::from_str(&body).context("unexpected response from the Adoptium API")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: &str = r#"[{"binary": {"architecture": "x64", "image_type": "jdk", "os": "windows",
        "package": {"checksum": "f9d6", "link": "https://example.com/OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip",
                    "name": "OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip", "size": 205073461}},
        "release_name": "jdk-21.0.12.1+1", "vendor": "eclipse",
        "version": {"build": 1, "major": 21, "minor": 0, "security": 12}}]"#;

    #[test]
    fn parses_latest_assets() {
        let pkg = parse_latest(LATEST).unwrap().unwrap();
        assert_eq!(pkg.release, "jdk-21.0.12.1+1");
        assert_eq!(pkg.major, 21);
        assert_eq!(pkg.sha256, "f9d6");
        assert_eq!(pkg.archive, ArchiveKind::Zip);
        assert_eq!(pkg.size, 205073461);
        assert!(parse_latest("[]").unwrap().is_none());
        assert!(parse_latest("{").is_err());
    }

    #[test]
    fn parses_release() {
        let json = r#"{"binaries": [
            {"image_type": "jre", "package": {"checksum": "1", "link": "l1", "name": "jre.tar.gz", "size": 1}},
            {"image_type": "jdk", "package": {"checksum": "2", "link": "l2", "name": "OpenJDK17U-jdk_x64_linux.tar.gz", "size": 2}}],
            "release_name": "jdk-17.0.13+11", "version_data": {"major": 17}}"#;
        let pkg = parse_release(json).unwrap().unwrap();
        assert_eq!(pkg.url, "l2");
        assert_eq!(pkg.archive, ArchiveKind::TarGz);
        assert_eq!(pkg.major, 17);
    }

    #[test]
    fn platform_names() {
        assert_eq!(platform_for("macos", "aarch64").unwrap(), ("mac", "aarch64"));
        assert_eq!(platform_for("windows", "x86_64").unwrap(), ("windows", "x64"));
        assert!(platform_for("freebsd", "x86_64").is_err());
        assert!(platform_for("linux", "riscv64").is_err());
    }
}
