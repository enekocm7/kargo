//! The npm registry, for `kargo add --npm`, `kargo outdated` and `kargo upgrade`: which versions a package has,
//! what its dist-tags point at and whether it ships TypeScript declarations.
//!
//! Kargo asks the registry's HTTP API itself (`GET <registry>/<name>`) instead of running `npm view`, so
//! no package manager has to be installed to edit `kargo.toml`. The registry is the one npm would use:
//! `NPM_CONFIG_REGISTRY`, else the `registry` of the project's `.npmrc` and then of the user's (`~/.npmrc`, or
//! `NPM_CONFIG_USERCONFIG`), else <https://registry.npmjs.org>, with `@scope:registry` lines for scoped
//! packages and `//host/path/:_authToken` lines for private ones (`${VAR}` is replaced by the variable).
//!
//! What was read is kept under `~/.kargo/npm/registry`, and `--offline` answers from it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::npm_semver::Semver;
use kargo_util::config::Config;

/// The public registry.
pub const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// The registry settings of the `.npmrc` files and the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Npmrc {
    /// The registry for packages without a scope of their own.
    registry: Option<String>,
    /// `@scope` to registry URL.
    scoped: BTreeMap<String, String>,
    /// `//host/path/` (no scheme) to bearer token.
    tokens: BTreeMap<String, String>,
}

impl Npmrc {
    /// The settings that apply to a project in `project_dir`: its `.npmrc` over the user's, over nothing, with
    /// `NPM_CONFIG_REGISTRY` over both.
    pub fn load(project_dir: &Path) -> Npmrc {
        let mut rc = Npmrc::default();
        let user = std::env::var_os("NPM_CONFIG_USERCONFIG").map(PathBuf::from).or_else(|| {
            std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|home| PathBuf::from(home).join(".npmrc"))
        });
        for path in [user, Some(project_dir.join(".npmrc"))].into_iter().flatten() {
            if let Ok(text) = std::fs::read_to_string(path) {
                rc.merge(&text, &|name| std::env::var(name).ok());
            }
        }
        let from_env = std::env::var("NPM_CONFIG_REGISTRY").or_else(|_| std::env::var("npm_config_registry"));
        if let Some(url) = from_env.ok().filter(|u| !u.trim().is_empty()) {
            rc.registry = Some(url.trim().to_owned());
        }
        rc
    }

    /// Adds the settings of `.npmrc` text; later lines win. `env` looks up `${NAME}`.
    pub fn merge(&mut self, text: &str, env: &dyn Fn(&str) -> Option<String>) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with([';', '#']) {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), expand(value.trim().trim_matches(['"', '\'']), env));
            if value.is_empty() {
                continue;
            }
            if key == "registry" {
                self.registry = Some(value);
            } else if let Some(scope) = key.strip_suffix(":registry").filter(|s| s.starts_with('@')) {
                self.scoped.insert(scope.to_owned(), value);
            } else if let Some(prefix) = key.strip_prefix("//").and_then(|k| k.strip_suffix(":_authToken")) {
                self.tokens.insert(format!("//{prefix}"), value);
            }
        }
    }

    /// The registry URL (without a trailing slash) that serves `package`.
    pub fn registry_for(&self, package: &str) -> String {
        let scope = package.strip_prefix('@').and_then(|_| package.split('/').next());
        let url = scope.and_then(|s| self.scoped.get(s)).or(self.registry.as_ref()).map_or(DEFAULT_REGISTRY, String::as_str);
        url.trim_end_matches('/').to_owned()
    }

    /// The token for `registry`: the entry whose `//host/path/` is the longest prefix of the URL.
    pub fn token_for(&self, registry: &str) -> Option<&str> {
        let bare = registry.split_once("://").map_or(registry, |(_, rest)| rest);
        let bare = format!("//{}/", bare.trim_end_matches('/'));
        self.tokens
            .iter()
            .filter(|(prefix, _)| bare.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, t)| t.as_str())
    }
}

