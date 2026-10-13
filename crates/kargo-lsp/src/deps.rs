//! Dependencies in `kargo.toml`: `group:artifact` completion from the Maven Central search API,
//! version completion from the project's repositories, and hints at newer versions.
//!
//! Everything here may use the network, so the server calls it on worker threads and never from
//! its message loop. Offline, coordinate search returns nothing and versions come from the cache
//! (and from repositories on disk), as everywhere else in Kargo.

use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use kargo_manifest::edit::DependencySection;
use kargo_maven::{MavenClient, ModuleId, Repository, Version};
use kargo_util::config::Config;

use crate::manifest::{self, Context};

/// The Maven Central search API.
pub const DEFAULT_SEARCH_URL: &str = "https://search.maven.org/solrsearch/select";
/// Overrides [`DEFAULT_SEARCH_URL`], for a mirror or a test.
pub const SEARCH_URL_ENV: &str = "KARGO_MAVEN_SEARCH_URL";
/// How long a search result stays in the cache.
const SEARCH_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a search waits for the next keystroke before it goes to the network.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Results asked of the search API; it sorts by date, so Kargo asks for more and ranks them itself.
const SEARCH_ROWS: usize = 60;
/// Completion items offered for one search.
const MAX_ITEMS: usize = 30;

const KIND_MODULE: u32 = 9;
const KIND_VALUE: u32 = 12;

/// What the cursor is on, as far as dependencies are concerned.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// A `group:artifact` being typed: a dependency key (`key`) or a `coords` value. `range` is the
    /// text to replace, `partial` the coordinate typed so far.
    Coordinate { partial: String, range: Range<usize>, key: bool, quoted: bool, has_value: bool },
    /// The version of `module`; `range` is the text to replace.
    Version { module: ModuleId, range: Range<usize>, quoted: bool },
}

/// The length of the dependency table at the start of `path`: `dependencies`,
/// `dev-dependencies`, `workspace.dependencies` or `target.<t>.dependencies`.
fn dependency_table(path: &[String]) -> Option<usize> {
    match path {
        [first, ..] if first == "dependencies" || first == "dev-dependencies" => Some(1),
        [w, d, ..] if w == "workspace" && d == "dependencies" => Some(2),
        [t, _, d, ..] if t == "target" && d == "dependencies" => Some(3),
        _ => None,
    }
}

/// Where the token the cursor is in starts: after the last space, `=`, `{` or `,` on the line.
fn token_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '=' | '{' | ','))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// The dependency request at byte `offset`, if the cursor is on a dependency's key, `coords` or version.
pub fn request(text: &str, offset: usize) -> Option<Request> {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    match manifest::context(text, offset) {
        Context::Key { table, .. } => {
            dependency_table(&table)?;
            let start = token_start(text, offset).max(line_start);
            // A key at the start of the line: not inside an inline table or after a dotted key.
            if !text[line_start..start].trim().is_empty() {
                return None;
            }
            let token = &text[start..offset];
            let quoted = token.starts_with(['"', '\'']);
            let partial = token.trim_start_matches(['"', '\'']).to_owned();
            if partial.trim().len() < 2 || partial.contains(['"', '\'', ' ']) {
                return None;
            }
            // Replace the closing quote the editor may have inserted, and keep a value already there.
            let mut end = offset;
            if quoted && text[end..line_end].starts_with(['"', '\'']) {
                end += 1;
            }
            let has_value = text[end..line_end].trim_start().starts_with('=');
            Some(Request::Coordinate { partial, range: start..end, key: true, quoted, has_value })
        }
        Context::Value { key, quoted } => {
            let n = dependency_table(&key)?;
            let rest = &key[n..];
            let start = if quoted { text[..offset].rfind(['"', '\'']).map_or(offset, |i| i + 1) } else { token_start(text, offset) };
            match rest {
                [_, field] if field == "coords" && quoted => {
                    let partial = text[start..offset].to_owned();
                    (partial.len() >= 2).then_some(Request::Coordinate {
                        partial,
                        range: start..offset,
                        key: false,
                        quoted,
                        has_value: false,
                    })
                }
                [name] => Some(Request::Version { module: name.parse().ok()?, range: start..offset, quoted }),
                [name, field] if field == "version" => {
                    let module = name.parse().ok().or_else(|| coords_of(text, offset, &key[..n + 1])?.parse().ok())?;
                    Some(Request::Version { module, range: start..offset, quoted })
                }
                _ => None,
            }
        }
        Context::Header { .. } => None,
    }
}

