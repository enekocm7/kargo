//! Remote Maven repositories and the local download cache.

use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use super::coord::ModuleId;
use super::version::{Version, select_latest};
use kargo_util::config::Config;
use kargo_util::shell::Shell;
use kargo_util::{fs as kfs, hash};

pub const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2";
pub const GOOGLE: &str = "https://dl.google.com/dl/android/maven2";

/// How long `maven-metadata.xml` (the version list) is trusted before re-checking.
const METADATA_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How many times a request that failed transiently (HTTP 429, 5xx, connection errors) is retried.
const MAX_RETRIES: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub url: String,
    pub credentials: Option<Credentials>,
}

impl Repository {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        let url = url.into();
        Self { name: name.into(), url: url.trim_end_matches('/').to_owned(), credentials: None }
    }

    pub fn maven_central() -> Self {
        Self::new("maven-central", MAVEN_CENTRAL)
    }

    pub fn google() -> Self {
        Self::new("google", GOOGLE)
    }

    fn is_local(&self) -> bool {
        self.url.starts_with("file:") || !self.url.contains("://")
    }

    fn local_root(&self) -> PathBuf {
        let path = self.url.strip_prefix("file://").or_else(|| self.url.strip_prefix("file:")).unwrap_or(&self.url);
        // `file:///C:/x` -> `/C:/x`; strip the leading slash before a drive letter.
        let path = if cfg!(windows) && path.len() > 2 && path.starts_with('/') && path.as_bytes()[2] == b':' { &path[1..] } else { path };
        PathBuf::from(path)
    }
}

/// Outcome of looking for a single remote file.
enum Fetch {
    Found,
    Missing,
}

/// Downloads artifacts from an ordered list of repositories into a shared cache.
pub struct MavenClient {
    repos: Vec<Repository>,
    cache: PathBuf,
    offline: bool,
    refresh: bool,
    agent: ureq::Agent,
    shell: Shell,
    /// Paths known to be missing from every repository during this run.
    missing: Mutex<HashSet<String>>,
    /// Cached paths checked against a repository's published checksum during this run.
    verified: Mutex<HashSet<String>>,
}

