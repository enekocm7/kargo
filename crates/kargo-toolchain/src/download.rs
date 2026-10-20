//! Plain HTTP(S) downloads for toolchain archives, verified by SHA-256.
//!
//! A transient failure (a dropped connection, HTTP 429, a 5xx) is retried with the same policy
//! as dependency downloads, so a 200 MB JDK doesn't fail on the first hiccup.
//!
//! Every URL first goes through `[toolchain-mirrors]` (see [`kargo_util::settings`]), and a
//! request to a URL that `[mirrors.credentials]` covers carries those credentials.

use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use kargo_util::config::Config;
use kargo_util::hash;
use kargo_util::retry;

fn agent(config: &Config) -> ureq::Agent {
    kargo_util::http::agent(config, Duration::from_secs(1800))
}

/// Where Kargo actually downloads `url` from: its toolchain mirror, if one is configured.
pub fn mirrored(config: &Config, url: &str) -> String {
    config.settings().toolchain_url(url)
}

fn ensure_online(config: &Config, url: &str) -> Result<()> {
    if config.offline() {
        bail!("cannot download {url}: Kargo is running in offline mode; run `kargo fetch` while online first");
    }
    Ok(())
}

/// A failed attempt at a request, and whether another attempt may succeed.
enum Failure {
    /// A dropped connection or a transient HTTP status.
    Transient(anyhow::Error),
    /// Anything else: a 404, or a file that can't be written.
    Permanent(anyhow::Error),
}

/// Sends a GET for `url`. A transient status is a [`Failure::Transient`]; any other response,
/// including a 404, is returned for the caller to judge.
fn send(config: &Config, url: &str) -> Result<ureq::http::Response<ureq::Body>, Failure> {
    let mut request = agent(config).get(url);
    if let Some((username, password)) = config.settings().credentials(url) {
        request = request.header("Authorization", kargo_util::http::basic_auth(&username, &password));
    }
    match request.call() {
        Ok(response) => {
            let status = response.status().as_u16();
            if retry::is_transient_status(status) {
                Err(Failure::Transient(anyhow!("unexpected HTTP {status} from {url}")))
            } else {
                Ok(response)
            }
        }
        Err(e) => Err(Failure::Transient(anyhow::Error::new(e).context(format!("failed to connect to {url}")))),
    }
}

/// Runs `attempt` until it succeeds or fails permanently, retrying transient failures with the
/// backoff of [`retry`].
fn retrying<T>(config: &Config, url: &str, mut attempt: impl FnMut() -> Result<T, Failure>) -> Result<T> {
    let mut retries = 0;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(Failure::Transient(err)) if retries < retry::MAX_RETRIES => {
                let delay = retry::backoff(retries);
                retries += 1;
                config.shell().verbose("Retrying", format!("{url} in {:.1}s: {err:#}", delay.as_secs_f64()));
                std::thread::sleep(delay);
            }
            Err(Failure::Transient(err) | Failure::Permanent(err)) => return Err(err),
        }
    }
}

/// The file a `file://` URL names. Lets tests and mirrors serve a release from a directory.
fn local_path(url: &str) -> Option<std::path::PathBuf> {
    let path = url.strip_prefix("file://").or_else(|| url.strip_prefix("file:"))?;
    // `file:///C:/x` -> `/C:/x`: drop the slash before a drive letter.
    let bytes = path.as_bytes();
    let path = if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':' { &path[1..] } else { path };
    Some(std::path::PathBuf::from(path))
}

/// Fetches a small text resource once, giving up after `timeout`. Unlike [`get_text`] it neither retries nor honors offline
/// mode, so callers that want a quick best-effort answer (the check for a newer Kargo) decide that themselves.
pub fn get_text_within(url: &str, timeout: Duration) -> Result<String> {
    if let Some(path) = local_path(url) {
        return std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(concat!("kargo/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let response = agent.get(url).call().with_context(|| format!("failed to connect to {url}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("unexpected HTTP {status} from {url}");
    }
    let mut body = String::new();
    response.into_body().into_reader().take(1024 * 1024).read_to_string(&mut body).with_context(|| format!("failed to read {url}"))?;
    Ok(body)
}

/// Fetches a small text resource (e.g. an API response).
pub fn get_text(config: &Config, url: &str) -> Result<String> {
    let url = &mirrored(config, url);
    if let Some(path) = local_path(url) {
        return std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()));
    }
    ensure_online(config, url)?;
    config.shell().verbose("Fetching", url);
    retrying(config, url, || {
        let response = send(config, url)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(Failure::Permanent(anyhow!("not found: {url}")));
        }
        if !(200..300).contains(&status) {
            return Err(Failure::Permanent(anyhow!("unexpected HTTP {status} from {url}")));
        }
        let mut body = String::new();
        response
            .into_body()
            .into_reader()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut body)
            .map_err(|e| Failure::Transient(anyhow::Error::new(e).context(format!("failed to read {url}"))))?;
        Ok(body)
    })
}