/// The `coords = "..."` of the dependency at `path`, from the line at `offset` (an inline table)
/// or from its own `[dependencies.<name>]` table.
fn coords_of(text: &str, offset: usize, path: &[String]) -> Option<String> {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    let quoted_value = |s: &str| -> Option<String> {
        let i = s.find("coords")?;
        let after = s[i + "coords".len()..].trim_start().strip_prefix('=')?.trim_start();
        let q = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = &after[1..];
        Some(value[..value.find(q)?].to_owned())
    };
    if let Some(coords) = quoted_value(&text[line_start..line_end]) {
        return Some(coords);
    }
    // In `[dependencies.<name>]`, the lines of that table.
    let mut in_table = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_table = manifest::header_path(trimmed) == path;
        } else if in_table && trimmed.starts_with("coords") {
            return quoted_value(trimmed);
        }
    }
    None
}

/// A module found by the search API.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub group: String,
    pub artifact: String,
}

/// Searches Maven Central for coordinates, with a cache in memory and under
/// `~/.kargo/cache/maven-search`, and a debounce so a burst of keystrokes makes one request.
pub struct Searcher {
    url: String,
    cache: PathBuf,
    offline: bool,
    agent: ureq::Agent,
    generation: AtomicU64,
    memory: Mutex<HashMap<String, Vec<Hit>>>,
}

impl Searcher {
    pub fn new(config: &Config, url: Option<String>) -> Self {
        let url =
            url.or_else(|| std::env::var(SEARCH_URL_ENV).ok().filter(|u| !u.is_empty())).unwrap_or_else(|| DEFAULT_SEARCH_URL.to_owned());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .user_agent(concat!("kargo/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            url,
            cache: config.home().join("cache").join("maven-search"),
            offline: config.offline(),
            agent,
            generation: AtomicU64::new(0),
            memory: Mutex::new(HashMap::new()),
        }
    }

    /// The modules matching `partial` (`group:artifact-prefix`, or words), best first. Returns
    /// `None` when the search was superseded by a newer one or could not be made.
    pub fn search(&self, partial: &str) -> Option<Vec<Hit>> {
        let query = search_query(partial)?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hits) = self.memory.lock().expect("lock poisoned").get(&query) {
            return Some(hits.clone());
        }
        if self.offline {
            return None;
        }
        let file = self.cache.join(format!("{}.json", kargo_util::hash::sha256_bytes(query.as_bytes())));
        let fresh = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < SEARCH_TTL);
        let cached = fresh.then(|| std::fs::read_to_string(&file).ok()).flatten().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let body = match cached {
            Some(body) => body,
            None => {
                std::thread::sleep(DEBOUNCE);
                if self.generation.load(Ordering::SeqCst) != generation {
                    return None;
                }
                let body = self.fetch(&query).ok()?;
                if std::fs::create_dir_all(&self.cache).is_ok() {
                    let _ = kargo_util::fs::write_string_atomic(&file, &body.to_string());
                }
                body
            }
        };
        let hits = rank(partial, parse_hits(&body));
        self.memory.lock().expect("lock poisoned").insert(query, hits.clone());
        Some(hits)
    }

    fn fetch(&self, query: &str) -> Result<Value> {
        let url = format!("{}?q={}&rows={SEARCH_ROWS}&wt=json&fl=id,g,a", self.url, query_escape(query));
        let response = self.agent.get(&url).call().with_context(|| format!("failed to search {}", self.url))?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response.into_body().into_reader().take(4 * 1024 * 1024).read_to_string(&mut text)?;
        if !(200..=299).contains(&status) {
            bail!("the search returned HTTP {status}");
        }
        Ok(serde_json::from_str(&text)?)
    }
}

