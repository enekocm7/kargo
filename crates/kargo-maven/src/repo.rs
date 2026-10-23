//! Remote Maven repositories and the local download cache.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use super::coord::{Artifact, ModuleId, is_snapshot};
use super::snapshot::SnapshotMetadata;
use super::version::{Version, select_latest};
use kargo_util::config::Config;
use kargo_util::retry;
use kargo_util::shell::Shell;
use kargo_util::{fs as kfs, hash};

pub const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2";
pub const GOOGLE: &str = "https://dl.google.com/dl/android/maven2";

/// The name of the local Maven repository (`~/.m2/repository`) in `[repositories]` and `kargo publish`.
pub const MAVEN_LOCAL: &str = "maven-local";

/// Environment variable overriding the local Maven repository directory.
pub const MAVEN_LOCAL_ENV: &str = "KARGO_MAVEN_LOCAL";

/// The local Maven repository that `mvn install` and Gradle's `publishToMavenLocal` write:
/// `$KARGO_MAVEN_LOCAL`, else `~/.m2/repository`.
pub fn maven_local_dir() -> Result<PathBuf> {
    match std::env::var_os(MAVEN_LOCAL_ENV) {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => Ok(dirs::home_dir().context("could not determine the home directory; set KARGO_MAVEN_LOCAL")?.join(".m2").join("repository")),
    }
}

/// How long `maven-metadata.xml` (the version list, or the builds of a SNAPSHOT) is trusted before re-checking.
const METADATA_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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

    /// The local Maven repository, `~/.m2/repository` (see [`maven_local_dir`]).
    pub fn maven_local() -> Result<Self> {
        Ok(Self::new(MAVEN_LOCAL, maven_local_dir()?.to_string_lossy().into_owned()))
    }

    /// Whether the repository is a directory on this machine (a `file:` URL or a plain path).
    pub fn is_local(&self) -> bool {
        self.url.starts_with("file:") || !self.url.contains("://")
    }

    /// The directory name of this repository's files in the Maven cache, when they are kept apart from other
    /// repositories': its name, and a hash of its URL so that two repositories with the same name stay apart.
    pub fn cache_dir_name(&self) -> String {
        format!("{SCOPE_PREFIX}{}-{}", sanitize(&self.name), &hash::sha256_bytes(self.url.as_bytes())[..8])
    }

    /// The directory of a local repository.
    pub fn local_root(&self) -> PathBuf {
        let path = self.url.strip_prefix("file://").or_else(|| self.url.strip_prefix("file:")).unwrap_or(&self.url);
        // `file:///C:/x` -> `/C:/x`; strip the leading slash before a drive letter.
        let path = if cfg!(windows) && path.len() > 2 && path.starts_with('/') && path.as_bytes()[2] == b':' { &path[1..] } else { path };
        PathBuf::from(path)
    }
}

/// The `Authorization` header value for HTTP basic authentication.
fn basic_auth(creds: &Credentials) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", creds.username, creds.password)))
}

/// The repository `kargo vendor` wrote at `dir`, the only one used by a `--vendored` build.
pub fn vendor_repository(dir: &Path) -> Repository {
    Repository::new("vendor", dir.to_string_lossy().into_owned())
}

/// The repositories Kargo actually contacts for `declared`: only the vendored one in a `--vendored`
/// build, else each remote repository replaced by its mirror from `[mirrors]` (with the mirror's
/// credentials), and repositories that end up at the same URL merged. Repositories on disk are kept.
pub fn effective_repositories(config: &Config, declared: &[Repository]) -> Vec<Repository> {
    if let Some(dir) = config.vendor_dir() {
        return vec![vendor_repository(dir)];
    }
    let mut out: Vec<Repository> = Vec::new();
    for repo in declared {
        let repo = mirrored(config, repo);
        if !out.iter().any(|r| r.url == repo.url) {
            out.push(repo);
        }
    }
    out
}

/// `repo` as Kargo contacts it: a remote repository replaced by its mirror from `[mirrors]`, with the credentials configured for the URL.
fn mirrored(config: &Config, repo: &Repository) -> Repository {
    let settings = config.settings();
    let mut repo = repo.clone();
    if !repo.is_local() {
        if let Some(mirror) = settings.repository_mirror(&repo.name, &repo.url) {
            repo.url = mirror.trim_end_matches('/').to_owned();
            repo.credentials = None;
        }
        if repo.credentials.is_none()
            && let Some((username, password)) = settings.credentials(&repo.url)
        {
            repo.credentials = Some(Credentials { username, password });
        }
    }
    repo
}

/// Cached files of repositories that are not shared (see [`MavenClient`]) live in a directory of this form, named after the
/// repository, directly under the cache: `@<name>-<hash of the URL>/<group>/<artifact>/...`.
const SCOPE_PREFIX: char = '@';

/// Splits a cache-relative path into the directory of its repository (if it has one) and the repository-relative path.
fn split_scope(rel: &str) -> (Option<&str>, &str) {
    if rel.starts_with(SCOPE_PREFIX)
        && let Some((scope, plain)) = rel.split_once('/')
    {
        return (Some(scope), plain);
    }
    (None, rel)
}

/// The repository-relative path of a cache-relative path (as `kargo.lock` and `kargo vendor` list them).
pub fn plain_rel(rel: &str) -> &str {
    split_scope(rel).1
}

/// Whether `rel` is in the directory of a SNAPSHOT version, whose files change under one name.
fn is_snapshot_path(rel: &str) -> bool {
    rel.split('/').any(|seg| seg.ends_with("-SNAPSHOT"))
}

/// Outcome of looking for a single remote file.
enum Fetch {
    Found,
    Missing,
}

/// The file that records the size and modification time a cached file had when it last matched its repository's checksum.
fn verified_stamp(cached: &Path) -> PathBuf {
    let mut name = cached.as_os_str().to_owned();
    name.push(".verified");
    PathBuf::from(name)
}

/// The size and modification time of a file, which tell whether it changed since it was checked.
fn file_fingerprint(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(format!("{} {}", meta.len(), modified.as_nanos()))
}

