//! Gradle version catalogs (`gradle/libs.versions.toml`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use super::dsl::normalize_alias;

/// A library of a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub group: String,
    pub artifact: String,
    pub version: Option<String>,
}

/// A plugin of a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPlugin {
    pub id: String,
    pub version: Option<String>,
}

/// One catalog. Aliases are normalized (`ktor-server-core` is `ktor.server.core`), the way Gradle's
/// accessors spell them.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub name: String,
    pub versions: HashMap<String, String>,
    pub libraries: HashMap<String, Library>,
    pub plugins: HashMap<String, CatalogPlugin>,
    pub bundles: HashMap<String, Vec<String>>,
}

/// The version a `version` entry stands for: a string, or a table with `strictly`, `require` or `prefer`.
fn rich_version(value: &toml::Value, versions: &HashMap<String, String>) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(table) => {
            if let Some(reference) = table.get("ref").and_then(toml::Value::as_str) {
                return versions.get(&normalize_alias(reference)).cloned();
            }
            ["strictly", "require", "prefer"].iter().find_map(|key| table.get(*key).and_then(toml::Value::as_str)).map(str::to_owned)
        }
        _ => None,
    }
}

impl Catalog {
    pub fn parse(name: &str, text: &str) -> Result<Self> {
        let doc: toml::Table = toml::from_str(text).context("malformed version catalog")?;
        let mut catalog = Catalog { name: name.to_owned(), ..Catalog::default() };
        if let Some(versions) = doc.get("versions").and_then(toml::Value::as_table) {
            for (alias, value) in versions {
                if let Some(version) = rich_version(value, &HashMap::new()) {
                    catalog.versions.insert(normalize_alias(alias), version);
                }
            }
        }
        if let Some(libraries) = doc.get("libraries").and_then(toml::Value::as_table) {
            for (alias, value) in libraries {
                if let Some(library) = library(value, &catalog.versions) {
                    catalog.libraries.insert(normalize_alias(alias), library);
                }
            }
        }
        if let Some(plugins) = doc.get("plugins").and_then(toml::Value::as_table) {
            for (alias, value) in plugins {
                let plugin = match value {
                    toml::Value::String(s) => {
                        let (id, version) = s.split_once(':').map_or((s.as_str(), None), |(id, v)| (id, Some(v.to_owned())));
                        Some(CatalogPlugin { id: id.to_owned(), version })
                    }
                    toml::Value::Table(table) => table.get("id").and_then(toml::Value::as_str).map(|id| CatalogPlugin {
                        id: id.to_owned(),
                        version: table.get("version").and_then(|v| rich_version(v, &catalog.versions)),
                    }),
                    _ => None,
                };
                if let Some(plugin) = plugin {
                    catalog.plugins.insert(normalize_alias(alias), plugin);
                }
            }
        }
        if let Some(bundles) = doc.get("bundles").and_then(toml::Value::as_table) {
            for (alias, value) in bundles {
                let members = value.as_array().into_iter().flatten().filter_map(toml::Value::as_str).map(normalize_alias).collect();
                catalog.bundles.insert(normalize_alias(alias), members);
            }
        }
        Ok(catalog)
    }
}

fn library(value: &toml::Value, versions: &HashMap<String, String>) -> Option<Library> {
    match value {
        toml::Value::String(s) => {
            let mut parts = s.splitn(3, ':');
            Some(Library { group: parts.next()?.to_owned(), artifact: parts.next()?.to_owned(), version: parts.next().map(str::to_owned) })
        }
        toml::Value::Table(table) => {
            let (group, artifact) = match table.get("module").and_then(toml::Value::as_str) {
                Some(module) => {
                    let (group, artifact) = module.split_once(':')?;
                    (group.to_owned(), artifact.to_owned())
                }
                None => (table.get("group")?.as_str()?.to_owned(), table.get("name")?.as_str()?.to_owned()),
            };
            Some(Library { group, artifact, version: table.get("version").and_then(|v| rich_version(v, versions)) })
        }
        _ => None,
    }
}

/// The catalogs of the build in `root`: `gradle/libs.versions.toml` as `libs`, and any other
/// `gradle/<name>.versions.toml` as `<name>`.
pub fn load_all(root: &Path) -> Vec<Catalog> {
    let Ok(entries) = std::fs::read_dir(root.join("gradle")) else { return Vec::new() };
    let mut catalogs = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".versions.toml") else { continue };
        if let Ok(text) = std::fs::read_to_string(entry.path())
            && let Ok(catalog) = Catalog::parse(name, &text)
        {
            catalogs.push(catalog);
        }
    }
    catalogs.sort_by(|a, b| a.name.cmp(&b.name));
    catalogs
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"
[versions]
kotlin = "2.0.20"
ktor = { strictly = "3.0.0" }
coroutines = "1.9.0"

[libraries]
ktor-server-core = { module = "io.ktor:ktor-server-core", version.ref = "ktor" }
kotlinx_coroutines = { group = "org.jetbrains.kotlinx", name = "kotlinx-coroutines-core", version = { ref = "coroutines" } }
logback = "ch.qos.logback:logback-classic:1.5.6"
no-version = { module = "org.slf4j:slf4j-api" }

[bundles]
ktor = ["ktor-server-core", "logback"]

[plugins]
kotlin-jvm = { id = "org.jetbrains.kotlin.jvm", version.ref = "kotlin" }
ktor = { id = "io.ktor.plugin", version = "3.0.0" }
"#;

    #[test]
    fn reads_every_table_with_normalized_aliases() {
        let catalog = Catalog::parse("libs", CATALOG).unwrap();
        assert_eq!(catalog.versions["ktor"], "3.0.0");
        let core = &catalog.libraries["ktor.server.core"];
        assert_eq!((core.group.as_str(), core.artifact.as_str(), core.version.as_deref()), ("io.ktor", "ktor-server-core", Some("3.0.0")));
        assert_eq!(catalog.libraries["kotlinx.coroutines"].version.as_deref(), Some("1.9.0"));
        assert_eq!(catalog.libraries["logback"].version.as_deref(), Some("1.5.6"));
        assert_eq!(catalog.libraries["no.version"].version, None);
        assert_eq!(catalog.bundles["ktor"], ["ktor.server.core", "logback"]);
        assert_eq!(catalog.plugins["kotlin.jvm"], CatalogPlugin { id: "org.jetbrains.kotlin.jvm".into(), version: Some("2.0.20".into()) });
    }
}
