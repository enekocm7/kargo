//! Format-preserving edits of `kargo.toml` (used by `kargo add` / `kargo remove`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use super::{Scope, TargetKind};
use crate::maven::ModuleId;

/// Which dependency table to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencySection {
    Main,
    Dev,
    Target(TargetKind),
}

impl DependencySection {
    fn path(self) -> Vec<&'static str> {
        match self {
            DependencySection::Main => vec!["dependencies"],
            DependencySection::Dev => vec!["dev-dependencies"],
            DependencySection::Target(kind) => vec!["target", kind.as_str(), "dependencies"],
        }
    }

    pub fn display(self) -> String {
        self.path().join(".")
    }
}

/// Options for a dependency entry written by `kargo add`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewDependency {
    pub version: Option<String>,
    pub scope: Option<Scope>,
    pub classifier: Option<String>,
    pub bom: bool,
}

pub struct ManifestEditor {
    path: PathBuf,
    doc: DocumentMut,
}

impl ManifestEditor {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_str(path, &text)
    }

    pub fn from_str(path: &Path, text: &str) -> Result<Self> {
        let doc: DocumentMut = text.parse().with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), doc })
    }

    pub fn save(&self) -> Result<()> {
        crate::util::fs::write_string_atomic(&self.path, &self.doc.to_string())
    }

    pub fn contents(&self) -> String {
        self.doc.to_string()
    }

    fn table_mut(&mut self, section: DependencySection) -> Result<&mut Table> {
        let path = section.path();
        let mut current = self.doc.as_table_mut();
        for (i, key) in path.iter().enumerate() {
            let is_last = i == path.len() - 1;
            let entry = current.entry(key).or_insert_with(|| {
                let mut t = Table::new();
                // Intermediate tables like [target] stay implicit.
                t.set_implicit(!is_last);
                Item::Table(t)
            });
            current = entry.as_table_mut().with_context(|| format!("`{}` in the manifest is not a table", path[..=i].join(".")))?;
        }
        Ok(current)
    }

    fn table(&self, section: DependencySection) -> Option<&Table> {
        let mut current = self.doc.as_table();
        for key in section.path() {
            current = current.get(key)?.as_table()?;
        }
        Some(current)
    }

    /// Finds the manifest key that declares `module` (either `"g:a"` or an alias).
    fn find_key(table: &Table, module: &ModuleId) -> Option<String> {
        let target = module.to_string();
        table.iter().find_map(|(key, item)| {
            if key == target {
                return Some(key.to_owned());
            }
            let coords = item.get("coords").and_then(|c| c.as_str()).map(|c| {
                let mut parts = c.split(':');
                format!("{}:{}", parts.next().unwrap_or(""), parts.next().unwrap_or(""))
            });
            let grouped = item.get("group").and_then(|g| g.as_str()).map(|g| {
                format!("{g}:{}", item.get("artifact").and_then(|a| a.as_str()).unwrap_or(key))
            });
            (coords.as_deref() == Some(target.as_str()) || grouped.as_deref() == Some(target.as_str())).then(|| key.to_owned())
        })
    }

    /// Adds or updates a dependency. Returns `true` if an existing entry was replaced.
    pub fn add_dependency(&mut self, section: DependencySection, module: &ModuleId, dep: &NewDependency) -> Result<bool> {
        let table = self.table_mut(section)?;
        let existing = Self::find_key(table, module);
        let key = existing.clone().unwrap_or_else(|| module.to_string());

        let simple = dep.scope.is_none_or(|s| s == Scope::Compile) && dep.classifier.is_none() && !dep.bom;
        let value: Item = match (&dep.version, simple) {
            (Some(v), true) => toml_edit::value(v.as_str()),
            _ => {
                let mut inline = InlineTable::new();
                // Preserve alias-style entries' coordinates.
                if key != module.to_string() {
                    inline.insert("coords", Value::from(module.to_string()));
                }
                if let Some(v) = &dep.version {
                    inline.insert("version", Value::from(v.as_str()));
                }
                match dep.scope {
                    Some(Scope::Runtime) => {
                        inline.insert("scope", Value::from("runtime"));
                    }
                    Some(Scope::Provided) => {
                        inline.insert("scope", Value::from("provided"));
                    }
                    _ => {}
                }
                if let Some(c) = &dep.classifier {
                    inline.insert("classifier", Value::from(c.as_str()));
                }
                if dep.bom {
                    inline.insert("bom", Value::from(true));
                }
                Item::Value(Value::InlineTable(inline))
            }
        };
        table.insert(&key, value);
        Ok(existing.is_some())
    }

    /// Removes a dependency by `group:artifact` or by its manifest key.
    pub fn remove_dependency(&mut self, section: DependencySection, name: &str) -> Result<String> {
        let Some(table) = self.table(section) else {
            bail!("the manifest has no [{}] section", section.display());
        };
        let key = match name.parse::<ModuleId>() {
            Ok(module) => Self::find_key(table, &module),
            Err(_) => table.contains_key(name).then(|| name.to_owned()),
        };
        let Some(key) = key else {
            bail!("dependency `{name}` is not declared in [{}]", section.display());
        };
        self.table_mut(section)?.remove(&key);
        Ok(key)
    }

    /// Enables a built-in framework (`[frameworks] name = true`) unless configured already.
    pub fn enable_framework(&mut self, name: &str) -> Result<bool> {
        let frameworks = self
            .doc
            .as_table_mut()
            .entry("frameworks")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .context("`frameworks` in the manifest is not a table")?;
        match frameworks.get(name) {
            Some(item) if item.as_bool() != Some(false) => Ok(false),
            _ => {
                frameworks.insert(name, toml_edit::value(true));
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"# My project
[package]
name = "demo"   # keep this comment
version = "0.1.0"

[dependencies]
"com.a:x" = "1.0"
alias = { coords = "com.b:y", version = "2.0" }
"#;

    fn editor() -> ManifestEditor {
        ManifestEditor::from_str(Path::new("kargo.toml"), BASE).unwrap()
    }

    #[test]
    fn adds_simple_dependency_preserving_format() {
        let mut ed = editor();
        let replaced = ed
            .add_dependency(DependencySection::Main, &"com.c:z".parse().unwrap(), &NewDependency { version: Some("3.0".into()), ..Default::default() })
            .unwrap();
        assert!(!replaced);
        let out = ed.contents();
        assert!(out.contains("# keep this comment"));
        assert!(out.contains(r#""com.c:z" = "3.0""#), "{out}");
        crate::manifest::Manifest::parse(&out).unwrap();
    }

    #[test]
    fn updates_existing_and_alias_entries() {
        let mut ed = editor();
        assert!(ed
            .add_dependency(DependencySection::Main, &"com.a:x".parse().unwrap(), &NewDependency { version: Some("1.1".into()), ..Default::default() })
            .unwrap());
        assert!(ed
            .add_dependency(
                DependencySection::Main,
                &"com.b:y".parse().unwrap(),
                &NewDependency { version: Some("2.1".into()), scope: Some(Scope::Runtime), ..Default::default() }
            )
            .unwrap());
        let out = ed.contents();
        assert!(out.contains(r#""com.a:x" = "1.1""#), "{out}");
        assert!(out.contains(r#"alias = { coords = "com.b:y", version = "2.1", scope = "runtime" }"#), "{out}");
        let m = crate::manifest::Manifest::parse(&out).unwrap();
        assert_eq!(m.dependencies.len(), 2);
    }

    #[test]
    fn adds_to_dev_and_target_sections() {
        let mut ed = editor();
        ed.add_dependency(DependencySection::Dev, &"t:t".parse().unwrap(), &NewDependency { version: Some("1".into()), ..Default::default() })
            .unwrap();
        ed.add_dependency(
            DependencySection::Target(TargetKind::Jvm),
            &"j:j".parse().unwrap(),
            &NewDependency { version: Some("1".into()), scope: Some(Scope::Provided), ..Default::default() },
        )
        .unwrap();
        let out = ed.contents();
        assert!(out.contains("[dev-dependencies]"), "{out}");
        assert!(out.contains("[target.jvm.dependencies]"), "{out}");
        assert!(!out.contains("[target]\n"), "intermediate table stays implicit: {out}");
        let m = crate::manifest::Manifest::parse(&out).unwrap();
        assert_eq!(m.dev_dependencies.len(), 1);
        assert_eq!(m.targets.dependencies(TargetKind::Jvm)[0].scope, Scope::Provided);
    }

    #[test]
    fn removes_by_module_or_key() {
        let mut ed = editor();
        assert_eq!(ed.remove_dependency(DependencySection::Main, "com.b:y").unwrap(), "alias");
        assert_eq!(ed.remove_dependency(DependencySection::Main, "com.a:x").unwrap(), "com.a:x");
        assert!(ed.remove_dependency(DependencySection::Main, "com.a:x").is_err());
        assert!(ed.remove_dependency(DependencySection::Dev, "x").is_err());
    }

    #[test]
    fn enables_frameworks_once() {
        let mut ed = editor();
        assert!(ed.enable_framework("serialization").unwrap());
        assert!(!ed.enable_framework("serialization").unwrap());
        let m = crate::manifest::Manifest::parse(&ed.contents()).unwrap();
        assert!(m.frameworks.serialization.is_some());
    }

    #[test]
    fn bom_entries_are_tables() {
        let mut ed = editor();
        ed.add_dependency(
            DependencySection::Main,
            &"org.springframework.boot:spring-boot-dependencies".parse().unwrap(),
            &NewDependency { version: Some("4.1.1".into()), bom: true, ..Default::default() },
        )
        .unwrap();
        let m = crate::manifest::Manifest::parse(&ed.contents()).unwrap();
        assert!(m.dependencies.iter().any(|d| d.bom));
    }
}
