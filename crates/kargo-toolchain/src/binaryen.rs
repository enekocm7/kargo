//! Pinned Binaryen distribution used to optimize Kotlin/Wasm release output.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kargo_util::config::Config;

use crate::{archive, download, install_lock};

/// Binaryen release pinned for reproducible Wasm optimization.
pub const VERSION: u32 = 133;

fn asset() -> Result<&'static str> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        other => bail!("Binaryen is not available for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" if os == "macos" => "arm64",
        "aarch64" => "aarch64",
        other => bail!("Binaryen is not available for {other}"),
    };
    Ok(match (os, arch) {
        ("linux", "x86_64") => "binaryen-version_133-x86_64-linux.tar.gz",
        ("linux", "aarch64") => "binaryen-version_133-aarch64-linux.tar.gz",
        ("macos", "x86_64") => "binaryen-version_133-x86_64-macos.tar.gz",
        ("macos", "arm64") => "binaryen-version_133-arm64-macos.tar.gz",
        ("windows", "x86_64") => "binaryen-version_133-x86_64-windows.tar.gz",
        ("windows", "aarch64") => "binaryen-version_133-arm64-windows.tar.gz",
        _ => bail!("Binaryen is not available for {os}/{arch}"),
    })
}

fn find_opt(dir: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "wasm-opt.exe" } else { "wasm-opt" };
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|n| n == exe) {
                return Some(path);
            }
        }
    }
    None
}

/// The installed pinned `wasm-opt`, if it is installed.
pub fn find(config: &Config) -> Option<PathBuf> {
    find_opt(&root(config))
}

fn root(config: &Config) -> PathBuf {
    config.toolchains_dir().join("binaryen").join(VERSION.to_string())
}

/// Finds or installs the pinned `wasm-opt`, checking the official release SHA-256 sidecar.
pub fn ensure(config: &Config) -> Result<PathBuf> {
    let archive_name = asset()?;
    let root = root(config);
    if let Some(path) = find_opt(&root) {
        return Ok(path);
    }
    if config.offline() {
        bail!("Binaryen {VERSION} is not installed and Kargo is offline; run `kargo build --release` once while online");
    }
    crate::ensure_auto_install(config, &format!("Binaryen {VERSION}"), "kargo build --release")?;
    let _lock = install_lock(config, &format!("binaryen-{VERSION}"))?;
    if let Some(path) = find_opt(&root) {
        return Ok(path);
    }
    let url = format!("https://github.com/WebAssembly/binaryen/releases/download/version_{VERSION}/{archive_name}");
    let sum = download::get_text(config, &format!("{url}.sha256")).context("failed to fetch Binaryen SHA-256")?;
    let expected = sum
        .split_whitespace()
        .next()
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .context("Binaryen published an invalid SHA-256 sidecar")?;
    let archive_path = config.toolchains_dir().join("downloads").join(archive_name);
    download::download(config, &url, &archive_path, Some(expected))?;
    archive::install(&archive_path, archive::ArchiveKind::TarGz, &root)?;
    find_opt(&root).context("the Binaryen archive did not contain wasm-opt")
}