/// Downloads artifacts from an ordered list of repositories into a shared cache.
pub struct MavenClient {
    repos: Vec<Repository>,
    /// The repositories as the project declares them, before mirrors and vendoring.
    declared: Vec<Repository>,
    cache: PathBuf,
    offline: bool,
    refresh: bool,
    /// Fail on a downloaded file without a published checksum (see [`Config::require_checksums`]).
    require_checksums: bool,
    agent: ureq::Agent,
    shell: Shell,
    /// Paths known to be missing from every repository during this run.
    missing: Mutex<HashSet<String>>,
    /// Cached paths checked against a repository's published checksum during this run.
    verified: Mutex<HashSet<String>>,
    /// SNAPSHOT metadata already read during this run, keyed by repository and version directory.
    snapshots: Mutex<HashMap<String, Option<Arc<SnapshotMetadata>>>>,
    /// Cached SNAPSHOT files under their plain name that were compared with their repository during this run.
    rechecked: Mutex<HashSet<String>>,
    /// SNAPSHOTs fixed at a build (`1.0-20240101.120000-1`) instead of the newest one.
    snapshot_builds: Mutex<HashMap<(ModuleId, String), String>>,
    /// URLs of the repositories whose released files are cached under their plain path: Maven Central and Google
    /// (or their mirrors), whose releases never change, and the vendor directory. The files of every other
    /// repository, and every SNAPSHOT, are cached in a directory of their repository.
    shared: HashSet<String>,
    /// Every file this client served during this run, by repository-relative path (see [`MavenClient::served_files`]).
    served: Mutex<BTreeMap<String, PathBuf>>,
    /// Held while a corrupt cached file is replaced (see [`MavenClient::fetch_verified`]).
    repairing: Mutex<()>,
    /// Hosts that could not be reached during this run; requests to them fail at once.
    unreachable: Mutex<HashSet<String>>,
}

impl MavenClient {
    /// A client for the `repos` a project declares; it contacts their mirrors, or only the vendored
    /// repository, as the configuration says (see [`effective_repositories`]).
    pub fn new(config: &Config, repos: Vec<Repository>) -> Self {
        let agent = kargo_util::http::agent(config, Duration::from_secs(600));
        let effective = effective_repositories(config, &repos);
        let shared = if config.vendor_dir().is_some() {
            effective.iter().map(|r| r.url.clone()).collect()
        } else {
            repos.iter().filter(|r| r.url == MAVEN_CENTRAL || r.url == GOOGLE).map(|r| mirrored(config, r).url).collect()
        };
        Self {
            repos: effective,
            shared,
            declared: repos,
            cache: config.maven_cache(),
            offline: config.offline(),
            refresh: config.refresh(),
            require_checksums: config.require_checksums(),
            agent,
            shell: config.shell().clone(),
            missing: Mutex::new(HashSet::new()),
            verified: Mutex::new(HashSet::new()),
            snapshots: Mutex::new(HashMap::new()),
            rechecked: Mutex::new(HashSet::new()),
            snapshot_builds: Mutex::new(HashMap::new()),
            served: Mutex::new(BTreeMap::new()),
            repairing: Mutex::new(()),
            unreachable: Mutex::new(HashSet::new()),
        }
    }

    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// The repositories this client contacts, after mirrors and vendoring.
    pub fn repositories(&self) -> &[Repository] {
        &self.repos
    }

    /// The repositories as the project declares them. `kargo.lock` fingerprints these, so moving
    /// to a mirror or to `vendor/` keeps the lock.
    pub fn declared_repositories(&self) -> &[Repository] {
        &self.declared
    }

    /// Every file this client served during this run (from the cache or a repository), by
    /// repository-relative path, with its cached copy. `maven-metadata.xml` files are listed under
    /// that name. `kargo vendor` copies these.
    pub fn served_files(&self) -> BTreeMap<String, PathBuf> {
        self.served.lock().expect("lock poisoned").clone()
    }