/// The search API query for a partial coordinate: `g:<group> AND a:<prefix>*` once a `:` is
/// typed, else the words as typed.
fn search_query(partial: &str) -> Option<String> {
    let partial = partial.trim();
    let valid = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    match partial.split_once(':') {
        Some((group, artifact)) if !group.is_empty() && valid(group) && valid(artifact) => {
            Some(if artifact.is_empty() { format!("g:{group}") } else { format!("g:{group} AND a:{artifact}*") })
        }
        Some(_) => None,
        None => (partial.len() >= 2 && valid(partial)).then(|| partial.to_owned()),
    }
}

fn query_escape(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn parse_hits(body: &Value) -> Vec<Hit> {
    body["response"]["docs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|doc| Some(Hit { group: doc["g"].as_str()?.to_owned(), artifact: doc["a"].as_str()?.to_owned() }))
        .collect()
}

/// Orders hits for `partial`: those that start with what was typed first, then shorter names, so
/// `ktor-server-core` comes before its per-platform `ktor-server-core-jvm`.
fn rank(partial: &str, mut hits: Vec<Hit>) -> Vec<Hit> {
    let partial = partial.trim();
    hits.sort_by_cached_key(|h| {
        let id = format!("{}:{}", h.group, h.artifact);
        (!id.starts_with(partial), id.len(), id)
    });
    hits.dedup();
    hits.truncate(MAX_ITEMS);
    hits
}

/// The completion items for a coordinate request.
pub fn coordinate_items(text: &str, request: &Request, hits: &[Hit]) -> Vec<Value> {
    let Request::Coordinate { range, key, quoted, has_value, .. } = request else { return Vec::new() };
    hits.iter()
        .enumerate()
        .map(|(i, hit)| {
            let id = format!("{}:{}", hit.group, hit.artifact);
            let (new_text, filter) = match (key, has_value) {
                (true, false) => (format!("\"{id}\" = "), if *quoted { format!("\"{id}") } else { id.clone() }),
                (true, true) => (format!("\"{id}\""), if *quoted { format!("\"{id}") } else { id.clone() }),
                (false, _) => (id.clone(), id.clone()),
            };
            json!({
                "label": id,
                "kind": KIND_MODULE,
                "detail": "Maven Central",
                "sortText": format!("{i:04}"),
                "filterText": filter,
                "textEdit": { "range": crate::text::range(text, range.clone()), "newText": new_text },
            })
        })
        .collect()
}

/// The repositories of the manifest at `path` with contents `text`, or Maven Central and Google
/// while it doesn't parse.
pub fn repositories(text: &str, path: &Path) -> Vec<Repository> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let workspace = manifest::enclosing_workspace(dir);
    match kargo_manifest::Manifest::parse_in(text, workspace.as_ref()) {
        Ok(m) => m.repositories,
        Err(_) => vec![Repository::maven_central(), Repository::google()],
    }
}

/// The published versions of `module`, newest stable versions first, then pre-releases.
pub fn versions(config: &Config, repositories: Vec<Repository>, module: &ModuleId) -> Vec<Version> {
    let client = MavenClient::new(config, repositories);
    let mut versions = client.versions(module).unwrap_or_default();
    versions.sort_by(|a, b| a.is_prerelease().cmp(&b.is_prerelease()).then_with(|| b.cmp(a)));
    versions
}

/// The completion items for a version request.
pub fn version_items(text: &str, request: &Request, versions: &[Version]) -> Vec<Value> {
    let Request::Version { range, quoted, .. } = request else { return Vec::new() };
    versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let new_text = if *quoted { v.to_string() } else { format!("\"{v}\"") };
            let mut item = json!({
                "label": v.to_string(),
                "kind": KIND_VALUE,
                "sortText": format!("{i:04}"),
                "filterText": new_text,
                "textEdit": { "range": crate::text::range(text, range.clone()), "newText": new_text },
            });
            if v.is_prerelease() {
                item["detail"] = json!("pre-release");
            }
            item
        })
        .collect()
}

/// A dependency with a newer version, as `kargo outdated` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub section: DependencySection,
    pub name: String,
    pub message: String,
}