/// Replaces `${NAME}` with the variable, or with nothing when it is not set.
fn expand(value: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        match rest[start..].find('}') {
            Some(end) => {
                out.push_str(&env(&rest[start + 2..start + end]).unwrap_or_default());
                rest = &rest[start + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the registry says about a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    /// The registry it came from, for messages.
    pub registry: String,
    #[serde(rename = "dist-tags", default)]
    pub tags: BTreeMap<String, String>,
    /// Every published version, oldest first.
    pub versions: Vec<Published>,
}

/// A published version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub version: String,
    /// It ships declarations of its own (a `types` or `typings` field, or a `types` condition in `exports`).
    #[serde(default)]
    pub types: bool,
    #[serde(default)]
    pub deprecated: bool,
}

impl Package {
    /// The versions the registry lists, skipping ones that are not semantic versions.
    pub fn semvers(&self) -> Vec<Semver> {
        self.versions.iter().filter_map(|p| Semver::parse(&p.version)).collect()
    }

    /// The version a dist-tag points at.
    pub fn tag(&self, tag: &str) -> Option<Semver> {
        self.tags.get(tag).and_then(|v| Semver::parse(v))
    }

    pub fn published(&self, version: &Semver) -> Option<&Published> {
        self.versions.iter().find(|p| Semver::parse(&p.version).as_ref() == Some(version))
    }

    /// The version to depend on when none is given: what the `latest` tag points at if that is a release,
    /// else the highest release (any version with `prerelease`). `None` when there is nothing to pick.
    pub fn default_version(&self, prerelease: bool) -> Option<Semver> {
        let versions = self.semvers();
        let usable = |v: &&Semver| prerelease || !v.is_prerelease();
        if !prerelease && let Some(latest) = self.tag("latest").filter(|t| !t.is_prerelease()) {
            return Some(latest);
        }
        versions.iter().filter(usable).max().cloned()
    }
}

/// The registry, with the cache of what it answered.
pub struct Registry<'a> {
    config: &'a Config,
    npmrc: Npmrc,
    cache: PathBuf,
}

impl<'a> Registry<'a> {
    /// The registry settings for a project in `project_dir` (see [`Npmrc::load`]).
    pub fn new(config: &'a Config, project_dir: &Path) -> Self {
        Self::with(config, Npmrc::load(project_dir))
    }

    pub fn with(config: &'a Config, npmrc: Npmrc) -> Self {
        Self { config, npmrc, cache: config.home().join("npm").join("registry") }
    }

    /// The registry URL that serves `package`.
    pub fn url_for(&self, package: &str) -> String {
        self.npmrc.registry_for(package)
    }

    /// The package from the registry, or with `--offline` from what an earlier run cached.
    pub fn package(&self, name: &str) -> Result<Package> {
        let registry = self.url_for(name);
        let cached = self.cache_path(&registry, name);
        if self.config.offline() {
            let text = std::fs::read_to_string(&cached).ok();
            return text.and_then(|t| serde_json::from_str(&t).ok()).with_context(|| {
                format!(
                    "no cached registry data for the npm package `{name}` and --offline is set; give a version (`{name}@1.2.3`) or run once without --offline"
                )
            });
        }
        let package = self.fetch(&registry, name)?;
        if let Some(dir) = cached.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // A cache that cannot be written only costs a request later.
        if let Ok(text) = serde_json::to_string(&package) {
            let _ = kargo_util::fs::write_string_atomic(&cached, &text);
        }
        Ok(package)
    }

    /// The names of the packages the registry's search finds for `text` (`GET /-/v1/search`), best first. A
    /// registry without that endpoint finds none; this is for completion in an editor, so nothing here is an error.
    pub fn search(&self, text: &str, size: usize) -> Vec<String> {
        if self.config.offline() {
            return Vec::new();
        }
        let registry = self.url_for("");
        let encoded: String = text
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        let url = format!("{registry}/-/v1/search?text={encoded}&size={size}");
        let agent = kargo_util::http::agent(self.config, Duration::from_secs(10));
        let mut request = agent.get(&url).header("Accept", "application/json");
        if let Some(token) = self.npmrc.token_for(&registry) {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let Ok(mut response) = request.call() else { return Vec::new() };
        if response.status().as_u16() != 200 {
            return Vec::new();
        }
        let json: Value = response.body_mut().read_to_string().ok().and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default();
        json["objects"].as_array().into_iter().flatten().filter_map(|o| o["package"]["name"].as_str().map(str::to_owned)).collect()
    }

    fn cache_path(&self, registry: &str, name: &str) -> PathBuf {
        let key = kargo_util::hash::sha256_bytes(format!("{registry}\n{name}").as_bytes());
        self.cache.join(format!("{}.json", &key[..32]))
    }

    fn fetch(&self, registry: &str, name: &str) -> Result<Package> {
        // A scoped name travels with its slash encoded: `@scope%2Fname`.
        let url = format!("{registry}/{}", name.replacen('/', "%2F", 1));
        let agent = kargo_util::http::agent(self.config, Duration::from_secs(120));
        let mut request = agent.get(&url).header("Accept", "application/json");
        if let Some(token) = self.npmrc.token_for(registry) {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request.call().with_context(|| {
            format!("failed to reach the npm registry at {registry}; check your network connection, the `registry` of your .npmrc, or use --offline")
        })?;
        match response.status().as_u16() {
            200 => {}
            404 => {
                bail!("the npm package `{name}` was not found in the registry {registry}; check the name and the `registry` of your .npmrc")
            }
            401 | 403 => bail!(
                "the registry {registry} refused access to `{name}`; add `//{}/:_authToken=<token>` to your .npmrc",
                registry.split_once("://").map_or(registry, |(_, rest)| rest).trim_end_matches('/')
            ),
            status => bail!("the npm registry {registry} answered {status} for `{name}`"),
        }
        let body = response.body_mut().read_to_string().with_context(|| format!("failed to read the answer of {registry} for `{name}`"))?;
        let json: Value =
            serde_json::from_str(&body).with_context(|| format!("the npm registry {registry} sent invalid JSON for `{name}`"))?;
        package_from_json(name, registry, &json)
    }
}

/// Reads a registry document (a "packument") into the parts Kargo uses.
fn package_from_json(name: &str, registry: &str, json: &Value) -> Result<Package> {
    let versions = json
        .get("versions")
        .and_then(Value::as_object)
        .filter(|v| !v.is_empty())
        .with_context(|| format!("the npm package `{name}` has no published versions in the registry {registry}"))?;
    let mut versions: Vec<Published> = versions
        .iter()
        .map(|(version, info)| Published {
            version: version.clone(),
            types: info.get("types").or_else(|| info.get("typings")).is_some_and(|t| t.as_str().is_some_and(|s| !s.is_empty()))
                || info.get("exports").is_some_and(|e| e.to_string().contains("\"types\"")),
            deprecated: info.get("deprecated").is_some_and(|d| d.as_str().is_none_or(|s| !s.is_empty())),
        })
        .collect();
    versions.sort_by(|a, b| match (Semver::parse(&a.version), Semver::parse(&b.version)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let tags = json
        .get("dist-tags")
        .and_then(Value::as_object)
        .map(|t| t.iter().filter_map(|(tag, v)| v.as_str().map(|v| (tag.clone(), v.to_owned()))).collect())
        .unwrap_or_default();
    Ok(Package { name: name.to_owned(), registry: registry.to_owned(), tags, versions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(text: &str) -> Npmrc {
        let mut rc = Npmrc::default();
        rc.merge(text, &|name| (name == "NPM_TOKEN").then(|| "s3cret".to_owned()));
        rc
    }

    #[test]
    fn picks_the_registry_of_a_package() {
        let rc = rc("; a comment\nregistry=https://npm.example.com/\n@acme:registry=https://acme.example.com/npm\n# another\n");
        assert_eq!(rc.registry_for("left-pad"), "https://npm.example.com");
        assert_eq!(rc.registry_for("@acme/ui"), "https://acme.example.com/npm");
        assert_eq!(rc.registry_for("@other/ui"), "https://npm.example.com");
        assert_eq!(Npmrc::default().registry_for("left-pad"), DEFAULT_REGISTRY);
    }

    #[test]
    fn finds_the_token_for_a_registry() {
        let rc = rc(
            "//acme.example.com/npm/:_authToken=${NPM_TOKEN}\n//npm.example.com/:_authToken=other\n//gone.example.com/:_authToken=${MISSING}\n",
        );
        assert_eq!(rc.token_for("https://acme.example.com/npm"), Some("s3cret"));
        assert_eq!(rc.token_for("https://npm.example.com"), Some("other"));
        assert_eq!(rc.token_for("https://gone.example.com"), None, "an unset variable gives no token");
        assert_eq!(rc.token_for("https://registry.npmjs.org"), None);
    }

    #[test]
    fn reads_a_registry_document() {
        let json = serde_json::json!({
            "dist-tags": { "latest": "1.3.0", "next": "2.0.0-beta.1" },
            "versions": {
                "2.0.0-beta.1": { "types": "index.d.ts" },
                "1.3.0": { "exports": { ".": { "types": "./a.d.ts", "default": "./a.js" } } },
                "1.2.0": { "deprecated": "use 1.3.0" },
                "1.10.0": {},
            },
        });
        let package = package_from_json("left-pad", DEFAULT_REGISTRY, &json).unwrap();
        let order: Vec<&str> = package.versions.iter().map(|p| p.version.as_str()).collect();
        assert_eq!(order, ["1.2.0", "1.3.0", "1.10.0", "2.0.0-beta.1"]);
        assert!(package.versions[1].types && package.versions[3].types && !package.versions[2].types);
        assert!(package.versions[0].deprecated && !package.versions[1].deprecated);
        assert_eq!(package.default_version(false).unwrap().to_string(), "1.3.0", "the `latest` tag, not the highest release");
        assert_eq!(package.default_version(true).unwrap().to_string(), "2.0.0-beta.1");
        assert_eq!(package.tag("next").unwrap().to_string(), "2.0.0-beta.1");
        assert!(package_from_json("x", DEFAULT_REGISTRY, &serde_json::json!({})).is_err());
    }
}
