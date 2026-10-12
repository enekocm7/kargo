//! Maven Central through the Central Portal (`central.sonatype.com`): a bundle of every
//! module's files (with checksums and signatures) is uploaded in one request, then the
//! Portal validates it and either waits for a release from its web UI or publishes it.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::Deserialize;

use super::repo::MavenClient;
use kargo_util::shell::Shell;

/// The Central Portal.
pub const PORTAL_URL: &str = "https://central.sonatype.com";
/// Environment variable overriding the Portal's URL (for a staging Portal or a test server).
pub const URL_ENV: &str = "KARGO_MAVEN_CENTRAL_URL";
/// Environment variable holding the username part of a Portal user token.
pub const USERNAME_ENV: &str = "KARGO_MAVEN_CENTRAL_USERNAME";
/// Environment variable holding the password part of a Portal user token.
pub const PASSWORD_ENV: &str = "KARGO_MAVEN_CENTRAL_PASSWORD";

/// Where the Portal lists deployments.
pub const DEPLOYMENTS_PAGE: &str = "https://central.sonatype.com/publishing/deployments";

/// How long `kargo publish` follows a deployment before leaving it to the Portal.
const WAIT_LIMIT: Duration = Duration::from_secs(30 * 60);

/// What the Portal does with a bundle once it is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishingType {
    /// Waits for someone to press "Publish" in the Portal (`USER_MANAGED`).
    Manual,
    /// Publishes it right away (`AUTOMATIC`).
    Automatic,
}

impl PublishingType {
    fn as_param(self) -> &'static str {
        match self {
            PublishingType::Manual => "USER_MANAGED",
            PublishingType::Automatic => "AUTOMATIC",
        }
    }
}

/// A deployment's state, as `/api/v1/publisher/status` reports it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatus {
    pub deployment_id: String,
    #[serde(default)]
    pub deployment_name: String,
    /// `PENDING`, `VALIDATING`, `VALIDATED`, `PUBLISHING`, `PUBLISHED` or `FAILED`.
    pub deployment_state: String,
    #[serde(default)]
    pub purls: Vec<String>,
    #[serde(default)]
    pub errors: Option<serde_json::Value>,
}

impl DeploymentStatus {
    /// The validation errors, one per line.
    pub fn error_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.errors {
            Some(serde_json::Value::Object(map)) => {
                for (component, errors) in map {
                    match errors {
                        serde_json::Value::Array(list) => out.extend(
                            list.iter().map(|e| format!("{component}: {}", e.as_str().map(str::to_owned).unwrap_or(e.to_string()))),
                        ),
                        other => out.push(format!("{component}: {other}")),
                    }
                }
            }
            Some(serde_json::Value::Null) | None => {}
            Some(other) => out.push(other.to_string()),
        }
        out
    }
}

/// A client of the Central Portal's Publisher API.
pub struct Portal<'a> {
    client: &'a MavenClient,
    base: String,
    token: String,
    /// How often [`Portal::wait`] asks for the deployment's state.
    poll_interval: Duration,
}

impl<'a> Portal<'a> {
    /// A Portal client with the user token from `KARGO_MAVEN_CENTRAL_USERNAME` and `KARGO_MAVEN_CENTRAL_PASSWORD`.
    pub fn from_env(client: &'a MavenClient) -> Result<Self> {
        let (username, password) = credentials_from_env()?;
        let base = std::env::var(URL_ENV).ok().filter(|u| !u.is_empty()).unwrap_or_else(|| PORTAL_URL.to_owned());
        Ok(Self::new(client, &base, &username, &password))
    }

    pub fn new(client: &'a MavenClient, base: &str, username: &str, password: &str) -> Self {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        Self { client, base: base.trim_end_matches('/').to_owned(), token, poll_interval: Duration::from_secs(5) }
    }