    fn serve(&self, rel: &str, cached: &Path) {
        self.served.lock().expect("lock poisoned").entry(rel.to_owned()).or_insert_with(|| cached.to_path_buf());
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    fn cache_path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.cache.clone(), |path, seg| path.join(seg))
    }

    /// Where `repo`'s copy of the repository-relative path `rel` is cached, relative to the cache.
    fn key_for(&self, repo: &Repository, rel: &str) -> String {
        if self.shared.contains(&repo.url) && !is_snapshot_path(rel) { rel.to_owned() } else { format!("{}/{rel}", repo.cache_dir_name()) }
    }

    /// The repositories that may hold the file at cache path `rel` (a plain repository path, or one in a
    /// repository's directory, as `kargo.lock` lists it), each with the cache-relative path of its copy.
    fn candidates<'a>(&'a self, rel: &'a str) -> Vec<(&'a Repository, String)> {
        match split_scope(rel) {
            (None, plain) => self.repos.iter().map(|r| (r, self.key_for(r, plain))).collect(),
            // A vendored build has one repository, which holds every file under its plain path.
            (Some(_), _) if self.repos.iter().all(|r| self.shared.contains(&r.url)) => {
                self.repos.iter().map(|r| (r, rel.to_owned())).collect()
            }
            (Some(scope), _) => self.repos.iter().filter(|r| r.cache_dir_name() == scope).map(|r| (r, rel.to_owned())).collect(),
        }
    }

    /// The cached copy of the repository-relative path `rel` from any of the repositories, if there is one.
    fn cached_file(&self, rel: &str) -> Option<PathBuf> {
        self.cached_copies(rel).into_iter().next()
    }

    /// Every cached copy of the repository-relative path `rel`, one per repository that has it cached, in repository order.
    pub fn cached_copies(&self, rel: &str) -> Vec<PathBuf> {
        self.candidates(rel).into_iter().map(|(_, key)| self.cache_path(&key)).filter(|p| p.is_file()).collect()
    }

    /// Fetches a path, returning the cached file or `None` if no repository has it. `rel` is a
    /// repository-relative path (`org/acme/lib/1.0/lib-1.0.jar`); the file comes from the first repository
    /// that has it, and is cached apart from other repositories' copies (see [`MavenClient::new`]). The path
    /// of a cached file, as `kargo.lock` lists it, fetches that file again.
    pub fn fetch(&self, rel: &str) -> Result<Option<PathBuf>> {
        self.fetch_with(rel, false)
    }

    /// [`MavenClient::fetch`]; `pinned` says the caller checks the file against a hash it already knows.
    fn fetch_with(&self, rel: &str, pinned: bool) -> Result<Option<PathBuf>> {
        validate_rel_path(rel)?;
        let plain = plain_rel(rel);
        let candidates = self.candidates(rel);
        for (repo, key) in &candidates {
            let cached = self.cache_path(key);
            if cached.is_file() {
                self.recheck_plain_snapshot(repo, plain, key, &cached);
            }
            if cached.is_file() {
                self.serve(plain, &cached);
                return Ok(Some(cached));
            }
        }
        if self.missing.lock().expect("lock poisoned").contains(rel) {
            return Ok(None);
        }
        for (repo, key) in &candidates {
            let cached = self.cache_path(key);
            match self.fetch_from(repo, plain, key, &cached, pinned)? {
                Fetch::Found => {
                    self.serve(plain, &cached);
                    return Ok(Some(cached));
                }
                Fetch::Missing => continue,
            }
        }
        if self.offline && !self.repos.iter().all(Repository::is_local) {
            bail!("`{rel}` is not in the local cache and Kargo is running in offline mode; run `kargo fetch` while online first");
        }
        self.missing.lock().expect("lock poisoned").insert(rel.to_owned());
        Ok(None)
    }

    /// A SNAPSHOT that a repository stores under its plain name (`lib-1.0-SNAPSHOT.jar`, as `mvn install`
    /// leaves it) is overwritten whenever the library is built again, so a cached copy goes stale. Once
    /// per run, the copy is compared with the repository's checksum, and deleted (to be downloaded again)
    /// if it changed. Remote repositories are only asked when the copy is older than [`METADATA_TTL`] or
    /// after `--refresh`; local ones every time.
    fn recheck_plain_snapshot(&self, repo: &Repository, rel: &str, key: &str, cached: &Path) {
        let mut segments = rel.rsplit('/');
        let (Some(name), Some(dir)) = (segments.next(), segments.next()) else { return };
        let is_plain_snapshot = dir.ends_with("-SNAPSHOT") && name.contains("-SNAPSHOT");
        // A file downloaded or checked during this run is current.
        if !is_plain_snapshot || self.is_verified(key) || !self.rechecked.lock().expect("lock poisoned").insert(key.to_owned()) {
            return;
        }
        let modified = std::fs::metadata(cached).and_then(|m| m.modified()).ok();
        let due = self.refresh || modified.and_then(|t| SystemTime::now().duration_since(t).ok()).is_none_or(|age| age >= METADATA_TTL);
        if !repo.is_local() && (self.offline || !due) {
            return;
        }
        let Ok(Some(expected)) = self.published_checksum(repo, rel) else { return };
        if expected.algorithm.hash_file(cached).is_ok_and(|actual| actual == expected.hex) {
            // Up to date: restart the clock for remote repositories.
            let _ = std::fs::File::options().write(true).open(cached).and_then(|f| f.set_modified(SystemTime::now()));
            self.mark_verified(key);
        } else {
            self.shell.verbose("Updating", format!("{rel} (changed in {})", repo.name));
            let _ = std::fs::remove_file(cached);
        }
    }

    /// Like [`MavenClient::fetch`] but fails with a helpful error if missing.
    pub fn fetch_required(&self, rel: &str) -> Result<PathBuf> {
        self.fetch_required_with(rel, false)
    }

    /// Like [`MavenClient::fetch_required`], for a file whose SHA-256 the caller already knows and checks the
    /// download against, such as one `kargo.lock` pins. That hash vouches for the file, so a repository that
    /// publishes no checksum for it is no error under `--require-checksums`.
    pub fn fetch_pinned(&self, rel: &str) -> Result<PathBuf> {
        self.fetch_required_with(rel, true)
    }

    fn fetch_required_with(&self, rel: &str, pinned: bool) -> Result<PathBuf> {
        self.fetch_with(rel, pinned)?.with_context(|| {
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
        for (repo, key) in self.candidates(rel) {
            let cached = self.cache_path(&key);
            if !cached.is_file() || self.is_verified(&key) {
                continue;
            }
            // A file that is unchanged since an earlier run checked it need not be checked again.
            let fingerprint = file_fingerprint(&cached);
            if fingerprint.as_deref().is_some_and(|f| std::fs::read_to_string(verified_stamp(&cached)).is_ok_and(|s| s == f)) {
                self.mark_verified(&key);
                continue;
            }
            let check = match self.check_cached(repo, plain_rel(rel), &cached) {
                // Another thread found it corrupt and removed it; the fetch below gets a fresh copy.
                Err(_) if !cached.is_file() => continue,
                check => check?,
            };
            match check {
                CacheCheck::Matches => {
                    self.mark_verified(&key);
                    if let Some(fingerprint) = fingerprint {
                        // A stamp only saves time, so one that cannot be written is not an error.
                        let _ = kfs::write_string_atomic(&verified_stamp(&cached), &fingerprint);
                    }
                }
                CacheCheck::Unknown => self.unverified(&repo.url, rel)?,
                CacheCheck::Offline if self.require_checksums => bail!(
                    "{rel} was never checked against the checksum its repository publishes, which --require-checksums needs, \
                     and Kargo is offline; run the command once without --offline"
                ),
                CacheCheck::Offline => self.shell.verbose("Unverified", format!("{rel} (offline)")),
                CacheCheck::Mismatch { expected, actual } => {
                    // Resolutions are locked in parallel and can share a file: one thread replaces it, the others use that copy.
                    let _repairing = self.repairing.lock().expect("lock poisoned");
                    if !cached.is_file()
                        || self.is_verified(&key)
                        || !matches!(self.check_cached(repo, plain_rel(rel), &cached)?, CacheCheck::Mismatch { .. })
                    {
                        continue;
                    }
                    self.shell.warn(format!(
                        "cached {rel} does not match the checksum published by `{}` (expected {} {}, got {actual}); downloading it again",
                        repo.name, expected.algorithm, expected.hex
                    ));
                    std::fs::remove_file(&cached).with_context(|| format!("failed to remove corrupt {}", cached.display()))?;
                }
            }
        }
        self.fetch_required(rel)
    }

    /// Whether the cached file at `rel` (as [`MavenClient::fetch`] returned it) was checked against a checksum
    /// published by its repository during this run, either when it was downloaded or by [`MavenClient::fetch_verified`].
    pub fn is_verified(&self, rel: &str) -> bool {
        let verified = self.verified.lock().expect("lock poisoned");
        verified.contains(rel) || self.candidates(rel).iter().any(|(_, key)| verified.contains(key))
    }

    fn mark_verified(&self, rel: &str) {
        self.verified.lock().expect("lock poisoned").insert(rel.to_owned());
    }

    /// Compares a cached file with the checksum its repository publishes.
    fn check_cached(&self, repo: &Repository, rel: &str, cached: &Path) -> Result<CacheCheck> {
        if self.offline && !repo.is_local() {
            return Ok(CacheCheck::Offline);
        }
        let Some(expected) = self.published_checksum(repo, rel)? else { return Ok(CacheCheck::Unknown) };
        let actual = expected.algorithm.hash_file(cached).with_context(|| format!("failed to hash {}", cached.display()))?;
        Ok(if actual == expected.hex { CacheCheck::Matches } else { CacheCheck::Mismatch { expected, actual } })
    }

    pub fn fetch_text(&self, rel: &str) -> Result<Option<String>> {
        match self.fetch(rel)? {
            Some(path) => Ok(Some(std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?)),
            None => Ok(None),
        }
    }

    /// Copies or downloads `rel` from `repo` into `dest`, which is cached under `key`.
    fn fetch_from(&self, repo: &Repository, rel: &str, key: &str, dest: &Path, pinned: bool) -> Result<Fetch> {
        if repo.is_local() {
            let src = repo.local_root().join(rel);
            if !src.is_file() {
                return Ok(Fetch::Missing);
            }
            kfs::write_atomic_if_absent(dest, |f| io::copy(&mut std::fs::File::open(&src)?, f).map(|_| ()))?;
            self.mark_verified(key);
            return Ok(Fetch::Found);
        }
        if self.offline {
            return Ok(Fetch::Missing);
        }
        let url = format!("{}/{rel}", repo.url);
        let Some(response) = self.get(repo, &url)? else { return Ok(Fetch::Missing) };
        let file_name = rel.rsplit('/').next().unwrap_or(rel);
        if !is_checksum_file(rel) && !rel.ends_with("maven-metadata.xml") {
            self.shell.status("Downloading", if self.shell.is_verbose() { &url } else { file_name });
        }
        let mut reader = response.into_body().into_reader();
        kfs::write_atomic_if_absent(dest, |f| io::copy(&mut reader, f).map(|_| ())).with_context(|| format!("failed to download {url}"))?;

        match self.verify_checksum(repo, rel, dest, pinned) {
            Ok(true) => self.mark_verified(key),
            Ok(false) => {}
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                return Err(e);
            }
        }
        Ok(Fetch::Found)
    }

    /// Verifies the checksum published next to the file (see [`MavenClient::published_checksum`]), returning
    /// whether there was one to check. A file without one is accepted unverified, or with `--require-checksums`
    /// is an error, unless it is `pinned` (its caller checks it against a hash it already knows).
    fn verify_checksum(&self, repo: &Repository, rel: &str, file: &Path, pinned: bool) -> Result<bool> {
        let Some(expected) = self.published_checksum(repo, rel)? else {
            if pinned {
                self.shell.verbose("Unverified", format!("{rel} (no checksum published; checked against kargo.lock)"));
            } else {
                self.unverified(&repo.url, rel)?;
            }
            return Ok(false);
        };
        let actual = expected.algorithm.hash_file(file)?;
        if actual != expected.hex {
            bail!("checksum mismatch for {}/{rel}: expected {} {}, got {actual}", repo.url, expected.algorithm, expected.hex);
        }
        Ok(true)
    }

    /// Accepts a file of `repo_url` that has no published checksum, or fails under `--require-checksums`.
    fn unverified(&self, repo_url: &str, rel: &str) -> Result<()> {
        if self.require_checksums {
            bail!(
                "{repo_url}/{rel} has no published checksum (`.sha256` or `.sha1`) to verify it against, and --require-checksums is set; \
                 ask the repository's maintainers to publish one, or build without --require-checksums"
            );
        }
        self.shell.verbose("Unverified", format!("{rel} (no checksum published)"));
        Ok(())
    }

    /// The strongest checksum a repository publishes for `rel` (see [`Algorithm`]), or `None` if it has none (or
    /// only malformed ones). A local repository without a checksum file vouches for the file it holds, so that
    /// file's hash is used.
    fn published_checksum(&self, repo: &Repository, rel: &str) -> Result<Option<Checksum>> {
        if is_checksum_file(rel) {
            return Ok(None);
        }
        for algorithm in Algorithm::ALL {
            let body = if repo.is_local() {
                let sidecar = repo.local_root().join(format!("{rel}.{algorithm}"));
                if !sidecar.is_file() {
                    continue;
                }
                std::fs::read_to_string(&sidecar).with_context(|| format!("failed to read {}", sidecar.display()))?
            } else {
                let url = format!("{}/{rel}.{algorithm}", repo.url);
                let Some(response) = self.get(repo, &url)? else { continue };
                let mut body = String::new();
                response
                    .into_body()
                    .into_reader()
                    .take(1024)
                    .read_to_string(&mut body)
                    .with_context(|| format!("failed to read checksum {url}"))?;
                body
            };
            match Checksum::parse(algorithm, &body) {
                Some(checksum) => return Ok(Some(checksum)),
                None => self.shell.verbose("Unverified", format!("{rel} (malformed {algorithm} checksum)")),
            }
        }
        let src = repo.local_root().join(rel);
        if repo.is_local() && src.is_file() {
            let hex = Algorithm::Sha256.hash_file(&src).with_context(|| format!("failed to hash {}", src.display()))?;
            return Ok(Some(Checksum { algorithm: Algorithm::Sha256, hex }));
        }
        Ok(None)
    }

    fn get(&self, repo: &Repository, url: &str) -> Result<Option<ureq::http::Response<ureq::Body>>> {
        let host = host_of(url);
        if self.unreachable.lock().expect("lock poisoned").contains(host) {
            bail!("{}", unreachable_message(host));
        }
        let mut attempt = 0;
        let response = loop {
            let mut request = self.agent.get(url);
            if let Some(creds) = &repo.credentials {
                request = request.header("Authorization", basic_auth(creds));
            }
            let result = request.call();
            // Rate limits, gateway hiccups and dropped connections are usually transient; DNS, TLS and URL errors are not.
            let transient = match &result {
                Ok(r) => retry::is_transient_status(r.status().as_u16()),
                Err(e) => retry::is_transient_error(e),
            };
            if !transient || attempt >= retry::MAX_RETRIES {
                if let Err(e) = &result
                    && !matches!(e, ureq::Error::BadUri(_) | ureq::Error::Http(_))
                {
                    self.unreachable.lock().expect("lock poisoned").insert(host.to_owned());
                    return Err(anyhow::Error::new(result.unwrap_err()))
                        .with_context(|| unreachable_message(host))
                        .with_context(|| format!("failed to connect to {url}"));
                }
                break result.with_context(|| format!("failed to connect to {url}"))?;
            }
            let delay = result
                .as_ref()
                .ok()
                .and_then(|r| r.headers().get("retry-after")?.to_str().ok()?.trim().parse::<u64>().ok())
                .map(|s| Duration::from_secs(s.min(30)))
                .unwrap_or(retry::backoff(attempt));
            attempt += 1;
            self.shell.verbose("Retrying", format!("{url} in {:.1}s", delay.as_secs_f64()));
            std::thread::sleep(delay);
        };
        match response.status().as_u16() {
            200..=299 => Ok(Some(response)),
            404 | 410 => Ok(None),
            // Object storage answers 403 for a key that does not exist, so probes for optional files treat it as absent.
            403 if is_optional_probe(url) => Ok(None),
            401 | 403 => bail!("access denied to {url} (HTTP {}); check repository credentials", response.status().as_u16()),
            status => bail!("unexpected HTTP {status} from {url}"),
        }
    }

    /// Reads `rel` straight from `repo`, bypassing the cache (for metadata that is about to change).
    pub fn read_text_from(&self, repo: &Repository, rel: &str) -> Result<Option<String>> {
        validate_rel_path(rel)?;
        if repo.is_local() {
            let path = repo.local_root().join(rel);
            return match std::fs::read_to_string(&path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
            };
        }
        let url = format!("{}/{rel}", repo.url);
        let Some(response) = self.get(repo, &url)? else { return Ok(None) };
        let mut body = String::new();
        response.into_body().into_reader().read_to_string(&mut body).with_context(|| format!("failed to read {url}"))?;
        Ok(Some(body))
    }

    /// Stores `data` at `rel` in `repo`: written into a local repository, or sent with an HTTP `PUT`
    /// (with the repository's credentials) like `mvn deploy` does.
    pub fn upload(&self, repo: &Repository, rel: &str, data: &[u8]) -> Result<()> {
        validate_rel_path(rel)?;
        if repo.is_local() {
            let path = rel.split('/').fold(repo.local_root(), |path, seg| path.join(seg));
            return kfs::write_atomic(&path, |f| io::Write::write_all(f, data));
        }
        if self.offline {
            bail!("cannot upload to `{}` in offline mode", repo.name);
        }
        let url = format!("{}/{rel}", repo.url);
        let mut attempt = 0;
        loop {
            let mut request = self.agent.put(&url);
            if let Some(creds) = &repo.credentials {
                request = request.header("Authorization", basic_auth(creds));
            }
            let result = request.send(data);
            let transient = match &result {
                Ok(r) => retry::is_transient_status(r.status().as_u16()),
                Err(_) => true,
            };
            if transient && attempt < retry::MAX_RETRIES {
                let delay = retry::backoff(attempt);
                attempt += 1;
                self.shell.verbose("Retrying", format!("{url} in {:.1}s", delay.as_secs_f64()));
                std::thread::sleep(delay);
                continue;
            }
            let response = result.with_context(|| format!("failed to upload to {url}"))?;
            return match response.status().as_u16() {
                200..=299 => Ok(()),
                401 | 403 if repo.credentials.is_none() => bail!(
                    "`{}` refused the upload of {url} (HTTP {}); set `username-env` and `password-env` for [repositories.{}] in kargo.toml and export those variables",
                    repo.name,
                    response.status().as_u16(),
                    repo.name
                ),
                401 | 403 => bail!(
                    "`{}` refused the upload of {url} (HTTP {}); check the repository credentials",
                    repo.name,
                    response.status().as_u16()
                ),
                status => {
                    let mut body = String::new();
                    let _ = response.into_body().into_reader().take(2048).read_to_string(&mut body);
                    let body = body.trim();
                    if body.is_empty() {
                        bail!("`{}` rejected the upload of {url} (HTTP {status})", repo.name)
                    }
                    bail!("`{}` rejected the upload of {url} (HTTP {status}): {body}", repo.name)
                }
            };
        }
    }

    /// The HTTP client, for services other than Maven repositories (the Maven Central Portal).
    pub fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    /// Whether the client may use the network.
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Whether cached metadata is ignored (`--refresh`).
    pub fn refresh(&self) -> bool {
        self.refresh
    }

    /// Lists all versions of a module known to any repository (`maven-metadata.xml`).
    pub fn versions(&self, module: &ModuleId) -> Result<Vec<Version>> {
        let mut versions: Vec<Version> = Vec::new();
        let rel = format!("{}/maven-metadata.xml", module.repo_dir());
        for repo in &self.repos {
            let cached = self.cache.join(module.repo_dir()).join(format!("maven-metadata-{}.xml", sanitize(&repo.name)));
            let Some(xml) = self.repo_metadata(repo, &rel, &cached, module) else { continue };
            for v in parse_metadata_versions(&xml) {
                if !versions.iter().any(|existing| existing.as_str() == v) {
                    versions.push(Version::parse(&v));
                }
            }
        }
        versions.sort();
        Ok(versions)
    }

    /// The text of `repo`'s `maven-metadata.xml` at `rel`, kept in `cached` and re-checked when older
    /// than [`METADATA_TTL`] or after `--refresh`. `subject` names what the metadata is about.
    fn repo_metadata(&self, repo: &Repository, rel: &str, cached: &Path, subject: &dyn std::fmt::Display) -> Option<String> {
        // A local repository is a plain directory, so its metadata is always read afresh: whatever was
        // just published into it must be seen, not hidden behind a day-old copy.
        let fresh = !repo.is_local()
            && !self.refresh
            && std::fs::metadata(cached)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < METADATA_TTL);
        if !fresh && !(self.offline && !repo.is_local()) {
            // The download replaces the cached copy atomically, so a concurrent resolution of the same
            // module never sees it missing. A failure just means this repository doesn't know the module.
            match self.fetch_from(repo, rel, rel, cached, false) {
                Ok(Fetch::Found) => {}
                // Only a definitive "not found" invalidates the cached copy.
                Ok(_) => {
                    let _ = std::fs::remove_file(cached);
                }
                Err(e) => {
                    self.shell.verbose("Skipping", format!("{subject} metadata from {}: {e:#}", repo.name));
                    if let Some(age) =
                        std::fs::metadata(cached).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok())
                    {
                        self.shell.warn(format!("{subject}: using cached metadata from {} ago", describe_age(age)));
                    }
                }
            }
        }
        let text = std::fs::read_to_string(cached).ok()?;
        self.serve(rel, cached);
        Some(text)
    }

    /// The repository-relative path of `artifact`. A SNAPSHOT is deployed as timestamped builds
    /// (`lib-1.0-20240101.120000-1.jar` in the `1.0-SNAPSHOT` directory) that its `maven-metadata.xml`
    /// lists, so this returns the file of the newest build. A repository without such metadata, or
    /// one that stores the file under the plain `-SNAPSHOT` name, keeps `artifact.repo_path()`.
    pub fn artifact_path(&self, artifact: &Artifact) -> Result<String> {
        if is_snapshot(&artifact.version) {
            let pinned =
                self.snapshot_builds.lock().expect("lock poisoned").get(&(artifact.module.clone(), artifact.version.clone())).cloned();
            if let Some(build) = pinned {
                return Ok(artifact.repo_path_for_build(&build));
            }
            for repo in &self.repos {
                let Some(meta) = self.snapshot_metadata(repo, &artifact.module, &artifact.version)? else { continue };
                if let Some(build) = meta.file_version(&artifact.version, artifact.classifier.as_deref(), &artifact.extension) {
                    return Ok(artifact.repo_path_for_build(&build));
                }
            }
        }
        Ok(artifact.repo_path())
    }

    /// Downloads the sources jar (`-sources.jar`) of `module:version`, or returns `None` when no
    /// repository publishes one. Sources jars are for editors; they are not locked. Offline,
    /// one that is neither cached nor in a repository on disk counts as missing.
    pub fn fetch_sources(&self, module: &ModuleId, version: &str) -> Result<Option<PathBuf>> {
        let artifact = Artifact::new(module.clone(), version).with_classifier(Some("sources".to_owned()));
        match self.fetch(&self.artifact_path(&artifact)?) {
            Err(_) if self.offline => Ok(None),
            result => result,
        }
    }

    /// The sources jar of `module:version` if it is already in the cache, without any network
    /// access. `files` are the module's downloaded artifacts, whose names give a SNAPSHOT's build.
    pub fn cached_sources(&self, module: &ModuleId, version: &str, files: &[PathBuf]) -> Option<PathBuf> {
        let artifact = Artifact::new(module.clone(), version).with_classifier(Some("sources".to_owned()));
        let builds = files.iter().filter_map(|f| {
            let stamp = super::snapshot::build_stamp(f.file_name()?.to_str()?, &module.artifact, version)?;
            Some(format!("{}-{stamp}", version.strip_suffix("-SNAPSHOT")?))
        });
        let mut candidates: Vec<String> = builds.map(|b| artifact.repo_path_for_build(&b)).collect();
        candidates.push(artifact.repo_path());
        candidates.into_iter().filter(|rel| validate_rel_path(rel).is_ok()).find_map(|rel| self.cached_file(&rel))
    }

    /// Fixes SNAPSHOT `module:version` at the build whose files are named `build` (`1.0-20240101.120000-1`),
    /// so a locked build is kept while the repository moves on.
    pub fn pin_snapshot_build(&self, module: &ModuleId, version: &str, build: &str) {
        self.snapshot_builds.lock().expect("lock poisoned").insert((module.clone(), version.to_owned()), build.to_owned());
    }

    /// What `repo` knows about the builds of SNAPSHOT `module:version`, read once per run.
    fn snapshot_metadata(&self, repo: &Repository, module: &ModuleId, version: &str) -> Result<Option<Arc<SnapshotMetadata>>> {
        let dir = format!("{}/{version}", module.repo_dir());
        let rel = format!("{dir}/maven-metadata.xml");
        validate_rel_path(&rel)?;
        let key = format!("{}|{rel}", repo.url);
        if let Some(known) = self.snapshots.lock().expect("lock poisoned").get(&key) {
            return Ok(known.clone());
        }
        let cached = self.cache_path(&format!("{dir}/maven-metadata-{}.xml", sanitize(&repo.name)));
        let meta = self
            .repo_metadata(repo, &rel, &cached, &format_args!("{module}:{version}"))
            .and_then(|xml| SnapshotMetadata::parse(&xml))
            .map(Arc::new);
        self.snapshots.lock().expect("lock poisoned").insert(key, meta.clone());
        Ok(meta)
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
    /// The repository publishes no checksum.
    Unknown,
    /// The repository is remote and Kargo is offline.
    Offline,
    Mismatch {
        expected: Checksum,
        actual: String,
    },
}

