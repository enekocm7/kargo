//! Plain HTTP(S) downloads for toolchain archives, verified by SHA-256.

use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use kargo_util::config::Config;
use kargo_util::hash;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(1800)))
        .http_status_as_error(false)
        .user_agent(concat!("kargo/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn ensure_online(config: &Config, url: &str) -> Result<()> {
    if config.offline() {
        bail!("cannot download {url}: Kargo is running in offline mode");
    }
    Ok(())
}

/// Fetches a small text resource (e.g. an API response).
pub fn get_text(config: &Config, url: &str) -> Result<String> {
    ensure_online(config, url)?;
    let response = agent().get(url).call().with_context(|| format!("failed to connect to {url}"))?;
    let status = response.status().as_u16();
    if status == 404 {
        bail!("not found: {url}");
    }
    if !(200..300).contains(&status) {
        bail!("unexpected HTTP {status} from {url}");
    }
    let mut body = String::new();
    response.into_body().into_reader().take(16 * 1024 * 1024).read_to_string(&mut body)?;
    Ok(body)
}

/// Downloads `url` to `dest`, verifying `sha256` when given. The file only
/// appears at `dest` once it is complete and verified.
pub fn download(config: &Config, url: &str, dest: &Path, sha256: Option<&str>) -> Result<()> {
    ensure_online(config, url)?;
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    config.shell().status("Downloading", &name);
    let response = agent().get(url).call().with_context(|| format!("failed to connect to {url}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("unexpected HTTP {status} downloading {url}");
    }
    let mut reader = response.into_body().into_reader();
    kargo_util::fs::write_atomic(dest, |f| io::copy(&mut reader, f).map(|_| ())).with_context(|| format!("failed to download {url}"))?;
    if let Some(expected) = sha256 {
        verify(dest, expected).inspect_err(|_| {
            let _ = std::fs::remove_file(dest);
        })?;
    }
    Ok(())
}

/// Checks a file's SHA-256 against `expected` (hex, case-insensitive).
pub fn verify(file: &Path, expected: &str) -> Result<()> {
    let actual = hash::sha256_file(file).with_context(|| format!("failed to hash {}", file.display()))?;
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!("checksum mismatch for {}: expected sha256 {expected}, got {actual}", file.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    #[test]
    fn verifies_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x");
        std::fs::write(&f, b"abc").unwrap();
        verify(&f, "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD").unwrap();
        assert!(verify(&f, "00").is_err());
    }

    #[test]
    fn offline_mode_refuses_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::new(dir.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
        let err = download(&config, "https://example.invalid/x.zip", &dir.path().join("x.zip"), None).unwrap_err();
        assert!(err.to_string().contains("offline"));
        assert!(get_text(&config, "https://example.invalid/").is_err());
    }
}