    /// Uploads `bundle` (a zip of the Maven repository layout) and returns the deployment id.
    pub fn upload(&self, bundle: &Path, name: &str, kind: PublishingType) -> Result<String> {
        if self.client.offline() {
            bail!("cannot publish to Maven Central in offline mode");
        }
        let data = std::fs::read(bundle).with_context(|| format!("failed to read {}", bundle.display()))?;
        let boundary = format!("kargo-{:016x}", data.len() as u64 ^ 0x6b61_7267_6f5f_6275);
        let mut body = Vec::with_capacity(data.len() + 512);
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bundle\"; filename=\"bundle.zip\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )?;
        body.extend_from_slice(&data);
        write!(body, "\r\n--{boundary}--\r\n")?;
        let url = format!("{}/api/v1/publisher/upload?name={}&publishingType={}", self.base, query_escape(name), kind.as_param());
        let response = self
            .client
            .agent()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..])
            .with_context(|| format!("failed to connect to {}", self.base))?;
        let status = response.status().as_u16();
        let text = read_body(response);
        match status {
            200..=299 => {
                let id = text.trim().trim_matches('"').to_owned();
                if id.is_empty() {
                    bail!("the Central Portal accepted the bundle but returned no deployment id");
                }
                Ok(id)
            }
            401 | 403 => bail!(
                "the Central Portal refused the credentials (HTTP {status}); set {USERNAME_ENV} and {PASSWORD_ENV} to the two parts of a user token generated at {PORTAL_URL}/account"
            ),
            _ => bail!("the Central Portal rejected the bundle (HTTP {status}){}", detail(&text)),
        }
    }

    /// The current state of deployment `id`.
    pub fn status(&self, id: &str) -> Result<DeploymentStatus> {
        let url = format!("{}/api/v1/publisher/status?id={}", self.base, query_escape(id));
        let response = self
            .client
            .agent()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send_empty()
            .with_context(|| format!("failed to connect to {}", self.base))?;
        let status = response.status().as_u16();
        let text = read_body(response);
        if !(200..=299).contains(&status) {
            bail!("could not read the state of deployment {id} from the Central Portal (HTTP {status}){}", detail(&text));
        }
        serde_json::from_str(&text).with_context(|| format!("unexpected deployment status from the Central Portal: {text}"))
    }

    /// Follows deployment `id` until the Portal has validated it (and, for an automatic release,
    /// started publishing it). Fails with the Portal's validation errors.
    pub fn wait(&self, id: &str, kind: PublishingType, shell: &Shell) -> Result<DeploymentStatus> {
        let start = Instant::now();
        let mut last = String::new();
        loop {
            let status = self.status(id)?;
            if status.deployment_state != last {
                shell.verbose("Deployment", format!("{id} is {}", status.deployment_state));
                last = status.deployment_state.clone();
            }
            match (status.deployment_state.as_str(), kind) {
                ("FAILED", _) => {
                    let errors = status.error_lines();
                    let list = if errors.is_empty() { String::new() } else { format!(":\n  {}", errors.join("\n  ")) };
                    bail!("Maven Central rejected deployment {id}{list}");
                }
                ("VALIDATED", PublishingType::Manual) | ("PUBLISHING" | "PUBLISHED", _) => return Ok(status),
                _ => {}
            }
            if start.elapsed() > WAIT_LIMIT {
                bail!(
                    "deployment {id} is still {} after {} minutes; follow it at {DEPLOYMENTS_PAGE}",
                    status.deployment_state,
                    WAIT_LIMIT.as_secs() / 60
                );
            }
            std::thread::sleep(self.poll_interval);
        }
    }
}

/// The user token from the environment, or an error naming the variables to set.
pub fn credentials_from_env() -> Result<(String, String)> {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    match (get(USERNAME_ENV), get(PASSWORD_ENV)) {
        (Some(u), Some(p)) => Ok((u, p)),
        _ => bail!(
            "publishing to Maven Central needs a Central Portal user token; generate one at {PORTAL_URL}/account and export its parts as {USERNAME_ENV} and {PASSWORD_ENV}"
        ),
    }
}

/// Zips the repository layout under `dir` (`com/example/lib/1.0/...`) into `dest`, the bundle the Portal takes.
pub fn write_bundle(dir: &Path, dest: &Path) -> Result<()> {
    let mut files: Vec<_> =
        walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).map(|e| e.into_path()).collect();
    files.sort();
    kargo_util::fs::write_atomic(dest, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let to_io = |e: zip::result::ZipError| std::io::Error::other(e);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        for path in &files {
            let rel = path.strip_prefix(dir).map_err(std::io::Error::other)?;
            zip.start_file(rel.to_string_lossy().replace('\\', "/"), options).map_err(to_io)?;
            std::io::copy(&mut std::fs::File::open(path)?, &mut zip)?;
        }
        zip.finish().map_err(to_io)?;
        Ok(())
    })
    .with_context(|| format!("failed to write {}", dest.display()))
}