/// The dependencies of the package at `path` that have newer versions, with the rules of
/// `kargo outdated`. Errors (a manifest that doesn't load) give no hints.
pub fn hints(config: &Config, path: &Path) -> Vec<Hint> {
    let Ok(project) = kargo_ops::workspace::Project::load(path) else { return Vec::new() };
    let Some(index) = project.current() else { return Vec::new() };
    let Ok(rows) = kargo_ops::ops::upgrade::outdated(config, project.package(index), &[], false) else { return Vec::new() };
    rows.into_iter()
        .filter_map(|row| {
            let message = match (&row.compatible, &row.latest) {
                (Some(compatible), Some(latest)) if compatible != latest => {
                    format!("`{}` {compatible} is available (latest: {latest})", row.module)
                }
                (Some(compatible), _) => format!("`{}` {compatible} is available", row.module),
                (None, Some(latest)) => format!("`{}` {latest} is available (not compatible with {})", row.module, row.current),
                (None, None) => return None,
            };
            Some(Hint { section: row.section, name: row.name, message })
        })
        .collect()
}

/// Where the key of the dependency a hint is about is in `text`, if it is still there.
pub fn locate(text: &str, hint: &Hint) -> Option<Range<usize>> {
    let doc = toml_edit::Document::parse(text.to_owned()).ok()?;
    let path = match hint.section {
        DependencySection::Main => vec!["dependencies"],
        DependencySection::Dev => vec!["dev-dependencies"],
        DependencySection::Target(kind) => vec!["target", kind.as_str(), "dependencies"],
    };
    let mut item = doc.as_item();
    for key in path {
        item = item.get(key)?;
    }
    item.as_table_like()?.get_key_value(&hint.name)?.0.span()
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn cursor(text: &str) -> (String, usize) {
        let offset = text.find('|').unwrap();
        (text.replacen('|', "", 1), offset)
    }

    fn req(text: &str) -> Option<Request> {
        let (text, offset) = cursor(text);
        request(&text, offset)
    }

    fn module(s: &str) -> ModuleId {
        s.parse().unwrap()
    }

    #[test]
    fn finds_dependency_requests() {
        assert_eq!(
            req("[dependencies]\n\"io.ktor:ktor-|\"\n"),
            Some(Request::Coordinate { partial: "io.ktor:ktor-".into(), range: 15..30, key: true, quoted: true, has_value: false })
        );
        assert!(matches!(req("[target.jvm.dependencies]\nio.kt|"), Some(Request::Coordinate { quoted: false, .. })));
        assert!(matches!(req("[dependencies]\n\"io.k|\" = \"1\""), Some(Request::Coordinate { has_value: true, .. })));
        assert!(matches!(
            req("[dependencies]\nktor = { coords = \"io.kt|"),
            Some(Request::Coordinate { key: false, ref partial, .. }) if partial == "io.kt"
        ));
        assert_eq!(req("[package]\nna|"), None);
        assert_eq!(req("[dependencies]\nktor = { co|"), None, "a key of the inline table");
        assert_eq!(req("[dependencies]\n\"i|"), None, "too short to search");

        let (text, offset) = cursor("[dependencies]\n\"g.x:a\" = \"1.|\"");
        assert_eq!(request(&text, offset), Some(Request::Version { module: module("g.x:a"), range: offset - 2..offset, quoted: true }));
        assert!(matches!(req("[dev-dependencies]\n\"g:a\" = |"), Some(Request::Version { quoted: false, .. })));
        assert!(matches!(req("[dependencies]\n\"g:a\" = { version = \"|"), Some(Request::Version { .. })));
        assert_eq!(
            req("[dependencies]\nk = { coords = \"g:a\", version = \"|"),
            Some(Request::Version { module: module("g:a"), range: 48..48, quoted: true })
        );
        let table = "[dependencies.k]\ncoords = \"g:b\"\nversion = \"|\"\n";
        assert!(matches!(req(table), Some(Request::Version { module: m, .. }) if m == module("g:b")));
        assert_eq!(req("[dependencies]\nk = { path = \"../k\", version = \"|"), None);
    }

    #[test]
    fn builds_search_queries() {
        assert_eq!(search_query("io.ktor:ktor-server").as_deref(), Some("g:io.ktor AND a:ktor-server*"));
        assert_eq!(search_query("io.ktor:").as_deref(), Some("g:io.ktor"));
        assert_eq!(search_query("ktor").as_deref(), Some("ktor"));
        assert_eq!(search_query("a\"b"), None);
        assert_eq!(query_escape("g:io.ktor AND a:x*"), "g%3Aio.ktor+AND+a%3Ax%2A");
    }

    #[test]
    fn ranks_hits() {
        let hit = |a: &str| Hit { group: "io.ktor".into(), artifact: a.into() };
        let ranked = rank("io.ktor:ktor-server-c", vec![hit("ktor-server-core-jvm"), hit("ktor-server-cio"), hit("ktor-server-core")]);
        let names: Vec<&str> = ranked.iter().map(|h| h.artifact.as_str()).collect();
        assert_eq!(names, ["ktor-server-cio", "ktor-server-core", "ktor-server-core-jvm"]);
    }

    /// A one-request HTTP server that answers with `body` and reports the request line.
    fn fake_search(body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/solrsearch/select", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or_default().to_owned();
            let _ = tx.send(request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (url, rx)
    }

    #[test]
    fn searches_and_caches() {
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let (url, requests) = fake_search(
            r#"{"response":{"docs":[{"id":"io.ktor:ktor-server-core-jvm","g":"io.ktor","a":"ktor-server-core-jvm"},{"g":"io.ktor","a":"ktor-server-core"}]}}"#,
        );
        let searcher = Searcher::new(&config, Some(url.clone()));
        let hits = searcher.search("io.ktor:ktor-server-co").unwrap();
        assert_eq!(hits[0].artifact, "ktor-server-core");
        assert!(requests.recv().unwrap().contains("q=g%3Aio.ktor+AND+a%3Aktor-server-co%2A"));
        // From the disk cache: the fake server is gone.
        let again = Searcher::new(&config, Some(url));
        assert_eq!(again.search("io.ktor:ktor-server-co").unwrap(), hits);

        let offline = Searcher::new(&config.clone().with_offline(true), Some("http://127.0.0.1:9/".into()));
        assert_eq!(offline.search("io.ktor:ktor-server-cio"), None, "no network offline");

        let (text, offset) = cursor("[dependencies]\n\"io.ktor:ktor-server-co|\"\n");
        let request = request(&text, offset).unwrap();
        let items = coordinate_items(&text, &request, &hits);
        assert_eq!(items[0]["textEdit"]["newText"], "\"io.ktor:ktor-server-core\" = ");
        assert_eq!(items[0]["filterText"], "\"io.ktor:ktor-server-core");
        assert_eq!(items[0]["textEdit"]["range"]["end"], json!({ "line": 1, "character": 24 }), "the closing quote is replaced");
    }

    fn publish(repo: &Path, artifact: &str, versions: &[&str]) {
        let dir = repo.join("com/acme").join(artifact);
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
        std::fs::write(
            dir.join("maven-metadata.xml"),
            format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"),
        )
        .unwrap();
    }

    #[test]
    fn lists_versions_and_hints_from_the_repositories() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
        let repo = project.path().join("repo");
        publish(&repo, "widget", &["1.0.0", "1.2.0", "2.0.0-RC1", "1.10.0"]);
        publish(&repo, "fresh", &["3.0"]);
        let url = format!("file://{}", repo.display()).replace('\\', "/");
        let text = format!(
            "[package]\nname = \"p\"\nversion = \"1.0\"\n\n[repositories]\nlocal = \"{url}\"\n\n[dependencies]\n\"com.acme:widget\" = \"1.0.0\"\n\"com.acme:fresh\" = \"3.0\"\n"
        );
        let path = project.path().join("kargo.toml");
        std::fs::write(&path, &text).unwrap();

        let found = versions(&config, repositories(&text, &path), &module("com.acme:widget"));
        let found: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert_eq!(found, ["1.10.0", "1.2.0", "1.0.0", "2.0.0-RC1"], "newest stable first, pre-releases last");

        let hints = hints(&config, &path);
        assert_eq!(
            hints,
            [Hint {
                section: DependencySection::Main,
                name: "com.acme:widget".into(),
                message: "`com.acme:widget` 1.10.0 is available".into()
            }]
        );
        assert_eq!(&text[locate(&text, &hints[0]).unwrap()], "\"com.acme:widget\"");
    }
}
