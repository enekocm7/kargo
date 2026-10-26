//! Security advisories from OSV (<https://osv.dev>), for `kargo audit`: which advisories affect a
//! package version, how severe they are and which version fixes them.
//!
//! Packages are looked up with OSV's batch endpoint (`/v1/querybatch`, up to [`BATCH`] per
//! request), which answers with advisory ids; the advisories themselves come from `/v1/vulns/<id>`.
//! Both are cached under `~/.kargo/advisories/osv`: the answers for a package version for
//! [`QUERY_TTL`], the advisories until OSV reports a newer `modified` time for them. `--offline`
//! uses the cache whatever its age, `--refresh` ignores it. `KARGO_OSV_URL` points Kargo at another
//! OSV server, such as a mirror or the canned one of the tests.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use kargo_manifest::Severity;
use kargo_maven::Version;
use kargo_util::config::Config;

/// The public OSV API.
pub const DEFAULT_OSV_URL: &str = "https://api.osv.dev";
/// The most queries OSV takes in one batch request.
const BATCH: usize = 1000;
/// How long the advisories found for a package version are trusted before OSV is asked again.
pub const QUERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The package registries OSV knows that Kargo installs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ecosystem {
    Maven,
    Npm,
}

impl Ecosystem {
    /// The name OSV uses.
    pub fn osv_name(self) -> &'static str {
        match self {
            Ecosystem::Maven => "Maven",
            Ecosystem::Npm => "npm",
        }
    }

    /// The name in Kargo's output (`--format json`).
    pub fn as_str(self) -> &'static str {
        match self {
            Ecosystem::Maven => "maven",
            Ecosystem::Npm => "npm",
        }
    }
}

/// A package version to look up: `group:artifact` for Maven, the package name for npm.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Query {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
}

/// An advisory that affects a queried package version.
#[derive(Debug, Clone, PartialEq)]
pub struct Advisory {
    /// Such as `GHSA-xxxx-xxxx-xxxx`.
    pub id: String,
    /// Other ids of the same advisory, such as its `CVE-...`.
    pub aliases: Vec<String>,
    pub summary: String,
    /// `None` when the advisory gives neither a CVSS v3 vector nor a severity.
    pub severity: Option<Severity>,
    /// The CVSS v3 base score, when the advisory has a vector.
    pub score: Option<f64>,
    /// The versions of the queried package that fix it, lowest first.
    pub fixed: Vec<String>,
    pub url: String,
}

impl Advisory {
    /// Whether `id` names this advisory, by its id or an alias (ignoring case).
    pub fn is_named(&self, id: &str) -> bool {
        self.id.eq_ignore_ascii_case(id) || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(id))
    }

    /// The lowest fixed version above `current`.
    pub fn fixed_after(&self, current: &str) -> Option<&str> {
        let current = Version::parse(current);
        self.fixed.iter().filter(|f| Version::parse(f) > current).min_by_key(|f| Version::parse(f)).map(String::as_str)
    }
}

/// The OSV API with its cache.
pub struct Osv<'a> {
    config: &'a Config,
    base: String,
    cache: PathBuf,
}

/// The cached answer for one package version.
#[derive(Debug, Serialize, Deserialize)]
struct CachedQuery {
    /// Seconds since the Unix epoch.
    fetched: u64,
    vulns: Vec<VulnRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VulnRef {
    id: String,
    #[serde(default)]
    modified: Option<String>,
}

impl<'a> Osv<'a> {
    /// The server of `KARGO_OSV_URL`, else the public OSV API.
    pub fn new(config: &'a Config) -> Self {
        let base = std::env::var("KARGO_OSV_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_OSV_URL.to_owned());
        Self { config, base: base.trim_end_matches('/').to_owned(), cache: config.home().join("advisories").join("osv") }
    }

    /// The URL advisories are read from.
    pub fn url(&self) -> &str {
        &self.base
    }

    /// The advisories that affect each query, in the order of `queries`.
    pub fn advisories(&self, queries: &[Query]) -> Result<Vec<Vec<Advisory>>> {
        let refs = self.query(queries)?;
        let mut wanted: HashMap<&str, Option<&str>> = HashMap::new();
        for r in refs.iter().flatten() {
            wanted.insert(&r.id, r.modified.as_deref());
        }
        let ids: Vec<(&str, Option<&str>)> = wanted.into_iter().collect();
        let fetched = kargo_maven::par_map_io(&ids, |(id, modified)| self.vuln(id, *modified).map(|v| (id.to_string(), v)));
        let vulns: HashMap<String, Value> = fetched.into_iter().collect::<Result<_>>()?;
        Ok(queries
            .iter()
            .zip(&refs)
            .map(|(q, refs)| {
                let mut found: Vec<Advisory> =
                    refs.iter().filter_map(|r| vulns.get(&r.id)).filter_map(|v| advisory(v, q.ecosystem, &q.name)).collect();
                found.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.id.cmp(&b.id)));
                found
            })
            .collect())
    }

