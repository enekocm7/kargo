//! Format-preserving edits of `kargo.toml` (used by `kargo add` / `kargo remove`, `kargo upgrade` and `kargo target`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{Decor, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::{Scope, TargetKind};
use kargo_maven::ModuleId;

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

/// The key of a target's npm packages: `npm-dependencies`, or with `dev` the one for the tests only.
pub fn npm_table_key(dev: bool) -> &'static str {
    if dev { "dev-npm-dependencies" } else { "npm-dependencies" }
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
        kargo_util::fs::write_string_atomic(&self.path, &self.doc.to_string())
    }

    pub fn contents(&self) -> String {
        self.doc.to_string()
    }

    /// Adds `member` to `[workspace] members` unless it is listed already. Returns whether it was added.
    pub fn add_workspace_member(&mut self, member: &str) -> Result<bool> {
        let workspace = self.doc.entry("workspace").or_insert_with(|| Item::Table(Table::new()));
        let workspace = workspace.as_table_like_mut().context("`workspace` in the manifest is not a table")?;
        let members = workspace.entry("members").or_insert(Item::Value(Value::Array(toml_edit::Array::new())));
        let members = members.as_array_mut().context("[workspace] `members` is not an array")?;
        if members.iter().any(|m| m.as_str() == Some(member)) {
            return Ok(false);
        }
        members.push(member);
        Ok(true)
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
            let grouped = item
                .get("group")
                .and_then(|g| g.as_str())
                .map(|g| format!("{g}:{}", item.get("artifact").and_then(|a| a.as_str()).unwrap_or(key)));
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
                    Some(Scope::Processor) => {
                        inline.insert("scope", Value::from("processor"));
                    }
                    Some(Scope::Ksp) => {
                        inline.insert("scope", Value::from("ksp"));
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

    /// Adds or updates a `name = { path = "..." }` dependency on another package. `path` is written
    /// with forward slashes. Returns `true` if an existing entry was replaced.
    pub fn add_path_dependency(&mut self, section: DependencySection, name: &str, path: &str, scope: Option<Scope>) -> Result<bool> {
        let table = self.table_mut(section)?;
        let mut inline = InlineTable::new();
        inline.insert("path", Value::from(path.replace('\\', "/")));
        match scope {
            Some(Scope::Runtime) => {
                inline.insert("scope", Value::from("runtime"));
            }
            Some(Scope::Provided) => {
                inline.insert("scope", Value::from("provided"));
            }
            _ => {}
        }
        let replaced = table.contains_key(name);
        table.insert(name, Item::Value(Value::InlineTable(inline)));
        Ok(replaced)
    }

    /// Whether the dependency `key` in `section` is `{ workspace = true }`, inherited from the workspace.
    pub fn is_inherited(&self, section: DependencySection, key: &str) -> bool {
        self.table(section).and_then(|t| t.get(key)).and_then(|item| item.get("workspace")).is_some()
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

    /// Changes the version of a declared dependency in place, keeping its formatting, comments and
    /// other keys. Returns the manifest key.
    pub fn set_dependency_version(&mut self, section: DependencySection, module: &ModuleId, version: &str) -> Result<String> {
        let key = self.table(section).and_then(|t| Self::find_key(t, module));
        let Some(key) = key else {
            bail!("dependency `{module}` is not declared in [{}]", section.display());
        };
        let item = self.table_mut(section)?.get_mut(&key).expect("key was just found");
        if item.is_str() {
            replace_str(item, version);
            return Ok(key);
        }
        let table = item.as_table_like_mut().with_context(|| format!("dependency `{key}` is neither a version string nor a table"))?;
        if let Some(v) = table.get_mut("version") {
            replace_str(v, version);
        } else if let Some(coords) = table.get_mut("coords").filter(|c| c.as_str().is_some_and(|s| s.split(':').count() >= 3)) {
            // `coords = "group:artifact:version[:classifier]"`
            let mut parts: Vec<String> = coords.as_str().unwrap_or_default().split(':').map(str::to_owned).collect();
            parts[2] = version.to_owned();
            replace_str(coords, &parts.join(":"));
        } else {
            bail!("dependency `{key}` in [{}] has no version to change (it is managed by a BOM)", section.display());
        }
        Ok(key)
    }

    /// The table of `kind`'s npm packages: `[target.<kind>.npm-dependencies]`, or with `dev` the one of
    /// the packages only the tests need. Created when `create` is set; `None` when it is missing.
    fn npm_table_mut(&mut self, kind: TargetKind, dev: bool, create: bool) -> Result<Option<&mut dyn TableLike>> {
        let key = npm_table_key(dev);
        let Some(target) = self.doc.get_mut("target").and_then(|t| t.get_mut(kind.as_str())) else {
            if create {
                bail!("[target.{kind}] is not declared in the manifest; add the target first with `kargo target add {kind}`");
            }
            return Ok(None);
        };
        let target = target.as_table_like_mut().with_context(|| format!("[target.{kind}] in the manifest is not a table"))?;
        if !target.contains_key(key) {
            if !create {
                return Ok(None);
            }
            target.insert(key, Item::Table(Table::new()));
        }
        let table =
            target.get_mut(key).and_then(Item::as_table_like_mut).with_context(|| format!("[target.{kind}] `{key}` is not a table"))?;
        Ok(Some(table))
    }

    /// The version `kind` declares for the npm package `name` in its `npm-dependencies` (or the test-only
    /// table with `dev`).
    pub fn npm_dependency(&mut self, kind: TargetKind, dev: bool, name: &str) -> Option<String> {
        let table = self.npm_table_mut(kind, dev, false).ok().flatten()?;
        table.get(name)?.as_str().map(str::to_owned)
    }

    /// Adds the npm package `name` at `version` (a semver range) to `[target.<kind>.npm-dependencies]`, or
    /// to the table of the test-only packages with `dev`. Returns the version it replaced, if any.
    pub fn add_npm_dependency(&mut self, kind: TargetKind, dev: bool, name: &str, version: &str) -> Result<Option<String>> {
        let table = self.npm_table_mut(kind, dev, true)?.expect("the table was created");
        let previous = table.get(name).and_then(|item| item.as_str()).map(str::to_owned);
        match table.get_mut(name).filter(|item| item.is_str()) {
            Some(item) => replace_str(item, version),
            None => {
                table.insert(name, toml_edit::value(version));
            }
        }
        Ok(previous)
    }

    /// Removes the npm package `name` from `[target.<kind>.npm-dependencies]` (or the test-only table with
    /// `dev`), and the table itself when nothing is left in it. Returns the version it had.
    pub fn remove_npm_dependency(&mut self, kind: TargetKind, dev: bool, name: &str) -> Result<String> {
        let key = npm_table_key(dev);
        let removed = self.npm_table_mut(kind, dev, false)?.and_then(|table| {
            let version = table.get(name).and_then(|item| item.as_str()).map(str::to_owned);
            version.inspect(|_| {
                table.remove(name);
            })
        });
        let Some(version) = removed else {
            bail!("npm package `{name}` is not declared in [target.{kind}.{key}]");
        };
        if self.npm_table_mut(kind, dev, false)?.is_some_and(|table| table.is_empty())
            && let Some(target) = self.doc.get_mut("target").and_then(|t| t.get_mut(kind.as_str())).and_then(Item::as_table_like_mut)
        {
            target.remove(key);
        }
        Ok(version)
    }

    /// Changes the version of the npm package `name` in `[target.<kind>.npm-dependencies]` in place,
    /// keeping its formatting and comments.
    pub fn set_npm_dependency_version(&mut self, kind: TargetKind, name: &str, version: &str) -> Result<()> {
        self.set_npm_version(kind, false, name, version)
    }

    /// [`set_npm_dependency_version`](Self::set_npm_dependency_version) for either npm table of `kind`.
    pub fn set_npm_version(&mut self, kind: TargetKind, dev: bool, name: &str, version: &str) -> Result<()> {
        let item = self.npm_table_mut(kind, dev, false)?.and_then(|table| table.get_mut(name)).filter(|item| item.is_str());
        let Some(item) = item else {
            bail!("npm package `{name}` is not declared in [target.{kind}.{}]", npm_table_key(dev));
        };
        replace_str(item, version);
        Ok(())
    }

    /// The targets with a `[target.<name>]` table, in manifest order. Empty when the manifest has no
    /// `[target]` section, which makes it an implicit JVM project.
    pub fn targets(&self) -> Vec<TargetKind> {
        let Some(targets) = self.doc.get("target").and_then(Item::as_table_like) else {
            return Vec::new();
        };
        targets.iter().filter_map(|(key, _)| key.parse().ok()).collect()
    }

    /// Adds `[target.<kind>]` from `section`, a manifest fragment holding that table (as written by
    /// `kargo new`). Returns `false`, changing nothing, when the target is already declared.
    pub fn add_target(&mut self, kind: TargetKind, section: &str) -> Result<bool> {
        if self.targets().contains(&kind) {
            return Ok(false);
        }
        let fragment: DocumentMut = section.parse().context("invalid target section")?;
        let mut table = fragment
            .get("target")
            .and_then(|t| t.get(kind.as_str()))
            .and_then(Item::as_table)
            .with_context(|| format!("the fragment has no [target.{kind}] table"))?
            .clone();
        // No position: the table is written after the last table of `[target]`.
        table.set_position(None);
        *table.decor_mut() = Decor::default();
        let targets = self
            .doc
            .as_table_mut()
            .entry("target")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .context("`target` in the manifest is not a table")?;
        targets.insert(kind.as_str(), Item::Table(table));
        Ok(true)
    }

    /// Removes `[target.<kind>]` with its sub-tables (dependencies, cinterop). Returns `false`
    /// when the target is not declared.
    pub fn remove_target(&mut self, kind: TargetKind) -> Result<bool> {
        let Some(targets) = self.doc.get_mut("target").and_then(Item::as_table_like_mut) else {
            return Ok(false);
        };
        let removed = targets.remove(kind.as_str()).is_some();
        if targets.is_empty() {
            self.doc.as_table_mut().remove("target");
        }
        Ok(removed)
    }

    /// Adds `[target.native.cinterop.<name>]` with the Kotlin package of the bindings and, when given,
    /// its `pkg-config` package. Returns `false`, changing nothing, when the entry exists already.
    pub fn add_cinterop(&mut self, name: &str, package: &str, pkg_config: Option<&str>) -> Result<bool> {
        let native = self
            .doc
            .get_mut("target")
            .and_then(|t| t.get_mut("native"))
            .and_then(Item::as_table_mut)
            .context("kargo.toml has no [target.native]; add the native target first with `kargo target add native`")?;
        let cinterop = native
            .entry("cinterop")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .context("`cinterop` in [target.native] is not a table")?;
        if cinterop.contains_key(name) {
            return Ok(false);
        }
        let mut entry = Table::new();
        entry.insert("package", toml_edit::value(package));
        if let Some(pkg_config) = pkg_config {
            entry.insert("pkg-config", toml_edit::value(pkg_config));
        }
        cinterop.insert(name, Item::Table(entry));
        Ok(true)
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

    /// Removes a built-in framework from `[frameworks]`, whether it is written as `name = true`, an inline
    /// table or a `[frameworks.name]` table, and drops `[frameworks]` when nothing is left in it. Returns
    /// `false`, changing nothing, when the framework is not configured.
    pub fn remove_framework(&mut self, name: &str) -> Result<bool> {
        let Some(frameworks) = self.doc.get_mut("frameworks") else {
            return Ok(false);
        };
        let frameworks = frameworks.as_table_like_mut().context("`frameworks` in the manifest is not a table")?;
        if frameworks.remove(name).is_none() {
            return Ok(false);
        }
        if frameworks.is_empty() {
            self.doc.as_table_mut().remove("frameworks");
        }
        Ok(true)
    }

    /// Renames the Kotlin declarations that entry points name: `[target.jvm] main-class`,
    /// `[target.native] entry-point` and the `main` and `entry-point` of `[[bin]]`. A name is
    /// changed when it is `old` or inside it (`old.`, `old$`), so a renamed package or class takes
    /// the entries below it along. Returns how many entries changed.
    pub fn rename_entry_points(&mut self, old: &str, new: &str) -> usize {
        let mut changed = 0;
        let mut rename = |item: Option<&mut Item>| {
            let Some(item) = item else { return };
            let Some(current) = item.as_str() else { return };
            let rest = match current.strip_prefix(old) {
                Some("") => "",
                Some(rest) if rest.starts_with('.') || rest.starts_with('$') => rest,
                _ => return,
            };
            let renamed = format!("{new}{rest}");
            replace_str(item, &renamed);
            changed += 1;
        };
        if let Some(target) = self.doc.get_mut("target") {
            rename(target.get_mut("jvm").and_then(|t| t.get_mut("main-class")));
            rename(target.get_mut("native").and_then(|t| t.get_mut("entry-point")));
        }
        if let Some(bins) = self.doc.get_mut("bin").and_then(Item::as_array_of_tables_mut) {
            for bin in bins.iter_mut() {
                rename(bin.get_mut("main"));
                rename(bin.get_mut("entry-point"));
            }
        }
        changed
    }

    /// Points the `path` of the `[[bin]]` entries that name the file `old` (relative to the
    /// manifest, with `/`) at `new`. Returns how many entries changed.
    pub fn rename_bin_path(&mut self, old: &str, new: &str) -> usize {
        let mut changed = 0;
        if let Some(bins) = self.doc.get_mut("bin").and_then(Item::as_array_of_tables_mut) {
            for bin in bins.iter_mut() {
                if let Some(path) = bin.get_mut("path")
                    && path.as_str().is_some_and(|p| p.replace('\\', "/").trim_start_matches("./") == old)
                {
                    replace_str(path, new);
                    changed += 1;
                }
            }
        }
        changed
    }
}

/// Replaces a string value, keeping the whitespace and comments around it.
fn replace_str(item: &mut Item, new: &str) {
    if let Some(value) = item.as_value_mut() {
        let decor = value.decor().clone();
        *value = Value::from(new);
        *value.decor_mut() = decor;
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
    fn renames_entry_points() {
        let text = "[target.jvm]
main-class = \"com.a.MainKt\" # app

[target.native]
entry-point = \"com.a.main\"

[[bin]]
name = \"x\"
main = \"com.a.Tool$Inner\"

[[bin]]
name = \"y\"
path = \"./src/Y.kt\"
";
        let mut e = ManifestEditor::from_str(Path::new("kargo.toml"), text).unwrap();
        assert_eq!(e.rename_entry_points("com.a", "org.b"), 3);
        assert_eq!(e.rename_entry_points("org.b.Tool", "org.b.Cli"), 1);
        assert_eq!(e.rename_entry_points("org.bb", "z"), 0);
        assert_eq!(e.rename_bin_path("src/Y.kt", "src/Z.kt"), 1);
        let out = e.contents();
        assert!(out.contains("main-class = \"org.b.MainKt\" # app"), "{out}");
        assert!(out.contains("entry-point = \"org.b.main\""), "{out}");
        assert!(out.contains("main = \"org.b.Cli$Inner\""), "{out}");
        assert!(out.contains("path = \"src/Z.kt\""), "{out}");
    }

    #[test]
    fn adds_workspace_members_preserving_format() {
        let text = "# My workspace
[workspace]
members = [\"app\"]  # apps

[workspace.kotlin]
version = \"2.3.0\"
";
        let mut e = ManifestEditor::from_str(Path::new("kargo.toml"), text).unwrap();
        assert!(e.add_workspace_member("libs/core").unwrap());
        assert!(!e.add_workspace_member("app").unwrap());
        let out = e.contents();
        assert!(
            out.starts_with(
                "# My workspace
[workspace]
members = [\"app\", \"libs/core\"]  # apps
"
            ),
            "{out}"
        );
        assert!(out.contains("[workspace.kotlin]"));

        let mut e = ManifestEditor::from_str(
            Path::new("kargo.toml"),
            "[workspace]
",
        )
        .unwrap();
        e.add_workspace_member("app").unwrap();
        assert_eq!(
            e.contents(),
            "[workspace]
members = [\"app\"]
"
        );
    }

    #[test]
    fn adds_path_dependency() {
        let mut ed = editor();
        assert!(!ed.add_path_dependency(DependencySection::Main, "mathlib", r"..\mathlib", None).unwrap());
        assert!(ed.add_path_dependency(DependencySection::Dev, "fixtures", "../fixtures", Some(Scope::Runtime)).is_ok());
        let out = ed.contents();
        assert!(out.contains(r#"mathlib = { path = "../mathlib" }"#), "{out}");
        assert!(out.contains(r#"fixtures = { path = "../fixtures", scope = "runtime" }"#), "{out}");
        let m = crate::Manifest::parse(&out).unwrap();
        assert_eq!(m.path_dependencies.len(), 2);
        ed.remove_dependency(DependencySection::Main, "mathlib").unwrap();
        assert!(!ed.contents().contains("mathlib"));
    }

    #[test]
    fn adds_simple_dependency_preserving_format() {
        let mut ed = editor();
        let replaced = ed
            .add_dependency(
                DependencySection::Main,
                &"com.c:z".parse().unwrap(),
                &NewDependency { version: Some("3.0".into()), ..Default::default() },
            )
            .unwrap();
        assert!(!replaced);
        let out = ed.contents();
        assert!(out.contains("# keep this comment"));
        assert!(out.contains(r#""com.c:z" = "3.0""#), "{out}");
        crate::Manifest::parse(&out).unwrap();
    }

    #[test]
    fn updates_existing_and_alias_entries() {
        let mut ed = editor();
        assert!(
            ed.add_dependency(
                DependencySection::Main,
                &"com.a:x".parse().unwrap(),
                &NewDependency { version: Some("1.1".into()), ..Default::default() }
            )
            .unwrap()
        );
        assert!(
            ed.add_dependency(
                DependencySection::Main,
                &"com.b:y".parse().unwrap(),
                &NewDependency { version: Some("2.1".into()), scope: Some(Scope::Runtime), ..Default::default() }
            )
            .unwrap()
        );
        let out = ed.contents();
        assert!(out.contains(r#""com.a:x" = "1.1""#), "{out}");
        assert!(out.contains(r#"alias = { coords = "com.b:y", version = "2.1", scope = "runtime" }"#), "{out}");
        let m = crate::Manifest::parse(&out).unwrap();
        assert_eq!(m.dependencies.len(), 2);
    }

    #[test]
    fn adds_to_dev_and_target_sections() {
        let mut ed = editor();
        ed.add_dependency(
            DependencySection::Dev,
            &"t:t".parse().unwrap(),
            &NewDependency { version: Some("1".into()), ..Default::default() },
        )
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
        let m = crate::Manifest::parse(&out).unwrap();
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
    fn sets_versions_in_place() {
        let text = format!(
            "{BASE}coords-style = {{ coords = \"com.d:w:1.0:linux\" }}\nmanaged = {{ coords = \"com.e:v\" }}\n\n[dev-dependencies]\n\"com.t:t\" = \"1.0\"   # pinned\n\n[target.jvm.dependencies.table-style]\ngroup = \"com.f\"\nversion = \"1.0\"\nscope = \"runtime\"\n"
        );
        let mut ed = ManifestEditor::from_str(Path::new("kargo.toml"), &text).unwrap();
        let set = |ed: &mut ManifestEditor, section, m: &str, v: &str| ed.set_dependency_version(section, &m.parse().unwrap(), v);
        assert_eq!(set(&mut ed, DependencySection::Main, "com.a:x", "1.5").unwrap(), "com.a:x");
        assert_eq!(set(&mut ed, DependencySection::Main, "com.b:y", "2.5").unwrap(), "alias");
        set(&mut ed, DependencySection::Main, "com.d:w", "1.1").unwrap();
        set(&mut ed, DependencySection::Dev, "com.t:t", "1.1").unwrap();
        set(&mut ed, DependencySection::Target(TargetKind::Jvm), "com.f:table-style", "1.1").unwrap();
        assert!(set(&mut ed, DependencySection::Main, "com.e:v", "1.0").unwrap_err().to_string().contains("BOM"));
        assert!(set(&mut ed, DependencySection::Main, "com.z:z", "1.0").unwrap_err().to_string().contains("not declared"));
        let out = ed.contents();
        assert!(out.contains("name = \"demo\"   # keep this comment"), "{out}");
        assert!(out.contains("\"com.a:x\" = \"1.5\"\n"), "{out}");
        assert!(out.contains("alias = { coords = \"com.b:y\", version = \"2.5\" }"), "{out}");
        assert!(out.contains("coords-style = { coords = \"com.d:w:1.1:linux\" }"), "{out}");
        assert!(out.contains("\"com.t:t\" = \"1.1\"   # pinned"), "{out}");
        assert!(out.contains("group = \"com.f\"\nversion = \"1.1\"\nscope = \"runtime\""), "{out}");
        crate::Manifest::parse(&out).unwrap();
    }

    #[test]
    fn sets_npm_versions_in_place() {
        let text = format!(
            "{BASE}\n[target.wasm]\nnpm-dependencies = {{ \"left-pad\" = \"1.0.0\" }}\n\n[target.js.npm-dependencies]\n\"@js-joda/core\" = \"^3.2.0\"  # dates\n"
        );
        let mut ed = ManifestEditor::from_str(Path::new("kargo.toml"), &text).unwrap();
        ed.set_npm_dependency_version(TargetKind::Wasm, "left-pad", "1.3.0").unwrap();
        ed.set_npm_dependency_version(TargetKind::Js, "@js-joda/core", "^3.2.1").unwrap();
        assert!(ed.set_npm_dependency_version(TargetKind::Js, "nope", "1.0.0").unwrap_err().to_string().contains("not declared"));
        let out = ed.contents();
        assert!(out.contains("npm-dependencies = { \"left-pad\" = \"1.3.0\" }"), "{out}");
        assert!(out.contains("\"@js-joda/core\" = \"^3.2.1\"  # dates"), "{out}");
    }

    #[test]
    fn adds_and_removes_npm_packages() {
        let text = format!(
            "{BASE}\n[target.wasm]\nenvironment = \"browser\"  # web\nnpm-dependencies = {{ \"left-pad\" = \"^1.0.0\" }}\n\n[target.js]\n"
        );
        let mut ed = ManifestEditor::from_str(Path::new("kargo.toml"), &text).unwrap();
        // An inline table is extended in place; a missing one is created, with the scoped name quoted.
        assert_eq!(ed.add_npm_dependency(TargetKind::Wasm, false, "chart.js", "^4.4.0").unwrap(), None);
        assert_eq!(ed.add_npm_dependency(TargetKind::Wasm, false, "left-pad", "^1.3.0").unwrap().as_deref(), Some("^1.0.0"));
        assert_eq!(ed.add_npm_dependency(TargetKind::Js, false, "@js-joda/core", "^3.2.0").unwrap(), None);
        assert_eq!(ed.add_npm_dependency(TargetKind::Js, true, "mocha", "^10.0.0").unwrap(), None);
        let out = ed.contents();
        assert!(out.contains("environment = \"browser\"  # web"), "{out}");
        assert!(out.contains("\"left-pad\" = \"^1.3.0\""), "{out}");
        assert!(out.contains("\"chart.js\" = \"^4.4.0\""), "a dot in a name is quoted: {out}");
        assert!(out.contains("[target.js.npm-dependencies]\n\"@js-joda/core\" = \"^3.2.0\""), "{out}");
        assert!(out.contains("[target.js.dev-npm-dependencies]\nmocha = \"^10.0.0\""), "{out}");
        let manifest = crate::Manifest::parse(&out).unwrap();
        assert_eq!(manifest.targets.js.unwrap().dev_npm_dependencies["mocha"], "^10.0.0");

        assert_eq!(ed.remove_npm_dependency(TargetKind::Js, true, "mocha").unwrap(), "^10.0.0");
        assert_eq!(ed.remove_npm_dependency(TargetKind::Wasm, false, "chart.js").unwrap(), "^4.4.0");
        assert!(ed.remove_npm_dependency(TargetKind::Wasm, false, "nope").unwrap_err().to_string().contains("not declared"));
        assert!(ed.remove_npm_dependency(TargetKind::Wasm, true, "left-pad").is_err());
        let out = ed.contents();
        assert!(!out.contains("mocha") && !out.contains("dev-npm-dependencies"), "an empty table goes away: {out}");
        assert!(out.contains("[target.js.npm-dependencies]"), "{out}");
        crate::Manifest::parse(&out).unwrap();

        let mut no_target = editor();
        let err = no_target.add_npm_dependency(TargetKind::Wasm, false, "left-pad", "^1.0.0").unwrap_err().to_string();
        assert!(err.contains("[target.wasm] is not declared"), "{err}");
    }

    #[test]
    fn enables_frameworks_once() {
        let mut ed = editor();
        assert!(ed.enable_framework("serialization").unwrap());
        assert!(!ed.enable_framework("serialization").unwrap());
        let m = crate::Manifest::parse(&ed.contents()).unwrap();
        assert!(m.frameworks.serialization.is_some());
    }

    #[test]
    fn removes_frameworks_in_every_form() {
        let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[frameworks]\nserialization = true\ncoroutines = { }\n\n[frameworks.compose]\nhtml = true\n";
        let mut ed = ManifestEditor::from_str(Path::new("kargo.toml"), text).unwrap();
        assert!(ed.remove_framework("serialization").unwrap());
        assert!(!ed.remove_framework("serialization").unwrap());
        assert!(ed.remove_framework("compose").unwrap());
        assert!(!ed.contents().contains("serialization") && !ed.contents().contains("compose"), "{}", ed.contents());
        assert!(ed.contents().contains("[frameworks]"), "{}", ed.contents());
        assert!(ed.remove_framework("coroutines").unwrap());
        assert!(!ed.contents().contains("frameworks"), "{}", ed.contents());
        assert!(!editor().remove_framework("lombok").unwrap(), "no [frameworks] table at all");
    }

    #[test]
    fn adds_and_removes_targets_preserving_format() {
        let text = "# My project\n[package]\nname = \"demo\"   # keep this comment\nversion = \"0.1.0\"\n\n[target.jvm]\n\n[target.jvm.dependencies]\n\"com.a:x\" = \"1.0\"\n\n[dependencies]\n\"com.b:y\" = \"2.0\"\n";
        let mut ed = ManifestEditor::from_str(Path::new("kargo.toml"), text).unwrap();
        assert_eq!(ed.targets(), [TargetKind::Jvm]);
        assert!(ed.add_target(TargetKind::Native, "[target.native]\nplatforms = [\"host\"]\nkind = \"executable\"\n").unwrap());
        assert!(!ed.add_target(TargetKind::Native, "[target.native]\n").unwrap());
        let out = ed.contents();
        assert!(out.starts_with(text.split("[dependencies]").next().unwrap()), "{out}");
        assert!(
            out.contains("\"com.a:x\" = \"1.0\"\n\n[target.native]\nplatforms = [\"host\"]\nkind = \"executable\"\n\n[dependencies]"),
            "new targets follow the existing ones: {out}"
        );
        let m = crate::Manifest::parse(&out).unwrap();
        assert_eq!(m.targets.enabled(), [TargetKind::Jvm, TargetKind::Native]);

        assert!(ed.remove_target(TargetKind::Jvm).unwrap());
        assert!(!ed.remove_target(TargetKind::Jvm).unwrap());
        let out = ed.contents();
        assert!(!out.contains("com.a:x"), "{out}");
        assert_eq!(ed.targets(), [TargetKind::Native]);
        assert!(ed.remove_target(TargetKind::Native).unwrap());
        assert!(!ed.contents().contains("target"), "{}", ed.contents());
    }

    #[test]
    fn adds_a_target_to_an_implicit_jvm_manifest() {
        let mut ed = editor();
        assert!(ed.targets().is_empty());
        ed.add_target(TargetKind::Jvm, "[target.jvm]\n").unwrap();
        ed.add_target(TargetKind::Wasm, "[target.wasm]\nenvironment = \"js\"\n").unwrap();
        let out = ed.contents();
        assert!(out.starts_with(BASE), "{out}");
        assert!(out.ends_with("\n[target.jvm]\n\n[target.wasm]\nenvironment = \"js\"\n"), "{out}");
        let m = crate::Manifest::parse(&out).unwrap();
        assert_eq!(m.targets.enabled(), [TargetKind::Jvm, TargetKind::Wasm]);
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
        let m = crate::Manifest::parse(&ed.contents()).unwrap();
        assert!(m.dependencies.iter().any(|d| d.bom));
    }

    #[test]
    fn adds_cinterop_entries() {
        let mut ed = editor();
        let err = ed.add_cinterop("sqlite", "sqlite", None).unwrap_err().to_string();
        assert!(err.contains("kargo target add native"), "{err}");
        ed.add_target(TargetKind::Native, "[target.native]\nplatforms = [\"host\"]\n").unwrap();
        assert!(ed.add_cinterop("sqlite", "sqlite", Some("sqlite3")).unwrap());
        assert!(ed.add_cinterop("my-lib", "my_lib", None).unwrap());
        assert!(!ed.add_cinterop("sqlite", "other", None).unwrap());
        let out = ed.contents();
        assert!(
            out.ends_with(
                "[target.native]\nplatforms = [\"host\"]\n\n[target.native.cinterop.sqlite]\npackage = \"sqlite\"\npkg-config = \"sqlite3\"\n\n[target.native.cinterop.my-lib]\npackage = \"my_lib\"\n"
            ),
            "{out}"
        );
        let m = crate::Manifest::parse(&out).unwrap();
        assert_eq!(m.targets.native.unwrap().cinterop.len(), 2);
    }
}
