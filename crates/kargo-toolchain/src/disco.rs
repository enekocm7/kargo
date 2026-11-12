//! JDKs of vendors other than Temurin (Corretto, Zulu, Liberica, ...), found with the foojay Disco
//! API (<https://api.foojay.io/swagger-ui>), which lists the builds of every vendor with their
//! checksums. Temurin JDKs come from the Adoptium API instead (see [`crate::adoptium`]).

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use kargo_manifest::JdkVendor;
use kargo_maven::Version;
use kargo_util::config::Config;

use crate::adoptium::JdkPackage;
use crate::archive::ArchiveKind;
use crate::graalvm::DISCO_API;

/// foojay's names for an OS and CPU given by their Adoptium names (see [`crate::adoptium::platform_for`]),
/// and the archive format to ask for there.
fn platform(os: &str, arch: &str) -> (&'static str, &'static str, &'static str) {
    let (os, archive) = match os {
        "windows" => ("windows", "zip"),
        "mac" => ("macos", "tar.gz"),
        _ => ("linux", "tar.gz"),
    };
    let arch = if arch == "aarch64" { "aarch64" } else { "x64" };
    (os, arch, archive)
}

#[derive(Debug, Deserialize)]
struct Packages {
    result: Vec<Package>,
}

#[derive(Debug, Clone, Deserialize)]
struct Package {
    id: String,
    java_version: String,
    major_version: u32,
    filename: String,
    #[serde(default)]
    javafx_bundled: bool,
    #[serde(default)]
    latest_build_available: bool,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Infos {
    result: Vec<Info>,
}

#[derive(Debug, Deserialize)]
struct Info {
    direct_download_uri: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    checksum_type: String,
    #[serde(default)]
    checksum_uri: String,
}

/// The release name Kargo uses for a foojay `java_version`: `21.0.5+11` is `jdk-21.0.5+11`, as for Temurin.
fn release_name(java_version: &str) -> String {
    format!("jdk-{java_version}")
}

/// The JDK of a `/packages` response that `wanted` asks for: the newest build, or with a release name
/// (`jdk-21.0.5+11`) the build of that version. Builds without JavaFX come first.
fn choose(json: &str, wanted: Option<&str>) -> Result<Option<Package>> {
    let packages: Packages = serde_json::from_str(json).context("unexpected response from the foojay Disco API")?;
    let candidates = packages.result.into_iter().filter(|p| ArchiveKind::from_name(&p.filename).is_ok());
    let chosen = match wanted {
        Some(release) => candidates.filter(|p| release_name(&p.java_version) == release).min_by_key(|p| p.javafx_bundled),
        None => candidates.max_by_key(|p| (!p.javafx_bundled, p.latest_build_available, Version::parse(&p.java_version))),
    };
    Ok(chosen)
}

/// The download URL and checksum of an `/ids/{id}` response. The checksum is the SHA-256 of the
/// response, of the checksum file it names, or of a `.sha256` / `.sha256.txt` file next to the
/// archive (SapMachine publishes those); else the SHA-1 of the response, when a vendor (Liberica)
/// publishes nothing stronger.
fn download(config: &Config, id: &str, what: &str) -> Result<(String, String)> {
    let body = crate::download::get_text(config, &format!("{DISCO_API}/ids/{id}"))
        .with_context(|| format!("failed to look up the download of {what}"))?;
    let infos: Infos = serde_json::from_str(&body).context("unexpected response from the foojay Disco API")?;
    let info = infos.result.into_iter().next().with_context(|| format!("the foojay Disco API returned no download for {what}"))?;
    let url = info.direct_download_uri;
    if info.checksum_type.eq_ignore_ascii_case("sha256") && !info.checksum.is_empty() {
        return Ok((url, info.checksum));
    }
    let mut files = Vec::new();
    if info.checksum_uri.contains("sha256") {
        files.push(info.checksum_uri.clone());
    }
    files.extend(sibling_checksum_files(&url));
    for file in &files {
        if let Some(sha256) = crate::download::get_text(config, file).ok().as_deref().and_then(|text| checksum_in(text, 64)) {
            return Ok((url, sha256));
        }
    }
    if info.checksum_type.eq_ignore_ascii_case("sha1") && checksum_in(&info.checksum, 40).is_some() {
        return Ok((url, info.checksum.to_ascii_lowercase()));
    }
    bail!("{what} has no published checksum, so Kargo can't verify its download; pick another vendor or version")
}

/// The checksum files vendors put next to an archive: `<archive>.sha256`, `<archive>.sha256.txt`
/// and `<archive without its extension>.sha256.txt`.
fn sibling_checksum_files(url: &str) -> Vec<String> {
    let mut files = vec![format!("{url}.sha256"), format!("{url}.sha256.txt")];
    if let Some(stem) = url.strip_suffix(".zip").or_else(|| url.strip_suffix(".tar.gz")) {
        files.push(format!("{stem}.sha256.txt"));
    }
    files
}

/// The checksum in a checksum file: its first hex word of `digits` digits (64 for SHA-256, 40 for SHA-1).
fn checksum_in(text: &str, digits: usize) -> Option<String> {
    text.split_whitespace().find(|w| w.len() == digits && w.chars().all(|c| c.is_ascii_hexdigit())).map(str::to_ascii_lowercase)
}

/// Amazon's download site, whose `latest` archives come with their SHA-256; the foojay Disco API has
/// no checksums for Corretto.
const CORRETTO: &str = "https://corretto.aws/downloads";

/// The newest Corretto of a feature version: `latest/<file>` and its `latest_sha256/<file>`. The
/// release is the version in the URL `latest/<file>` redirects to (`resources/<version>/...`).
fn corretto_latest(config: &Config, major: u32, os: &str, arch: &str) -> Result<JdkPackage> {
    let (os, arch, archive) = platform(os, arch);
    let file = format!("amazon-corretto-{major}-{arch}-{os}-jdk.{archive}");
    let what = format!("Corretto JDK {major}");
    let checksum = crate::download::get_text(config, &format!("{CORRETTO}/latest_sha256/{file}"))
        .with_context(|| format!("failed to look up the checksum of {what} for {os}/{arch}"))?;
    let sha256 = checksum_in(&checksum, 64).with_context(|| format!("Amazon published no SHA-256 for {what}"))?;
    let url = format!("{CORRETTO}/latest/{file}");
    let target = crate::download::final_url(config, &url).with_context(|| format!("failed to look up {what} for {os}/{arch}"))?;
    let version = corretto_version(&target).unwrap_or_else(|| major.to_string());
    Ok(JdkPackage {
        vendor: JdkVendor::Corretto,
        release: release_name(&version),
        major,
        url: target,
        sha256,
        size: 0,
        archive: ArchiveKind::from_name(&file)?,
    })
}

/// The version in a Corretto download URL: `.../resources/21.0.5.11.1/amazon-corretto-...`.
fn corretto_version(url: &str) -> Option<String> {
    let rest = &url[url.find("/resources/")? + "/resources/".len()..];
    let version = rest.split('/').next()?;
    (!version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| version.to_owned())
}

/// The newest JDK of `vendor` for a feature version, for `os`/`arch` (Adoptium names).
pub fn latest_for(config: &Config, vendor: JdkVendor, major: u32, os: &str, arch: &str) -> Result<JdkPackage> {
    if vendor == JdkVendor::Corretto {
        return corretto_latest(config, major, os, arch);
    }
    find(config, vendor, &major.to_string(), None, os, arch)
}

/// A JDK release of `vendor` such as `jdk-21.0.5+11`, for `os`/`arch` (Adoptium names).
pub fn release_for(config: &Config, vendor: JdkVendor, release: &str, os: &str, arch: &str) -> Result<JdkPackage> {
    if vendor == JdkVendor::Corretto {
        bail!(
            "Kargo installs Corretto by feature version only, since Amazon publishes checksums for the newest release alone; \
             use a version like `21` instead of `{release}`"
        );
    }
    let version = release.strip_prefix("jdk-").unwrap_or(release);
    find(config, vendor, version, Some(release), os, arch)
}

fn find(config: &Config, vendor: JdkVendor, version: &str, release: Option<&str>, os: &str, arch: &str) -> Result<JdkPackage> {
    let (disco_os, disco_arch, archive) = platform(os, arch);
    let title = vendor.title();
    let what = match release {
        Some(release) => format!("{title} {release}"),
        None => format!("{title} JDK {version}"),
    };
    let latest = if release.is_some() { "" } else { "&latest=available" };
    let url = format!(
        "{DISCO_API}/packages?version={}&distribution={}&architecture={disco_arch}&archive_type={archive}&operating_system={disco_os}&package_type=jdk&release_status=ga{latest}",
        version.replace('+', "%2B"),
        vendor.disco_distribution(),
    );
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up {what}"))?;
    let package = choose(&body, release)?.with_context(|| {
        format!("no {what} build is available for {disco_os}/{disco_arch}; see `kargo toolchain available jdk --vendor {vendor}`")
    })?;
    let (url, sha256) = download(config, &package.id, &what)?;
    Ok(JdkPackage {
        vendor,
        release: release_name(&package.java_version),
        major: package.major_version,
        url,
        sha256,
        size: package.size,
        archive: ArchiveKind::from_name(&package.filename)?,
    })
}

#[derive(Debug, Deserialize)]
struct MajorVersions {
    result: Vec<MajorVersion>,
}

#[derive(Debug, Deserialize)]
struct MajorVersion {
    major_version: u32,
}

/// The feature versions `vendor` publishes for this OS and CPU, oldest first.
pub fn available(config: &Config, vendor: JdkVendor) -> Result<Vec<u32>> {
    let (os, arch) = crate::adoptium::platform()?;
    let (os, arch, archive) = platform(os, arch);
    let url = format!(
        "{DISCO_API}/packages?distribution={}&architecture={arch}&archive_type={archive}&operating_system={os}&package_type=jdk&release_status=ga&latest=available",
        vendor.disco_distribution()
    );
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up the {} JDKs", vendor.title()))?;
    parse_majors(&body)
}

fn parse_majors(json: &str) -> Result<Vec<u32>> {
    let versions: MajorVersions = serde_json::from_str(json).context("unexpected response from the foojay Disco API")?;
    let mut majors: Vec<u32> = versions.result.into_iter().map(|v| v.major_version).collect();
    majors.sort_unstable();
    majors.dedup();
    Ok(majors)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGES: &str = r#"{"result": [
        {"id": "a", "java_version": "21.0.4+7", "major_version": 21, "filename": "amazon-corretto-21.0.4.7.1-linux-x64.tar.gz", "latest_build_available": false},
        {"id": "b", "java_version": "21.0.5+11", "major_version": 21, "filename": "amazon-corretto-21.0.5.11.1-linux-x64.tar.gz", "latest_build_available": true, "size": 5},
        {"id": "c", "java_version": "21.0.5+11", "major_version": 21, "filename": "fx.tar.gz", "javafx_bundled": true, "latest_build_available": true},
        {"id": "d", "java_version": "21.0.6+1", "major_version": 21, "filename": "corretto.msi"}
    ]}"#;

    #[test]
    fn chooses_the_newest_build_without_javafx() {
        let newest = choose(PACKAGES, None).unwrap().unwrap();
        assert_eq!((newest.id.as_str(), newest.size), ("b", 5));
        assert_eq!(choose(PACKAGES, Some("jdk-21.0.4+7")).unwrap().unwrap().id, "a");
        // An installer is not an archive Kargo can unpack.
        assert!(choose(PACKAGES, Some("jdk-21.0.6+1")).unwrap().is_none());
        assert!(choose(r#"{"result": []}"#, None).unwrap().is_none());
        assert!(choose("{", None).is_err());
    }

    #[test]
    fn reads_checksum_files() {
        let sum = "A".repeat(64);
        assert_eq!(checksum_in(&format!("{sum}  sapmachine.zip\n"), 64), Some(sum.to_ascii_lowercase()));
        assert_eq!(checksum_in("not a checksum", 64), None);
        assert_eq!(checksum_in(&"b".repeat(40), 40), Some("b".repeat(40)));
        assert_eq!(
            sibling_checksum_files("https://x/sapmachine-jdk-21_windows-x64_bin.zip"),
            [
                "https://x/sapmachine-jdk-21_windows-x64_bin.zip.sha256",
                "https://x/sapmachine-jdk-21_windows-x64_bin.zip.sha256.txt",
                "https://x/sapmachine-jdk-21_windows-x64_bin.sha256.txt"
            ]
        );
    }

    #[test]
    fn reads_corretto_versions() {
        let url = "https://corretto.aws/downloads/resources/21.0.13.0.11/amazon-corretto-21.0.13.0.11-windows-x64-jdk.zip";
        assert_eq!(corretto_version(url).as_deref(), Some("21.0.13.0.11"));
        assert_eq!(corretto_version("https://corretto.aws/downloads/latest/amazon-corretto-21-x64-windows-jdk.zip"), None);
    }

    #[test]
    fn maps_platforms_and_lists_majors() {
        assert_eq!(platform("mac", "aarch64"), ("macos", "aarch64", "tar.gz"));
        assert_eq!(platform("windows", "x64"), ("windows", "x64", "zip"));
        let json = r#"{"result": [{"major_version": 21}, {"major_version": 17}, {"major_version": 21}]}"#;
        assert_eq!(parse_majors(json).unwrap(), [17, 21]);
        assert_eq!(release_name("21.0.5+11"), "jdk-21.0.5+11");
    }
}