/// Downloads `url` to `dest`, verifying `sha256` when given. The file only
/// appears at `dest` once it is complete and verified.
pub fn download(config: &Config, url: &str, dest: &Path, sha256: Option<&str>) -> Result<()> {
    let url = &mirrored(config, url);
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(source) = local_path(url) {
        config.shell().status("Copying", &name);
        kargo_util::fs::write_atomic_if_absent(dest, |f| io::copy(&mut std::fs::File::open(&source)?, f).map(|_| ()))
            .with_context(|| format!("failed to read {}", source.display()))?;
    } else {
        ensure_online(config, url)?;
        config.shell().status("Downloading", if config.shell().is_verbose() { url } else { &name });
        retrying(config, url, || fetch_to(config, url, dest))?;
    }
    if let Some(expected) = sha256 {
        verify(dest, expected).inspect_err(|_| {
            let _ = std::fs::remove_file(dest);
        })?;
    }
    Ok(())
}

/// One attempt at downloading `url` to `dest`.
fn fetch_to(config: &Config, url: &str, dest: &Path) -> Result<(), Failure> {
    let response = send(config, url)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Failure::Permanent(anyhow!("unexpected HTTP {status} downloading {url}")));
    }
    let mut reader = response.into_body().into_reader();
    // A connection dropped while the body streams is worth another attempt; a file that can't be
    // written is not. Either way nothing is left at `dest`.
    let mut body_failed = false;
    let written =
        kargo_util::fs::write_atomic_if_absent(dest, |f| io::copy(&mut reader, f).map(|_| ()).inspect_err(|_| body_failed = true));
    written.map_err(|e| {
        let err = e.context(format!("failed to download {url}"));
        if body_failed { Failure::Transient(err) } else { Failure::Permanent(err) }
    })
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    /// Serves one answer per connection, in order, on a loopback port; returns the base URL.
    fn serve(answers: Vec<(u16, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let response = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        url
    }

    fn quiet(dir: &Path) -> Config {
        Config::new(dir.to_path_buf(), Shell::new(Verbosity::Quiet))
    }

    #[test]
    fn file_urls_name_local_files() {
        assert_eq!(local_path("file:///tmp/r/x").unwrap(), std::path::PathBuf::from("/tmp/r/x"));
        assert_eq!(local_path("file:///C:/r/x").unwrap(), std::path::PathBuf::from("C:/r/x"));
        assert!(local_path("https://example.com/x").is_none());
        let dir = tempfile::tempdir().unwrap();
        let text = format!("0.2.0{}", char::from(10));
        std::fs::write(dir.path().join("v.txt"), &text).unwrap();
        let url = format!("file://{}", dir.path().join("v.txt").display()).replace(char::from(92), "/");
        assert_eq!(get_text_within(&url, Duration::from_secs(1)).unwrap(), text);
        assert_eq!(get_text(&quiet(dir.path()).with_offline(true), &url).unwrap(), text);
        let dest = dir.path().join("copy.txt");
        download(&quiet(dir.path()), &url, &dest, Some(&kargo_util::hash::sha256_bytes(text.as_bytes()))).unwrap();
        assert!(download(&quiet(dir.path()), &url, &dir.path().join("bad.txt"), Some("00")).is_err());
    }

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
        let config = quiet(dir.path()).with_offline(true);
        let err = download(&config, "https://example.invalid/x.zip", &dir.path().join("x.zip"), None).unwrap_err();
        assert!(err.to_string().contains("offline"));
        assert!(get_text(&config, "https://example.invalid/").is_err());
    }

    #[test]
    fn retries_transient_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let url = serve(vec![(503, ""), (429, ""), (200, "ok")]);
        assert_eq!(get_text(&quiet(dir.path()), &url).unwrap(), "ok");
    }

    #[test]
    fn does_not_retry_a_missing_resource() {
        let dir = tempfile::tempdir().unwrap();
        // The second answer would succeed, so a retry would show up as Ok.
        let url = serve(vec![(404, ""), (200, "ok")]);
        let err = get_text(&quiet(dir.path()), &url).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn downloads_from_a_toolchain_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = serve(vec![(200, "from the mirror")]);
        let mut settings = kargo_util::settings::Settings::default();
        settings
            .merge_str(
                &format!(
                    "[toolchain-mirrors]
\"https://services.gradle.org\" = \"{mirror}/gradle\"
"
                ),
                dir.path(),
            )
            .unwrap();
        let config = quiet(dir.path()).with_settings(settings).unwrap();
        assert_eq!(mirrored(&config, "https://services.gradle.org/distributions/g.zip"), format!("{mirror}/gradle/distributions/g.zip"));
        let dest = dir.path().join("g.zip");
        download(&config, "https://services.gradle.org/distributions/g.zip", &dest, None).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "from the mirror");
    }

    #[test]
    fn retries_a_download_then_writes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let url = serve(vec![(503, ""), (200, "payload")]);
        let dest = dir.path().join("x.zip");
        download(&quiet(dir.path()), &format!("{url}/x.zip"), &dest, None).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "payload");
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no partial files left behind");
    }
}