/// The checksum algorithms Kargo checks a download with, strongest first: the SHA-256 that Maven 3.9 and Gradle
/// publish next to every file (with a SHA-512 that would only cost another request), else the SHA-1 that every
/// Maven repository has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Sha256,
    Sha1,
}

impl Algorithm {
    const ALL: [Algorithm; 2] = [Algorithm::Sha256, Algorithm::Sha1];

    fn hex_len(self) -> usize {
        match self {
            Algorithm::Sha256 => 64,
            Algorithm::Sha1 => 40,
        }
    }

    fn hash_file(self, path: &Path) -> io::Result<String> {
        match self {
            Algorithm::Sha256 => hash::sha256_file(path),
            Algorithm::Sha1 => hash::sha1_file(path),
        }
    }
}

impl std::fmt::Display for Algorithm {
    /// The name, which is also the extension of the file it is published in.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha1 => "sha1",
        })
    }
}

/// A checksum a repository publishes next to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Checksum {
    algorithm: Algorithm,
    /// Lowercase hexadecimal.
    hex: String,
}

impl Checksum {
    /// Reads a checksum file: the hash, optionally followed by the file name (`<hash>  lib-1.0.jar`).
    fn parse(algorithm: Algorithm, body: &str) -> Option<Self> {
        let hex = body.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        (hex.len() == algorithm.hex_len() && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(Self { algorithm, hex })
    }
}

/// Whether `rel` is itself a checksum or signature, which nothing is published next to.
fn is_checksum_file(rel: &str) -> bool {
    [".sha1", ".sha256", ".sha512", ".md5", ".asc"].iter().any(|ext| rel.ends_with(ext))
}

/// The `host[:port]` of an `http(s)` URL.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or(rest);
    authority.rsplit_once('@').map_or(authority, |(_, host)| host)
}