fn read_body(response: ureq::http::Response<ureq::Body>) -> String {
    let mut text = String::new();
    let _ = response.into_body().into_reader().take(64 * 1024).read_to_string(&mut text);
    text
}

fn detail(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() { String::new() } else { format!(": {text}") }
}

/// Percent-encodes a query parameter value.
fn query_escape(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_deployment_status_and_errors() {
        let json = r#"{"deploymentId":"28570f16","deploymentName":"com.acme:lib:1.0","deploymentState":"FAILED",
            "purls":[],"errors":{"pkg:maven/com.acme/lib@1.0":["Missing signature for file: lib-1.0.jar","License information is missing"]}}"#;
        let status: DeploymentStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.deployment_state, "FAILED");
        assert_eq!(
            status.error_lines(),
            [
                "pkg:maven/com.acme/lib@1.0: Missing signature for file: lib-1.0.jar",
                "pkg:maven/com.acme/lib@1.0: License information is missing"
            ]
        );
        let ok: DeploymentStatus = serde_json::from_str(r#"{"deploymentId":"1","deploymentState":"VALIDATED"}"#).unwrap();
        assert!(ok.error_lines().is_empty());
    }

    #[test]
    fn uploads_a_bundle_and_follows_the_deployment() {
        use crate::testserver::TestServer;
        use kargo_util::config::Config;
        use kargo_util::shell::Verbosity;
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = polls.clone();
        let server = TestServer::start(move |r| {
            if r.header("authorization") != Some("Bearer dXNlcjpwYXNz") {
                return (401, String::new());
            }
            if r.path.starts_with("/api/v1/publisher/upload") {
                return (201, "dep-1".into());
            }
            let state = if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { "VALIDATING" } else { "VALIDATED" };
            (200, format!(r#"{{"deploymentId":"dep-1","deploymentState":"{state}"}}"#))
        });
        let dir = tempfile::tempdir().unwrap();
        let config = Config::new(dir.path().join("home"), Shell::new(Verbosity::Quiet));
        let client = MavenClient::new(&config, vec![]);
        let bundle = dir.path().join("bundle.zip");
        std::fs::write(&bundle, "zip bytes").unwrap();

        let mut portal = Portal::new(&client, &server.url, "user", "pass");
        portal.poll_interval = Duration::ZERO;
        let id = portal.upload(&bundle, "com.acme:lib:1.0", PublishingType::Manual).unwrap();
        assert_eq!(id, "dep-1");
        let upload = &server.requests()[0];
        assert_eq!(upload.path, "/api/v1/publisher/upload?name=com.acme%3Alib%3A1.0&publishingType=USER_MANAGED");
        let body = String::from_utf8_lossy(&upload.body);
        assert!(upload.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));
        assert!(body.contains("name=\"bundle\"; filename=\"bundle.zip\"") && body.contains("zip bytes"), "{body}");

        let status = portal.wait(&id, PublishingType::Manual, client.shell()).unwrap();
        assert_eq!(status.deployment_state, "VALIDATED");
        assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 2);

        let wrong = Portal::new(&client, &server.url, "user", "nope");
        assert!(wrong.upload(&bundle, "x", PublishingType::Automatic).unwrap_err().to_string().contains(USERNAME_ENV));
    }

    #[test]
    fn escapes_query_values() {
        assert_eq!(query_escape("com.acme:lib:1.0"), "com.acme%3Alib%3A1.0");
        assert_eq!(query_escape("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn bundles_the_repository_layout() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        std::fs::create_dir_all(staged.join("com/acme/lib/1.0")).unwrap();
        std::fs::write(staged.join("com/acme/lib/1.0/lib-1.0.pom"), "<project/>").unwrap();
        let bundle = dir.path().join("bundle.zip");
        write_bundle(&staged, &bundle).unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&bundle).unwrap()).unwrap();
        let mut text = String::new();
        archive.by_name("com/acme/lib/1.0/lib-1.0.pom").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "<project/>");
    }
}