impl MavenClient {
    pub fn new(config: &Config, repos: Vec<Repository>) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .user_agent(concat!("kargo/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            repos,
            cache: config.maven_cache(),
            offline: config.offline(),
            refresh: config.refresh(),
            agent,
            shell: config.shell().clone(),
            missing: Mutex::new(HashSet::new()),
            verified: Mutex::new(HashSet::new()),
        }
    }

    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    pub fn repositories(&self) -> &[Repository] {
        &self.repos
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    fn cache_path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.cache.clone(), |path, seg| path.join(seg))
    }

    /// Fetches a repository-relative path, returning the cached file or `None`
    /// if no repository has it.
    pub fn fetch(&self, rel: &str) -> Result<Option<PathBuf>> {
        validate_rel_path(rel)?;
        let cached = self.cache_path(rel);
        if cached.is_file() {
            return Ok(Some(cached));
        }
        if self.missing.lock().expect("lock poisoned").contains(rel) {
            return Ok(None);
        }
        for repo in &self.repos {
            match self.fetch_from(repo, rel, &cached)? {
                Fetch::Found => return Ok(Some(cached)),
                Fetch::Missing => continue,
            }
        }
        if self.offline && !self.repos.iter().all(Repository::is_local) {
            bail!("`{rel}` is not in the local cache and Kargo is running in offline mode");
        }
        self.missing.lock().expect("lock poisoned").insert(rel.to_owned());
        Ok(None)
    }

    /// Like [`MavenClient::fetch`] but fails with a helpful error if missing.
    pub fn fetch_required(&self, rel: &str) -> Result<PathBuf> {
        self.fetch(rel)?.with_context(|| {
            let names: Vec<_> = self.repos.iter().map(|r| r.name.as_str()).collect();
            format!("could not find `{rel}` in any repository ({})", names.join(", "))
        })
    }

    /// Like [`MavenClient::fetch_required`], but a file that was already in the cache is first
    /// checked against the checksum its repository publishes. A corrupt cached file (for example
    /// a truncated download) is replaced with a fresh download. Used before an artifact's hash is
    /// written to `kargo.lock`, so a bad file is never locked.
    pub fn fetch_verified(&self, rel: &str) -> Result<PathBuf> {
        validate_rel_path(rel)?;
        let cached = self.cache_path(rel);
        if cached.is_file() && !self.is_verified(rel) {
            match self.check_cached(rel, &cached)? {
                CacheCheck::Matches => self.mark_verified(rel),
                CacheCheck::Unknown => self.shell.verbose("Unverified", format!("{rel} (no checksum published)")),
                CacheCheck::Mismatch { repo, expected, actual } => {
                    self.shell.warn(format!(
                        "cached {rel} does not match the checksum published by `{repo}` (expected sha1 {expected}, got {actual}); downloading it again"
                    ));
                    std::fs::remove_file(&cached).with_context(|| format!("failed to remove corrupt {}", cached.display()))?;
                }
            }
        }
        self.fetch_required(rel)
    }

    /// Whether `rel` was checked against a checksum published by its repository during this run,
    /// either when it was downloaded or by [`MavenClient::fetch_verified`].
    pub fn is_verified(&self, rel: &str) -> bool {
        self.verified.lock().expect("lock poisoned").contains(rel)
    }

    fn mark_verified(&self, rel: &str) {
        self.verified.lock().expect("lock poisoned").insert(rel.to_owned());
    }

    /// Compares a cached file with the checksum of the first repository that publishes one.
    fn check_cached(&self, rel: &str, cached: &Path) -> Result<CacheCheck> {
        for repo in &self.repos {
            if self.offline && !repo.is_local() {
                continue;
            }
            let Some(expected) = self.published_sha1(repo, rel)? else { continue };
            let actual = hash::sha1_file(cached).with_context(|| format!("failed to hash {}", cached.display()))?;
            return Ok(if actual == expected {
                CacheCheck::Matches
            } else {
                CacheCheck::Mismatch { repo: repo.name.clone(), expected, actual }
            });
        }
        Ok(CacheCheck::Unknown)
    }

    pub fn fetch_text(&self, rel: &str) -> Result<Option<String>> {
        match self.fetch(rel)? {
            Some(path) => Ok(Some(std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?)),
            None => Ok(None),
        }
    }

    fn fetch_from(&self, repo: &Repository, rel: &str, dest: &Path) -> Result<Fetch> {
        if repo.is_local() {
            let src = repo.local_root().join(rel);
            if !src.is_file() {
                return Ok(Fetch::Missing);
            }
            kfs::write_atomic(dest, |f| io::copy(&mut std::fs::File::open(&src)?, f).map(|_| ()))?;
            self.mark_verified(rel);
            return Ok(Fetch::Found);
        }
        if self.offline {
            return Ok(Fetch::Missing);
        }
        let url = format!("{}/{rel}", repo.url);
        let Some(response) = self.get(repo, &url)? else { return Ok(Fetch::Missing) };
        let file_name = rel.rsplit('/').next().unwrap_or(rel);
        if !rel.ends_with(".sha1") && !rel.ends_with("maven-metadata.xml") {
            self.shell.status("Downloading", file_name);
        }
        let mut reader = response.into_body().into_reader();
        kfs::write_atomic(dest, |f| io::copy(&mut reader, f).map(|_| ())).with_context(|| format!("failed to download {url}"))?;

        match self.verify_checksum(repo, rel, dest) {
            Ok(true) => self.mark_verified(rel),
            Ok(false) => {}
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                return Err(e);
            }
        }
        Ok(Fetch::Found)
    }

    /// Verifies the `.sha1` published next to the file, returning whether there was one to check.
    fn verify_checksum(&self, repo: &Repository, rel: &str, file: &Path) -> Result<bool> {
        let Some(expected) = self.published_sha1(repo, rel)? else {
            self.shell.verbose("Unverified", format!("{rel} (no checksum published)"));
            return Ok(false);
        };
        let actual = hash::sha1_file(file)?;
        if actual != expected {
            bail!("checksum mismatch for {}/{rel}: expected sha1 {expected}, got {actual}", repo.url);
        }
        Ok(true)
    }

    /// The SHA-1 a repository publishes for `rel`, or `None` if it has none (or a malformed one).
    /// A local repository without a `.sha1` file vouches for the file it holds, so that file's hash is used.
    fn published_sha1(&self, repo: &Repository, rel: &str) -> Result<Option<String>> {
        if rel.ends_with(".sha1") || rel.ends_with(".md5") || rel.ends_with(".asc") {
            return Ok(None);
        }
        let body = if repo.is_local() {
            let src = repo.local_root().join(rel);
            let sidecar = repo.local_root().join(format!("{rel}.sha1"));
            if sidecar.is_file() {
                std::fs::read_to_string(&sidecar).with_context(|| format!("failed to read {}", sidecar.display()))?
            } else if src.is_file() {
                return Ok(Some(hash::sha1_file(&src).with_context(|| format!("failed to hash {}", src.display()))?));
            } else {
                return Ok(None);
            }
        } else {
            let url = format!("{}/{rel}.sha1", repo.url);
            let Some(response) = self.get(repo, &url)? else { return Ok(None) };
            let mut body = String::new();
            response
                .into_body()
                .into_reader()
                .take(1024)
                .read_to_string(&mut body)
                .with_context(|| format!("failed to read checksum {url}"))?;
            body
        };
        let expected = body.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if expected.len() != 40 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
            self.shell.verbose("Unverified", format!("{rel} (malformed checksum)"));
            return Ok(None);
        }
        Ok(Some(expected))
    }

    fn get(&self, repo: &Repository, url: &str) -> Result<Option<ureq::http::Response<ureq::Body>>> {
        let mut attempt = 0;
        let response = loop {
            let mut request = self.agent.get(url);
            if let Some(creds) = &repo.credentials {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", creds.username, creds.password));
                request = request.header("Authorization", format!("Basic {token}"));
            }
            let result = request.call();
            // Rate limits, gateway hiccups and dropped connections are usually transient.
            let transient = match &result {
                Ok(r) => matches!(r.status().as_u16(), 408 | 429 | 500 | 502 | 503 | 504),
                Err(_) => true,
            };
            if !transient || attempt >= MAX_RETRIES {
                break result.with_context(|| format!("failed to connect to {url}"))?;
            }
            let delay = result
                .as_ref()
                .ok()
                .and_then(|r| r.headers().get("retry-after")?.to_str().ok()?.trim().parse::<u64>().ok())
                .map(|s| Duration::from_secs(s.min(30)))
                .unwrap_or(Duration::from_millis(250 << attempt));
            attempt += 1;
            self.shell.verbose("Retrying", format!("{url} in {:.1}s", delay.as_secs_f64()));
            std::thread::sleep(delay);
        };
        match response.status().as_u16() {
            200..=299 => Ok(Some(response)),
            404 | 410 => Ok(None),
            401 | 403 => bail!("access denied to {url} (HTTP {}); check repository credentials", response.status().as_u16()),
            status => bail!("unexpected HTTP {status} from {url}"),
        }
    }

    /// Lists all versions of a module known to any repository (`maven-metadata.xml`).
    pub fn versions(&self, module: &ModuleId) -> Result<Vec<Version>> {
        let mut versions: Vec<Version> = Vec::new();
        let rel = format!("{}/maven-metadata.xml", module.repo_dir());
        for repo in &self.repos {
            let cached = self.cache.join(module.repo_dir()).join(format!("maven-metadata-{}.xml", sanitize(&repo.name)));
            let fresh = !self.refresh
                && std::fs::metadata(&cached)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .is_some_and(|age| age < METADATA_TTL);
            if !fresh && !(self.offline && !repo.is_local()) {
                // A failure here just means this repository doesn't know the module.
                let _ = std::fs::remove_file(&cached);
                if let Err(e) = self.fetch_from(repo, &rel, &cached) {
                    self.shell.verbose("Skipping", format!("{} metadata from {}: {e:#}", module, repo.name));
                }
            }
            if let Ok(xml) = std::fs::read_to_string(&cached) {
                for v in parse_metadata_versions(&xml) {
                    if !versions.iter().any(|existing| existing.as_str() == v) {
                        versions.push(Version::parse(&v));
                    }
                }
            }
        }
        versions.sort();
        Ok(versions)
    }

    /// The newest version, ignoring pre-releases unless `allow_prerelease` and preferring plain
    /// versions over variants such as `-jre` or `-compat` (see [`select_latest`]).
    pub fn latest_version(&self, module: &ModuleId, allow_prerelease: bool) -> Result<Version> {
        let versions = self.versions(module)?;
        select_latest(&versions, "", allow_prerelease).cloned().with_context(|| format!("no published versions found for `{module}`"))
    }
}