fn unreachable_message(host: &str) -> String {
    format!("cannot reach {host}; run `kargo fetch` while online, or use `--offline`")
}

/// Whether `url` names a file a repository may legitimately not have: a checksum, Gradle module
/// metadata or `maven-metadata.xml`.
fn is_optional_probe(url: &str) -> bool {
    [".sha1", ".sha256", ".module", "maven-metadata.xml"].iter().any(|suffix| url.ends_with(suffix))
}

fn describe_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..=89 => format!("{secs}s"),
        90..=5399 => format!("{}m", secs / 60),
        5400..=129_599 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
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
        bail!("refusing to use the suspicious repository path `{rel}`");
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
    fn finds_sources_jars() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let write = |rel: &str| {
            let path = repo.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"zip").unwrap();
        };
        write("com/acme/lib/1.0/lib-1.0-sources.jar");
        write("com/acme/lib/2.0-SNAPSHOT/lib-2.0-20240101.120000-1-sources.jar");
        let client = local_client(repo.path(), home.path());
        let lib = ModuleId::new("com.acme", "lib");
        // Nothing is cached until the sources are fetched.
        assert_eq!(client.cached_sources(&lib, "1.0", &[]), None);
        let fetched = client.fetch_sources(&lib, "1.0").unwrap().unwrap();
        assert!(fetched.ends_with("lib-1.0-sources.jar"));
        assert_eq!(client.cached_sources(&lib, "1.0", &[]), Some(fetched));
        assert_eq!(client.fetch_sources(&ModuleId::new("com.acme", "other"), "1.0").unwrap(), None);

        // A SNAPSHOT's sources belong to the build its jar came from.
        let rel = "com/acme/lib/2.0-SNAPSHOT/lib-2.0-20240101.120000-1-sources.jar";
        client.fetch(rel).unwrap().unwrap();
        let jar = home.path().join("lib-2.0-20240101.120000-1.jar");
        let found = client.cached_sources(&lib, "2.0-SNAPSHOT", &[jar]).unwrap();
        assert!(found.ends_with("lib-2.0-20240101.120000-1-sources.jar"));
        assert_eq!(client.cached_sources(&lib, "2.0-SNAPSHOT", &[]), None);
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

    fn deploy_snapshot(repo_dir: &Path, timestamp: &str, build: u32) {
        let dir = repo_dir.join("com/acme/lib/1.0-SNAPSHOT");
        std::fs::create_dir_all(&dir).unwrap();
        let file_version = format!("1.0-{timestamp}-{build}");
        let updated = timestamp.replace('.', "");
        let entry = |ext: &str| {
            format!(
                "<snapshotVersion><extension>{ext}</extension><value>{file_version}</value><updated>{updated}</updated></snapshotVersion>"
            )
        };
        std::fs::write(
            dir.join("maven-metadata.xml"),
            format!("<metadata><versioning><snapshotVersions>{}{}</snapshotVersions></versioning></metadata>", entry("jar"), entry("pom")),
        )
        .unwrap();
        std::fs::write(dir.join(format!("lib-{file_version}.jar")), format!("jar {build}")).unwrap();
    }

    #[test]
    fn snapshots_resolve_to_the_newest_timestamped_build() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let module = ModuleId::new("com.acme", "lib");
        let jar = Artifact::new(module.clone(), "1.0-SNAPSHOT");
        let path = |client: &MavenClient, artifact: &Artifact| client.artifact_path(artifact).unwrap();

        // Without metadata the plain `-SNAPSHOT` name is used, as is the case for releases.
        let client = local_client(repo.path(), home.path());
        assert_eq!(path(&client, &jar), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-SNAPSHOT.jar");
        assert_eq!(path(&client, &Artifact::new(module.clone(), "1.0")), "com/acme/lib/1.0/lib-1.0.jar");

        deploy_snapshot(repo.path(), "20240101.120000", 1);
        let client = local_client(repo.path(), home.path());
        assert_eq!(path(&client, &jar), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar");
        assert_eq!(path(&client, &jar.clone().with_extension("pom")), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.pom");
        assert_eq!(path(&client, &jar.clone().with_extension("module")), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-SNAPSHOT.module");
        let fetched = client.fetch_required(&path(&client, &jar)).unwrap();
        assert_eq!(std::fs::read_to_string(fetched).unwrap(), "jar 1");

        // A local repository is never read from a stale copy: the next run sees the new build,
        // while this run keeps the answer it already gave.
        deploy_snapshot(repo.path(), "20240102.080000", 2);
        assert_eq!(path(&client, &jar), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar");
        let client = local_client(repo.path(), home.path());
        assert_eq!(path(&client, &jar), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.080000-2.jar");

        // A build can be pinned, to keep a locked one while the repository moves on.
        client.pin_snapshot_build(&module, "1.0-SNAPSHOT", "1.0-20240101.120000-1");
        assert_eq!(path(&client, &jar), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar");
        assert_eq!(path(&client, &jar.with_extension("pom")), "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.pom");
    }

    #[test]
    fn plain_snapshot_files_follow_their_repository_but_releases_do_not() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let snapshot = "com/acme/lib/2.0-SNAPSHOT/lib-2.0-SNAPSHOT.jar";
        let release = "com/acme/lib/1.0/lib-1.0.jar";
        let publish = |rel: &str, contents: &str| {
            let path = repo.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };
        let read = |client: &MavenClient, rel: &str| std::fs::read_to_string(client.fetch(rel).unwrap().unwrap()).unwrap();

        publish(snapshot, "one");
        publish(release, "one");
        let client = local_client(repo.path(), home.path());
        assert_eq!((read(&client, snapshot), read(&client, release)), ("one".to_owned(), "one".to_owned()));

        // `mvn install` overwrote both files. A run notices that for the SNAPSHOT only, and keeps its answer.
        publish(snapshot, "two");
        publish(release, "two");
        assert_eq!(read(&client, snapshot), "one");
        let client = local_client(repo.path(), home.path());
        assert_eq!((read(&client, snapshot), read(&client, release)), ("two".to_owned(), "one".to_owned()));
        assert!(client.is_verified(snapshot));

        // A file that is gone upstream stays in the cache.
        std::fs::remove_file(repo.path().join(snapshot)).unwrap();
        let client = local_client(repo.path(), home.path());
        assert_eq!(read(&client, snapshot), "two");
    }

    #[test]
    fn fetch_verified_replaces_corrupt_cached_files() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        std::fs::create_dir_all(repo.path().join("com/acme/lib/1.0")).unwrap();
        std::fs::write(repo.path().join(rel), "good").unwrap();

        let client = local_client(repo.path(), home.path());
        let cached = client.cache_path(&client.key_for(&client.repos[0], rel));
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
    fn fetch_verified_remembers_a_check_across_runs() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        std::fs::create_dir_all(repo.path().join("com/acme/lib/1.0")).unwrap();
        std::fs::write(repo.path().join(rel), "good").unwrap();

        let first = local_client(repo.path(), home.path());
        let cached = first.fetch_verified(rel).unwrap();
        first.verified.lock().unwrap().clear();
        assert!(!first.is_verified(rel));
        // The repository now disagrees, but the unchanged cached file is not asked about again.
        std::fs::write(repo.path().join(rel), "changed upstream").unwrap();
        let second = local_client(repo.path(), home.path());
        assert_eq!(second.fetch_verified(rel).unwrap(), cached);
        assert!(second.is_verified(rel));

        // A cached file that changed is checked again.
        std::fs::write(&cached, "garbage!").unwrap();
        let third = local_client(repo.path(), home.path());
        assert_eq!(std::fs::read_to_string(third.fetch_verified(rel).unwrap()).unwrap(), "changed upstream");
    }

    #[test]
    fn each_repository_has_its_own_cached_copy() {
        let home = tempfile::tempdir().unwrap();
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        let repo_with = |content: &str| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("com/acme/lib/1.0")).unwrap();
            std::fs::write(dir.path().join(rel), content).unwrap();
            dir
        };
        let (a, b) = (repo_with("from a"), repo_with("from b"));
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();

        let first = local_client(a.path(), home.path());
        let from_a = first.fetch(rel).unwrap().unwrap();
        assert_eq!(read(from_a.clone()), "from a");
        // The same repository again is served from the cache, even after it changed.
        std::fs::write(a.path().join(rel), "changed").unwrap();
        assert_eq!(read(local_client(a.path(), home.path()).fetch(rel).unwrap().unwrap()), "from a");

        // Another repository with the same coordinates gets its own copy, and the first stays.
        let second = local_client(b.path(), home.path());
        let from_b = second.fetch(rel).unwrap().unwrap();
        assert_ne!(from_a, from_b);
        assert_eq!(read(from_b.clone()), "from b");
        assert_eq!(read(second.fetch_verified(rel).unwrap()), "from b");
        assert_eq!(read(from_a), "from a");

        // The path of a cached copy, as `kargo.lock` names files, is the repository-relative path.
        assert_eq!(second.cached_copies(rel), [from_b]);
        let key = second.key_for(&second.repos[0], rel);
        assert_eq!(plain_rel(&key), rel);
        assert!(key.starts_with(&second.repos[0].cache_dir_name()));
    }

    #[test]
    fn only_released_files_of_public_repositories_are_shared() {
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let client =
            MavenClient::new(&config, vec![Repository::maven_central(), Repository::new("corp", "https://repo.corp.example/maven")]);
        let (central, corp) = (&client.repos[0], &client.repos[1]);
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        assert_eq!(client.key_for(central, rel), rel);
        assert!(client.key_for(corp, rel).starts_with('@'));
        // A SNAPSHOT changes under one name, so it is never shared.
        let snapshot = "com/acme/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-1.jar";
        assert!(client.key_for(central, snapshot).starts_with('@'));
        assert_eq!(plain_rel(&client.key_for(corp, snapshot)), snapshot);
    }

    #[test]
    fn repository_urls_are_normalized() {
        assert_eq!(Repository::new("x", "https://a/b/").url, "https://a/b");
        assert!(Repository::new("x", "file:///tmp/r").is_local());
        assert!(!Repository::maven_central().is_local());
    }

    #[test]
    fn unreachable_hosts_are_remembered_and_stale_metadata_is_kept() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cache = tempfile::tempdir().unwrap();
        let config = Config::new(cache.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let client = MavenClient::new(&config, vec![Repository::new("down", format!("http://127.0.0.1:{port}/maven2"))]);
        let module = ModuleId::new("org.example", "lib");
        let stale = client.cache_dir().join(module.repo_dir()).join("maven-metadata-down.xml");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "<metadata><versioning><versions><version>1.0</version></versions></versioning></metadata>").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        std::fs::File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();

        let versions = client.versions(&module).unwrap();
        assert_eq!(versions.len(), 1, "the stale metadata is still used");
        assert!(stale.is_file(), "a transport error keeps the cached metadata");

        let err = client.fetch("org/example/lib/1.0/lib-1.0.pom").unwrap_err();
        assert!(format!("{err:#}").contains("cannot reach 127.0.0.1"), "{err:#}");
    }

    /// A client of a repository served over HTTP with `files` (path below the root -> content); anything else is a 404.
    fn http_client(
        files: Vec<(String, String)>,
        require_checksums: bool,
    ) -> (crate::testserver::TestServer, MavenClient, tempfile::TempDir) {
        let server =
            crate::testserver::TestServer::start(move |request| match files.iter().find(|(path, _)| request.path == format!("/{path}")) {
                Some((_, content)) => (200, content.clone()),
                None => (404, String::new()),
            });
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_require_checksums(require_checksums);
        let client = MavenClient::new(&config, vec![Repository::new("remote", &server.url)]);
        (server, client, home)
    }

    #[test]
    fn downloads_are_checked_against_the_published_sha256_first() {
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        let sha256 = hash::sha256_bytes(b"good");
        let file = |path: &str, content: &str| (path.to_owned(), content.to_owned());

        // A SHA-256 wins over a SHA-1, even a wrong one.
        let (_server, client, _home) = http_client(
            vec![
                file(rel, "good"),
                file(&format!("{rel}.sha256"), &format!("{sha256}  lib-1.0.jar\n")),
                file(&format!("{rel}.sha1"), "0".repeat(40).as_str()),
            ],
            false,
        );
        assert_eq!(std::fs::read_to_string(client.fetch_required(rel).unwrap()).unwrap(), "good");
        assert!(client.is_verified(rel));

        let (_server, client, _home) = http_client(vec![file(rel, "evil"), file(&format!("{rel}.sha256"), &sha256)], false);
        let err = format!("{:#}", client.fetch_required(rel).unwrap_err());
        assert!(err.contains(&format!("expected sha256 {sha256}")), "{err}");
        assert!(client.cached_copies(rel).is_empty(), "a file that fails its check is not cached");

        // Without a SHA-256, the SHA-1 is checked.
        let (server, client, _home) =
            http_client(vec![file(rel, "evil"), file(&format!("{rel}.sha1"), &hash::checksums_bytes(b"good").sha1)], false);
        let err = format!("{:#}", client.fetch_required(rel).unwrap_err());
        assert!(err.contains("expected sha1"), "{err}");
        let asked: Vec<String> = server.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(asked, [format!("/{rel}"), format!("/{rel}.sha256"), format!("/{rel}.sha1")]);
    }

    #[test]
    fn require_checksums_rejects_files_without_one() {
        let rel = "com/acme/lib/1.0/lib-1.0.jar";
        let files = vec![(rel.to_owned(), "bytes".to_owned())];
        let (_server, client, _home) = http_client(files.clone(), false);
        assert!(client.fetch_required(rel).is_ok(), "accepted unverified by default");
        assert!(!client.is_verified(rel));

        let (_server, client, _home) = http_client(files.clone(), true);
        let err = format!("{:#}", client.fetch_required(rel).unwrap_err());
        assert!(err.contains("has no published checksum") && err.contains("--require-checksums"), "{err}");
        assert!(client.cached_copies(rel).is_empty());
        // A file kargo.lock pins is checked against the lock instead.
        assert_eq!(std::fs::read_to_string(client.fetch_pinned(rel).unwrap()).unwrap(), "bytes");

        // A cached file that was never checked fails too, before it is locked.
        let (_server, client, _home) = http_client(files, true);
        let cached = client.cache_path(&client.key_for(&client.repos[0], rel));
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, "bytes").unwrap();
        assert!(client.fetch_verified(rel).is_err());
    }

    #[test]
    fn checksum_files() {
        assert_eq!(Checksum::parse(Algorithm::Sha1, &format!("{}  x.jar\n", "A".repeat(40))).unwrap().hex, "a".repeat(40));
        assert!(Checksum::parse(Algorithm::Sha256, &"a".repeat(40)).is_none(), "a SHA-1 is no SHA-256");
        assert!(Checksum::parse(Algorithm::Sha1, "<html>not found</html>").is_none());
        assert!(is_checksum_file("a/b.jar.sha256") && is_checksum_file("a/b.jar.asc") && !is_checksum_file("a/b.jar"));
    }

    #[test]
    fn optional_probes_and_hosts() {
        assert!(is_optional_probe("https://h/a/b.jar.sha1"));
        assert!(is_optional_probe("https://h/a/b.jar.sha256"));
        assert!(is_optional_probe("https://h/a/b.module"));
        assert!(!is_optional_probe("https://h/a/b.jar"));
        assert_eq!(host_of("https://user:pw@repo.example.com:8443/maven2/a"), "repo.example.com:8443");
    }
}