    /// The advisory ids for each query: from the cache when fresh (or offline), the others in batches.
    fn query(&self, queries: &[Query]) -> Result<Vec<Vec<VulnRef>>> {
        let now = now();
        let mut out: Vec<Option<Vec<VulnRef>>> = Vec::with_capacity(queries.len());
        let mut missing = Vec::new();
        for (i, q) in queries.iter().enumerate() {
            let cached = (!self.config.refresh()).then(|| self.cached_query(q)).flatten();
            match cached {
                Some(c) if self.config.offline() || now.saturating_sub(c.fetched) < QUERY_TTL.as_secs() => out.push(Some(c.vulns)),
                _ => {
                    out.push(None);
                    missing.push(i);
                }
            }
        }
        if !missing.is_empty() && self.config.offline() {
            let q = &queries[missing[0]];
            bail!(
                "no cached advisories for {} {} {} (and {} more); run `kargo audit` once without --offline",
                q.ecosystem.as_str(),
                q.name,
                q.version,
                missing.len() - 1
            );
        }
        for chunk in missing.chunks(BATCH) {
            let body = serde_json::json!({
                "queries": chunk.iter().map(|&i| {
                    let q = &queries[i];
                    serde_json::json!({ "package": { "ecosystem": q.ecosystem.osv_name(), "name": q.name }, "version": q.version })
                }).collect::<Vec<_>>(),
            });
            let answer = self.post("/v1/querybatch", &body)?;
            let results = answer.get("results").and_then(Value::as_array).context("OSV answered the batch query without `results`")?;
            if results.len() != chunk.len() {
                bail!("OSV answered {} of {} queries", results.len(), chunk.len());
            }
            for (&i, result) in chunk.iter().zip(results) {
                let vulns: Vec<VulnRef> = result
                    .get("vulns")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .context("OSV answered with an unexpected list of advisories")?
                    .unwrap_or_default();
                let cached = CachedQuery { fetched: now, vulns };
                self.store(&self.query_path(&queries[i]), &serde_json::to_string(&cached)?);
                out[i] = Some(cached.vulns);
            }
        }
        Ok(out.into_iter().map(Option::unwrap_or_default).collect())
    }

    /// The advisory `id`: cached unless OSV reports a different `modified` time for it.
    fn vuln(&self, id: &str, modified: Option<&str>) -> Result<Value> {
        let path = self.cache.join("vulns").join(format!("{}.json", file_name(id)));
        if !self.config.refresh()
            && let Some(cached) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            let current = cached.get("modified").and_then(Value::as_str);
            if self.config.offline() || modified.is_none() || current == modified {
                return Ok(cached);
            }
        }
        if self.config.offline() {
            bail!("advisory {id} is not cached; run `kargo audit` once without --offline");
        }
        let vuln = self.get(&format!("/v1/vulns/{id}"))?;
        self.store(&path, &serde_json::to_string(&vuln)?);
        Ok(vuln)
    }

    fn cached_query(&self, q: &Query) -> Option<CachedQuery> {
        serde_json::from_str(&std::fs::read_to_string(self.query_path(q)).ok()?).ok()
    }

    fn query_path(&self, q: &Query) -> PathBuf {
        let key =
            kargo_util::hash::sha256_bytes(format!("{}\n{}\n{}\n{}", self.base, q.ecosystem.osv_name(), q.name, q.version).as_bytes());
        self.cache.join("queries").join(format!("{}.json", &key[..32]))
    }

    /// Caches `text` at `path`; a cache that cannot be written only costs a request next time.
    fn store(&self, path: &std::path::Path, text: &str) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = kargo_util::fs::write_string_atomic(path, text);
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let response = kargo_util::http::agent(self.config, Duration::from_secs(60))
            .post(&url)
            .header("Content-Type", "application/json")
            .send(serde_json::to_string(body)?.as_bytes())
            .with_context(|| format!("failed to reach the advisory database at {}; check your network connection", self.base))?;
        read_json(&url, response)
    }

    fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let response = kargo_util::http::agent(self.config, Duration::from_secs(60))
            .get(&url)
            .call()
            .with_context(|| format!("failed to reach the advisory database at {}; check your network connection", self.base))?;
        read_json(&url, response)
    }
}

