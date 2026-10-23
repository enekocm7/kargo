//! JetBrains Runtime (JBR), the JDK whose `-XX:+AllowEnhancedClassRedefinition` lets Compose Hot
//! Reload replace classes whose methods and fields changed, not only method bodies. Downloaded on
//! demand into `~/.kargo/toolchains/jbr/<version>`, looked up with the foojay Disco API like GraalVM.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_util::config::Config;

use crate::archive::{self, ArchiveKind};
use crate::graalvm::{DISCO_API, parse_info, parse_packages};
use crate::install_lock;
use crate::jdk::Jdk;

/// The JVM option that turns on JBR's enhanced class redefinition.
pub const ENHANCED_REDEFINITION: &str = "-XX:+AllowEnhancedClassRedefinition";

/// Records the Java feature version of an installation, so it can be found without probing `java`.
const MAJOR_FILE: &str = "kargo-jbr-major";

fn java_in(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
}

/// The JDK home inside an installation (`Contents/Home` on macOS, or one directory down from the archive's root).
fn find_home(dir: &Path) -> Option<PathBuf> {
    let candidates = [dir.to_path_buf(), dir.join("Contents").join("Home")];
    candidates.into_iter().find(|c| java_in(c).is_file())
}

/// The installed JBRs of `major`, newest last.
fn installed(config: &Config, major: u32) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(jbr_dir(config)) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| std::fs::read_to_string(e.path().join(MAJOR_FILE)).ok().and_then(|m| m.trim().parse::<u32>().ok()) == Some(major))
        .filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), find_home(&e.path())?)))
        .collect();
    out.sort_by_key(|(version, _)| kargo_maven::Version::parse(version));
    out
}

fn jbr_dir(config: &Config) -> PathBuf {
    config.toolchains_dir().join("jbr")
}

/// The JetBrains Runtime of Java `major`, downloading the newest one when none is installed.
pub fn resolve(config: &Config, major: u32) -> Result<Jdk> {
    if let Some((_, home)) = installed(config, major).pop() {
        return Ok(Jdk { home, version: major });
    }
    if config.offline() {
        bail!("JetBrains Runtime {major} is not installed and Kargo is offline; run `kargo run --hot-reload` once while online");
    }
    crate::ensure_auto_install(config, &format!("JetBrains Runtime {major}"), "kargo run --hot-reload")?;
    install(config, major)
}

/// How a downloaded archive is checked.
enum Checksum {
    Sha256(String),
    Sha512(String),
}

/// The archive to download and its SHA-512 from the `.checksum` file JetBrains publishes next to it.
/// foojay lists the build with JCEF (`jbrsdk_jcef-`); the plain `jbrsdk-` is half the size and has
/// all Compose needs, so it is taken when JetBrains publishes one.
fn download_with_sha512(config: &Config, info: &str, filename: &str) -> Result<(String, Checksum)> {
    let value: serde_json::Value = serde_json::from_str(info).context("unexpected response from the foojay Disco API")?;
    let url = value["result"][0]["direct_download_uri"]
        .as_str()
        .context("the foojay Disco API returned no download for JetBrains Runtime")?
        .to_owned();
    let plain = url.replace("/jbrsdk_jcef-", "/jbrsdk-");
    for candidate in [plain, url.clone()] {
        let Ok(text) = crate::download::get_text(config, &format!("{candidate}.checksum")) else { continue };
        if let Some(sha512) = text.split_whitespace().next().filter(|h| h.len() == 128 && h.chars().all(|c| c.is_ascii_hexdigit())) {
            return Ok((candidate, Checksum::Sha512(sha512.to_owned())));
        }
    }
    bail!("JetBrains publishes no checksum for {filename}, so Kargo does not install it")
}

/// foojay's names for this OS and CPU.
fn platform() -> Result<(&'static str, &'static str)> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        other => bail!("JetBrains Runtime is not available for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => bail!("JetBrains Runtime is not available for the {other} architecture"),
    };
    Ok((os, arch))
}

fn install(config: &Config, major: u32) -> Result<Jdk> {
    let (os, arch) = platform()?;
    let archive_type = if os == "windows" { "zip" } else { "tar.gz" };
    let url = format!(
        "{DISCO_API}/packages?version={major}&distribution=jetbrains&architecture={arch}&archive_type={archive_type}&operating_system={os}&package_type=jdk&release_status=ga&latest=available"
    );
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up JetBrains Runtime {major}"))?;
    let (id, version, _, filename) =
        parse_packages(&body)?.with_context(|| format!("no JetBrains Runtime {major} build is available for {os}/{arch}"))?;
    if version.is_empty() || version.contains(['/', '\\', ':']) || version.starts_with('.') {
        bail!("invalid JetBrains Runtime version `{version}`");
    }
    let info = crate::download::get_text(config, &format!("{DISCO_API}/ids/{id}"))
        .with_context(|| format!("failed to look up the download of JetBrains Runtime {version}"))?;
    let (download, checksum) = match parse_info(&info) {
        Ok((url, sha256)) => (url, Checksum::Sha256(sha256)),
        // foojay lists JBR without checksums; JetBrains publishes a SHA-512 next to every archive.
        Err(_) => download_with_sha512(config, &info, &filename)?,
    };
    let filename = download.rsplit('/').next().unwrap_or(&filename).to_owned();
    let kind = ArchiveKind::from_name(&filename)?;

    let dir = jbr_dir(config).join(&version);
    let _lock = install_lock(config, &format!("jbr-{version}"))?;
    if let Some(home) = find_home(&dir).filter(|_| dir.join(MAJOR_FILE).is_file()) {
        return Ok(Jdk { home, version: major });
    }
    let archive_path = config.toolchains_dir().join("downloads").join(format!("jbr-{version}.{}", kind.extension()));
    match &checksum {
        Checksum::Sha256(sha256) => crate::download::download(config, &download, &archive_path, Some(sha256))?,
        Checksum::Sha512(sha512) => {
            let _ = std::fs::remove_file(&archive_path);
            crate::download::download(config, &download, &archive_path, None)?;
            let actual = kargo_util::hash::sha512_file(&archive_path)?;
            if !actual.eq_ignore_ascii_case(sha512) {
                let _ = std::fs::remove_file(&archive_path);
                bail!("the download of JetBrains Runtime {version} has SHA-512 {actual}, but JetBrains publishes {sha512}");
            }
        }
    }
    config.shell().status("Installing", format!("JetBrains Runtime {version}"));
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    archive::install(&archive_path, kind, &dir)?;
    let _ = std::fs::remove_file(&archive_path);
    let home = find_home(&dir).with_context(|| format!("the downloaded JetBrains Runtime {version} has no bin/java"))?;
    std::fs::write(dir.join(MAJOR_FILE), major.to_string())?;
    Ok(Jdk { home, version: major })
}