/// Outcome of checking a cached file against its repository.
enum CacheCheck {
    Matches,
    /// No repository publishes a checksum (or none is reachable offline).
    Unknown,
    Mismatch {
        repo: String,
        expected: String,
        actual: String,
    },
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Rejects paths that could escape the cache directory.
fn validate_rel_path(rel: &str) -> Result<()> {
    if rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains(':')
        || rel.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        bail!("refusing to fetch suspicious repository path `{rel}`");
    }
    Ok(())
}

/// Resolves `url` (from Gradle module metadata) relative to the directory of `base`.
pub fn resolve_relative(base_dir: &str, url: &str) -> Result<String> {
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in url.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop().with_context(|| format!("relative url `{url}` escapes the repository"))?;
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    validate_rel_path(&joined)?;
    Ok(joined)
}

fn parse_metadata_versions(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("version") && n.parent().is_some_and(|p| p.has_tag_name("versions")))
        .filter_map(|n| n.text())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::Verbosity;

    #[test]
    fn rejects_traversal() {
        for bad in ["", "/etc/passwd", "a/../../b", "a//b", "C:/x", "a\\b", "./a"] {
            assert!(validate_rel_path(bad).is_err(), "{bad}");
        }
        assert!(validate_rel_path("org/x/y/1.0/y-1.0.jar").is_ok());
    }

    #[test]
    fn resolves_relative_module_urls() {
        assert_eq!(
            resolve_relative("org/x/lib/1.0", "../../lib-jvm/1.0/lib-jvm-1.0.module").unwrap(),
            "org/x/lib-jvm/1.0/lib-jvm-1.0.module"
        );
        assert_eq!(resolve_relative("org/x/lib/1.0", "lib-1.0.klib").unwrap(), "org/x/lib/1.0/lib-1.0.klib");
        assert!(resolve_relative("a", "../../../x").is_err());
    }

    #[test]
    fn parses_metadata() {
        let xml = r#"<metadata><groupId>g</groupId><versioning><latest>2.0</latest><versions>
            <version>1.0</version><version> 2.0 </version></versions></versioning></metadata>"#;
        assert_eq!(parse_metadata_versions(xml), ["1.0", "2.0"]);
        assert!(parse_metadata_versions("garbage").is_empty());
    }

    fn local_client(repo_dir: &Path, cache: &Path) -> MavenClient {
        let config = Config::new(cache.to_path_buf(), Shell::new(Verbosity::Quiet));
        MavenClient::new(&config, vec![Repository::new("local", format!("file://{}", repo_dir.display()))])
    }

    #[test]
    fn fetches_from_local_file_repository() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let rel = "com/acme/lib/1.0/lib-1.0.pom";
        std::fs::create_dir_all(repo.path().join("com/acme/lib/1.0")).unwrap();
        std::fs::write(repo.path().join(rel), "<project/>").unwrap();
        std::fs::write(
            repo.path().join("com/acme/lib/maven-metadata.xml"),
            "<metadata><versioning><versions><version>1.0</version><version>1.1-SNAPSHOT</version></versions></versioning></metadata>",
        )
        .unwrap();

        let client = local_client(repo.path(), home.path());
        let fetched = client.fetch(rel).unwrap().unwrap();
        assert!(fetched.starts_with(home.path()));
        assert_eq!(client.fetch_text(rel).unwrap().unwrap(), "<project/>");
        assert!(client.fetch("com/acme/lib/2.0/lib-2.0.pom").unwrap().is_none());
        assert!(client.fetch_required("com/acme/lib/2.0/lib-2.0.pom").is_err());

        let module = ModuleId::new("com.acme", "lib");
        let versions: Vec<_> = client.versions(&module).unwrap().iter().map(|v| v.to_string()).collect();
        assert_eq!(versions, ["1.0", "1.1-SNAPSHOT"]);
        assert_eq!(client.latest_version(&module, false).unwrap().as_str(), "1.0");
        assert_eq!(client.latest_version(&module, true).unwrap().as_str(), "1.1-SNAPSHOT");
    }

    #[test]
    fn fetch_verified_replaces_corrupt_cached_files() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        std::fs::create_dir_all(repo.path().join("com/acme/lib/1.0")).unwrap();
        std::fs::write(repo.path().join(rel), "good").unwrap();

        let client = local_client(repo.path(), home.path());
        let cached = client.cache_path(rel);
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, "garbage").unwrap();
        assert_eq!(std::fs::read_to_string(client.fetch(rel).unwrap().unwrap()).unwrap(), "garbage", "plain fetch trusts the cache");
        assert!(!client.is_verified(rel));

        let fetched = client.fetch_verified(rel).unwrap();
        assert_eq!(std::fs::read_to_string(&fetched).unwrap(), "good");
        assert!(client.is_verified(rel));

        // A published `.sha1` takes precedence over hashing the repository's copy.
        let fresh = local_client(repo.path(), home.path());
        std::fs::write(repo.path().join(format!("{rel}.sha1")), format!("{}  lib-1.0.jar\n", hash::sha1_file(&fetched).unwrap())).unwrap();
        std::fs::write(repo.path().join(rel), "changed upstream").unwrap();
        assert_eq!(std::fs::read_to_string(fresh.fetch_verified(rel).unwrap()).unwrap(), "good");
        assert!(fresh.is_verified(rel));
    }

    #[test]
    fn repository_urls_are_normalized() {
        assert_eq!(Repository::new("x", "https://a/b/").url, "https://a/b");
        assert!(Repository::new("x", "file:///tmp/r").is_local());
        assert!(!Repository::maven_central().is_local());
    }
}