fn read_json(url: &str, response: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = response.status().as_u16();
    let mut text = String::new();
    response.into_body().into_reader().take(64 * 1024 * 1024).read_to_string(&mut text).with_context(|| format!("failed to read {url}"))?;
    if !(200..=299).contains(&status) {
        bail!("the advisory database answered HTTP {status} for {url}: {}", text.trim().chars().take(300).collect::<String>());
    }
    serde_json::from_str(&text).with_context(|| format!("the advisory database answered {url} with invalid JSON"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// An advisory id as a file name: ids are letters, digits and `-`, but a server could send anything.
fn file_name(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// Reads an OSV advisory for the package `name` of `ecosystem`; `None` for a withdrawn one.
pub fn advisory(vuln: &Value, ecosystem: Ecosystem, name: &str) -> Option<Advisory> {
    if vuln.get("withdrawn").and_then(Value::as_str).is_some() {
        return None;
    }
    let id = vuln.get("id")?.as_str()?.to_owned();
    let strings = |key: &str| -> Vec<String> {
        vuln.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let summary = vuln
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| vuln.get("details").and_then(Value::as_str).and_then(|d| d.lines().find(|l| !l.trim().is_empty())))
        .unwrap_or("")
        .trim()
        .to_owned();
    let affected: Vec<&Value> = vuln
        .get("affected")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|entry| {
                    let package = entry.get("package");
                    let eco = package.and_then(|p| p.get("ecosystem")).and_then(Value::as_str);
                    let pkg = package.and_then(|p| p.get("name")).and_then(Value::as_str);
                    eco == Some(ecosystem.osv_name()) && pkg == Some(name)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut fixed: Vec<String> = affected
        .iter()
        .flat_map(|entry| entry.get("ranges").and_then(Value::as_array).into_iter().flatten())
        .flat_map(|range| range.get("events").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|event| event.get("fixed").and_then(Value::as_str).map(str::to_owned))
        .collect();
    fixed.sort_by_key(|v| Version::parse(v));
    fixed.dedup();

    let score = vuln
        .get("severity")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("CVSS_V3"))
        .filter_map(|s| s.get("score").and_then(Value::as_str).and_then(cvss3_score))
        .reduce(f64::max);
    let named = || {
        let from = |v: Option<&Value>| v.and_then(|d| d.get("severity")).and_then(Value::as_str).and_then(named_severity);
        from(vuln.get("database_specific"))
            .or_else(|| affected.iter().find_map(|a| from(a.get("database_specific")).or_else(|| from(a.get("ecosystem_specific")))))
    };
    let severity = match score {
        Some(s) => Severity::from_score(s).or_else(named),
        None => named(),
    };
    Some(Advisory { url: format!("https://osv.dev/vulnerability/{id}"), id, aliases: strings("aliases"), summary, severity, score, fixed })
}

/// A severity as advisory databases spell it (`MODERATE` is GitHub's medium).
fn named_severity(s: &str) -> Option<Severity> {
    match s.to_ascii_uppercase().as_str() {
        "LOW" => Some(Severity::Low),
        "MODERATE" | "MEDIUM" => Some(Severity::Medium),
        "HIGH" => Some(Severity::High),
        "CRITICAL" => Some(Severity::Critical),
        _ => None,
    }
}

/// The base score of a CVSS v3.0 or v3.1 vector such as `CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H`.
pub fn cvss3_score(vector: &str) -> Option<f64> {
    let mut parts = vector.split('/');
    if !parts.next()?.starts_with("CVSS:3") {
        return None;
    }
    let metrics: HashMap<&str, &str> = parts.filter_map(|p| p.split_once(':')).collect();
    let metric = |name: &str| metrics.get(name).copied();
    let changed = match metric("S")? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let av = match metric("AV")? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match metric("AC")? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (metric("PR")?, changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match metric("UI")? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |name: &str| match metric(name)? {
        "H" => Some(0.56),
        "L" => Some(0.22),
        "N" => Some(0.0),
        _ => None,
    };
    let iss = 1.0 - (1.0 - cia("C")?) * (1.0 - cia("I")?) * (1.0 - cia("A")?);
    let impact = if changed { 7.52 * (iss - 0.029) - 3.25 * (iss - 0.02f64).powi(15) } else { 6.42 * iss };
    if impact <= 0.0 {
        return Some(0.0);
    }
    let exploitability = 8.22 * av * ac * pr * ui;
    let base = if changed { (1.08 * (impact + exploitability)).min(10.0) } else { (impact + exploitability).min(10.0) };
    Some(round_up(base))
}

/// CVSS v3.1's `Roundup`: the smallest number with one decimal that is at least `value`.
fn round_up(value: f64) -> f64 {
    let int = (value * 100_000.0).round() as i64;
    if int % 10_000 == 0 { int as f64 / 100_000.0 } else { ((int / 10_000) + 1) as f64 / 10.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_cvss_vectors() {
        assert_eq!(cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"), Some(9.8));
        assert_eq!(cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"), Some(6.1));
        assert_eq!(cvss3_score("CVSS:3.0/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N"), Some(5.9));
        assert_eq!(cvss3_score("CVSS:3.1/AV:L/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H"), Some(8.8));
        assert_eq!(cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N"), Some(0.0));
        assert_eq!(cvss3_score("CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"), None);
        assert_eq!(cvss3_score("CVSS:3.1/AV:N"), None);
    }

    fn vuln() -> Value {
        serde_json::json!({
            "id": "GHSA-aaaa-bbbb-cccc",
            "modified": "2026-01-01T00:00:00Z",
            "aliases": ["CVE-2026-0001"],
            "summary": "Remote code execution in widget",
            "severity": [{ "type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H" }],
            "affected": [
                {
                    "package": { "ecosystem": "Maven", "name": "com.acme:widget" },
                    "ranges": [{ "type": "ECOSYSTEM", "events": [
                        { "introduced": "0" }, { "fixed": "1.2.5" }, { "introduced": "2.0.0" }, { "fixed": "2.0.3" }
                    ] }]
                },
                {
                    "package": { "ecosystem": "Maven", "name": "com.acme:other" },
                    "ranges": [{ "type": "ECOSYSTEM", "events": [{ "introduced": "0" }, { "fixed": "9.0" }] }]
                }
            ],
            "database_specific": { "severity": "MODERATE" }
        })
    }

    #[test]
    fn reads_advisories() {
        let a = advisory(&vuln(), Ecosystem::Maven, "com.acme:widget").unwrap();
        assert_eq!(a.id, "GHSA-aaaa-bbbb-cccc");
        assert_eq!(a.score, Some(9.8));
        assert_eq!(a.severity, Some(Severity::Critical), "the CVSS score wins over the database's severity");
        assert_eq!(a.fixed, ["1.2.5", "2.0.3"], "only the fixes of the queried package");
        assert_eq!(a.fixed_after("1.2.0"), Some("1.2.5"));
        assert_eq!(a.fixed_after("2.0.1"), Some("2.0.3"));
        assert_eq!(a.fixed_after("2.1.0"), None);
        assert!(a.is_named("cve-2026-0001"));
        assert_eq!(a.url, "https://osv.dev/vulnerability/GHSA-aaaa-bbbb-cccc");

        let mut v = vuln();
        v.as_object_mut().unwrap().remove("severity");
        let a = advisory(&v, Ecosystem::Maven, "com.acme:widget").unwrap();
        assert_eq!((a.severity, a.score), (Some(Severity::Medium), None), "GitHub's MODERATE");
        v.as_object_mut().unwrap().remove("database_specific");
        assert_eq!(advisory(&v, Ecosystem::Maven, "com.acme:widget").unwrap().severity, None);

        v["withdrawn"] = "2026-02-01T00:00:00Z".into();
        assert_eq!(advisory(&v, Ecosystem::Maven, "com.acme:widget"), None);
    }
}
