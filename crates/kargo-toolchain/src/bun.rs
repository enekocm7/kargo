//! Pinned Bun distribution, the bundler of browser apps (`bundler = "bun"`) when no `bun` is on `PATH`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kargo_util::config::Config;

use crate::{archive, download, install_lock};

/// Bun release downloaded when no `bun` is on `PATH`.
pub const VERSION: &str = "1.2.23";

/// The release asset for this machine, such as `bun-linux-x64.zip`.
fn asset() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "bun-linux-x64.zip",
        ("linux", "aarch64") => "bun-linux-aarch64.zip",
        ("macos", "x86_64") => "bun-darwin-x64.zip",
        ("macos", "aarch64") => "bun-darwin-aarch64.zip",
        // Windows on Arm runs the x64 build.
        ("windows", "x86_64" | "aarch64") => "bun-windows-x64.zip",
        (os, arch) => bail!("Bun is not available for {os}/{arch}; install it and put `bun` on PATH"),
    })
}

fn root(config: &Config) -> PathBuf {
    config.toolchains_dir().join("bun").join(VERSION)
}

fn find_in(dir: &Path) -> Option<PathBuf> {
    let exe = dir.join(if cfg!(windows) { "bun.exe" } else { "bun" });
    exe.is_file().then_some(exe)
}

/// The installed pinned `bun`, if it is installed.
pub fn find(config: &Config) -> Option<PathBuf> {
    find_in(&root(config))
}

/// The line of `sums` (a release's `SHASUMS256.txt`) that gives the SHA-256 of `asset`.
fn checksum(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (sum, name) = (parts.next()?, parts.next()?);
        (name.trim_start_matches('*') == asset && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| sum.to_ascii_lowercase())
    })
}

/// Finds or installs the pinned `bun`, checking the SHA-256 the release publishes.
pub fn ensure(config: &Config) -> Result<PathBuf> {
    let asset = asset()?;
    let root = root(config);
    if let Some(path) = find_in(&root) {
        return Ok(path);
    }
    if config.offline() {
        bail!("Bun {VERSION} is not installed and Kargo is offline; install `bun` and put it on PATH, or run `kargo fetch` while online");
    }
    crate::ensure_auto_install(config, &format!("Bun {VERSION}"), "kargo fetch")?;
    let _lock = install_lock(config, &format!("bun-{VERSION}"))?;
    if let Some(path) = find_in(&root) {
        return Ok(path);
    }
    let base = format!("https://github.com/oven-sh/bun/releases/download/bun-v{VERSION}");
    let sums = download::get_text(config, &format!("{base}/SHASUMS256.txt")).context("failed to fetch the Bun checksums")?;
    let expected = checksum(&sums, asset).with_context(|| format!("the Bun {VERSION} checksums do not list {asset}"))?;
    let archive_path = config.toolchains_dir().join("downloads").join(asset);
    download::download(config, &format!("{base}/{asset}"), &archive_path, Some(&expected))?;
    archive::install(&archive_path, archive::ArchiveKind::Zip, &root)?;
    find_in(&root).context("the Bun archive did not contain bun")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_checksum_of_an_asset() {
        let sums = format!(
            "{}  bun-darwin-aarch64-profile.zip
{}  bun-linux-x64.zip
not a line
",
            "a".repeat(64),
            "B".repeat(64)
        );
        assert_eq!(checksum(&sums, "bun-linux-x64.zip"), Some("b".repeat(64)));
        assert_eq!(checksum(&sums, "bun-linux-x64-baseline.zip"), None);
        assert_eq!(checksum(&format!("{} *bun-windows-x64.zip", "c".repeat(64)), "bun-windows-x64.zip"), Some("c".repeat(64)));
        assert_eq!(checksum("abc  bun-linux-x64.zip", "bun-linux-x64.zip"), None, "a checksum has 64 hex digits");
    }

    #[test]
    fn finds_bun_in_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_in(dir.path()), None);
        let exe = dir.path().join(if cfg!(windows) { "bun.exe" } else { "bun" });
        std::fs::write(&exe, "").unwrap();
        assert_eq!(find_in(dir.path()), Some(exe));
    }
}
