//! What `kargo import` learns about a project, independent of the build tool it comes from, and
//! how that is written as a `kargo.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use kargo_manifest::TargetKind;

/// A TOML value in the small subset the importer writes.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Inline(Vec<(String, Value)>),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Self {
        Value::Str(s.into())
    }

    pub fn strings<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Self {
        Value::Array(items.into_iter().map(|s| Value::Str(s.into())).collect())
    }

    fn render(&self) -> String {
        match self {
            Value::Str(s) => quote(s),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Array(items) => format!("[{}]", items.iter().map(Value::render).collect::<Vec<_>>().join(", ")),
            Value::Inline(rows) if rows.is_empty() => "{}".to_owned(),
            Value::Inline(rows) => {
                format!("{{ {} }}", rows.iter().map(|(k, v)| format!("{} = {}", key(k), v.render())).collect::<Vec<_>>().join(", "))
            }
        }
    }
}

/// A basic TOML string.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A TOML key, quoted unless it is made of bare-key characters.
fn key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { k.to_owned() } else { quote(k) }
}

/// One `[header]` table of a manifest.
#[derive(Debug, Clone)]
pub struct Section {
    pub header: String,
    pub rows: Vec<(String, Value)>,
    /// Write the header even without rows (`[target.jvm]` enables a target).
    pub keep_empty: bool,
}

impl Section {
    pub fn new(header: impl Into<String>) -> Self {
        Self { header: header.into(), rows: Vec::new(), keep_empty: false }
    }

    pub fn kept(header: impl Into<String>) -> Self {
        Self { keep_empty: true, ..Self::new(header) }
    }

    pub fn row(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.rows.push((key.into(), value));
        self
    }

    pub fn row_if(&mut self, key: impl Into<String>, value: Option<String>) -> &mut Self {
        if let Some(value) = value {
            self.rows.push((key.into(), Value::Str(value)));
        }
        self
    }
}

/// Writes sections one after the other with a blank line between them.
pub fn render(sections: &[Section]) -> String {
    let mut out = String::new();
    for section in sections.iter().filter(|s| s.keep_empty || !s.rows.is_empty()) {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(out, "[{}]", section.header);
        for (k, v) in &section.rows {
            let _ = writeln!(out, "{} = {}", key(k), v.render());
        }
    }
    out
}

/// Where a dependency is on the classpath, as `scope` in `kargo.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Compile,
    Runtime,
    Provided,
    Processor,
    Ksp,
}

impl Scope {
    fn name(self) -> Option<&'static str> {
        match self {
            Scope::Compile => None,
            Scope::Runtime => Some("runtime"),
            Scope::Provided => Some("provided"),
            Scope::Processor => Some("processor"),
            Scope::Ksp => Some("ksp"),
        }
    }
}

/// A dependency, a library from a Maven repository or a package of the same project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dependency {
    pub group: String,
    pub artifact: String,
    pub version: Option<String>,
    pub scope: Scope,
    pub classifier: Option<String>,
    pub extension: Option<String>,
    pub exclude: Vec<String>,
    pub bom: bool,
    /// `transitive = false`: leave out the dependency's own dependencies.
    pub non_transitive: bool,
    /// Part of the package's API (Maven's compile scope, Gradle's `api`).
    pub exported: bool,
    /// The package (its name and its directory relative to the import root) this depends on.
    pub path: Option<LocalPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPackage {
    pub name: String,
    pub dir: PathBuf,
}

impl Dependency {
    pub fn new(group: impl Into<String>, artifact: impl Into<String>, version: Option<String>) -> Self {
        Self { group: group.into(), artifact: artifact.into(), version, ..Self::default() }
    }

    pub fn coords(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }

    /// The identity of a dependency within one table: the same module with the same classifier and extension.
    fn identity(&self) -> (String, String, Option<String>, Option<String>) {
        (self.group.clone(), self.artifact.clone(), self.classifier.clone(), self.extension.clone())
    }
}

/// A `[repositories]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub url: String,
}

/// A compiler plugin of `[plugins.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerPlugin {
    pub name: String,
    pub maven: String,
    pub id: String,
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frameworks {
    /// `Some(version)` (or `Some(None)` for the default) enables kotlinx.serialization.
    pub serialization: Option<Serialization>,
    pub coroutines: Option<Option<String>>,
    pub compose: Option<Option<String>>,
    pub lombok: Option<Option<String>>,
    pub ktor: Option<Ktor>,
    pub spring_boot: Option<SpringBoot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Serialization {
    pub version: Option<String>,
    pub formats: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ktor {
    pub version: Option<String>,
    pub server: Option<String>,
    pub client: Option<String>,
    pub server_plugins: Vec<String>,
    pub client_plugins: Vec<String>,
    pub json: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpringBoot {
    pub version: Option<String>,
    pub starters: Vec<String>,
    pub jpa: bool,
}

/// `[kotlin]` settings that differ from Kargo's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KotlinSettings {
    pub version: Option<String>,
    pub language_version: Option<String>,
    pub api_version: Option<String>,
    pub ksp: Option<String>,
    pub opt_in: Vec<String>,
    pub compiler_args: Vec<String>,
    pub warnings_as_errors: bool,
}

/// How one target is configured, as the rows of its `[target.<name>]` table.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSettings {
    pub kind: TargetKind,
    pub rows: Vec<(String, Value)>,
}

impl TargetSettings {
    pub fn new(kind: TargetKind) -> Self {
        Self { kind, rows: Vec::new() }
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.rows.retain(|(k, _)| k != key);
        self.rows.push((key.to_owned(), value));
    }
}

/// A source directory that moves into Kargo's layout: its contents merge into `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// One Kargo package.
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// The package's directory relative to the project root (empty for the root).
    pub dir: PathBuf,
    pub name: String,
    pub version: String,
    pub group: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub authors: Vec<String>,
    pub kotlin: KotlinSettings,
    pub jvm_target: Option<String>,
    pub jdk: Option<String>,
    pub main_class: Option<String>,
    pub javac_args: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    pub target_dependencies: BTreeMap<TargetKind, Vec<Dependency>>,
    /// npm packages of the Wasm target (`[target.wasm.npm-dependencies]`).
    pub npm_dependencies: Vec<(String, String)>,
    pub repositories: Vec<Repository>,
    pub frameworks: Frameworks,
    pub plugins: Vec<CompilerPlugin>,
    /// Empty for a plain JVM project.
    pub targets: Vec<TargetSettings>,
    pub moves: Vec<DirMove>,
}

impl Package {
    /// The `kargo.toml` of this package.
    pub fn manifest(&self, workspace_kotlin: Option<&str>) -> String {
        let mut sections = Vec::new();

        let mut package = Section::new("package");
        package.row("name", Value::str(&self.name)).row("version", Value::str(&self.version));
        package
            .row_if("group", self.group.clone())
            .row_if("description", self.description.clone())
            .row_if("license", self.license.clone())
            .row_if("homepage", self.homepage.clone())
            .row_if("repository", self.repository.clone());
        if !self.authors.is_empty() {
            package.row("authors", Value::strings(&self.authors));
        }
        sections.push(package);

        let k = &self.kotlin;
        let mut kotlin = Section::new("kotlin");
        // The project keeps the Kotlin version it builds with; a member leaves out the one `[workspace.kotlin]` sets.
        let version = k.version.as_deref().filter(|v| Some(*v) != workspace_kotlin);
        kotlin
            .row_if("version", version.map(str::to_owned))
            .row_if("language-version", k.language_version.clone())
            .row_if("api-version", k.api_version.clone())
            .row_if("ksp", k.ksp.clone());
        if !k.opt_in.is_empty() {
            kotlin.row("opt-in", Value::strings(&k.opt_in));
        }
        if !k.compiler_args.is_empty() {
            kotlin.row("compiler-args", Value::strings(&k.compiler_args));
        }
        if k.warnings_as_errors {
            kotlin.row("warnings-as-errors", Value::Bool(true));
        }
        sections.push(kotlin);

        if let Some(jdk) = &self.jdk {
            let mut toolchain = Section::new("toolchain");
            toolchain.row("jdk", Value::str(jdk));
            sections.push(toolchain);
        }

        // No `[target.*]` at all means a plain JVM project.
        let mut targets = self.targets.clone();
        let jvm_settings = self.jvm_target.is_some() || self.main_class.is_some() || !self.javac_args.is_empty();
        if jvm_settings && targets.is_empty() {
            targets.push(TargetSettings::new(TargetKind::Jvm));
        }
        for target in &targets {
            let mut section = Section::kept(format!("target.{}", target.kind.as_str()));
            if target.kind == TargetKind::Jvm {
                section.row_if("jvm-target", self.jvm_target.clone()).row_if("main-class", self.main_class.clone());
                if !self.javac_args.is_empty() {
                    section.row("javac-args", Value::strings(&self.javac_args));
                }
            }
            section.rows.extend(target.rows.iter().cloned());
            sections.push(section);
        }

        sections.push(dependency_section("dependencies", &self.dependencies));
        sections.push(dependency_section("dev-dependencies", &self.dev_dependencies));
        for (target, deps) in &self.target_dependencies {
            sections.push(dependency_section(&format!("target.{}.dependencies", target.as_str()), deps));
        }

        let mut npm = Section::new("target.wasm.npm-dependencies");
        for (name, version) in &self.npm_dependencies {
            npm.row(name, Value::str(version));
        }
        sections.push(npm);

        let mut repositories = Section::new("repositories");
        for repo in &self.repositories {
            repositories.row(&repo.name, Value::str(&repo.url));
        }
        sections.push(repositories);

        sections.push(self.frameworks.section());

        for plugin in &self.plugins {
            let mut section = Section::new(format!("plugins.{}", key(&plugin.name)));
            section.row("maven", Value::str(&plugin.maven)).row("id", Value::str(&plugin.id));
            if !plugin.options.is_empty() {
                section.row("options", Value::Inline(plugin.options.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect()));
            }
            sections.push(section);
        }
        render(&sections)
    }
}

impl Frameworks {
    fn section(&self) -> Section {
        let mut s = Section::new("frameworks");
        let version_or_true = |version: &Option<String>| match version {
            Some(v) => Value::Inline(vec![("version".to_owned(), Value::str(v))]),
            None => Value::Bool(true),
        };
        if let Some(ser) = &self.serialization {
            let only_json = ser.formats.is_empty() || ser.formats == ["json"];
            let value = if only_json {
                version_or_true(&ser.version)
            } else {
                let mut rows = Vec::new();
                if let Some(v) = &ser.version {
                    rows.push(("version".to_owned(), Value::str(v)));
                }
                rows.push(("formats".to_owned(), Value::strings(&ser.formats)));
                Value::Inline(rows)
            };
            s.row("serialization", value);
        }
        if let Some(version) = &self.coroutines {
            s.row(
                "coroutines",
                match version {
                    Some(v) => Value::str(v),
                    None => Value::Bool(true),
                },
            );
        }
        if let Some(version) = &self.compose {
            s.row("compose", version_or_true(version));
        }
        if let Some(ktor) = &self.ktor {
            let mut rows = Vec::new();
            if let Some(v) = &ktor.version {
                rows.push(("version".to_owned(), Value::str(v)));
            }
            if let Some(server) = &ktor.server {
                rows.push(("server".to_owned(), Value::str(server)));
            }
            if let Some(client) = &ktor.client {
                rows.push(("client".to_owned(), Value::str(client)));
            }
            if !ktor.server_plugins.is_empty() {
                rows.push(("server-plugins".to_owned(), Value::strings(&ktor.server_plugins)));
            }
            if !ktor.client_plugins.is_empty() {
                rows.push(("client-plugins".to_owned(), Value::strings(&ktor.client_plugins)));
            }
            rows.push(("json".to_owned(), Value::Bool(ktor.json)));
            s.row("ktor", Value::Inline(rows));
        }
        if let Some(spring) = &self.spring_boot {
            let mut rows = Vec::new();
            if let Some(v) = &spring.version {
                rows.push(("version".to_owned(), Value::str(v)));
            }
            rows.push(("starters".to_owned(), Value::strings(&spring.starters)));
            if spring.jpa {
                rows.push(("jpa".to_owned(), Value::Bool(true)));
            }
            s.row("spring-boot", Value::Inline(rows));
        }
        if let Some(version) = &self.lombok {
            s.row("lombok", version_or_true(version));
        }
        s
    }
}

/// The `[dependencies]`-like table for `deps`, one row per module (and classifier).
fn dependency_section(header: &str, deps: &[Dependency]) -> Section {
    let mut section = Section::new(header);
    let mut seen = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for dep in deps {
        if !seen.insert(dep.identity()) && dep.path.is_none() {
            continue;
        }
        let (name, value) = dependency_row(dep, &keys);
        if !keys.insert(name.clone()) {
            continue;
        }
        section.row(name, value);
    }
    section
}

fn dependency_row(dep: &Dependency, taken: &BTreeSet<String>) -> (String, Value) {
    if let Some(local) = &dep.path {
        let mut rows = vec![("path".to_owned(), Value::str(relative_path(&local.dir)))];
        if let Some(scope) = dep.scope.name() {
            rows.push(("scope".to_owned(), Value::str(scope)));
        }
        if dep.exported {
            rows.push(("exported".to_owned(), Value::Bool(true)));
        }
        return (local.name.clone(), Value::Inline(rows));
    }
    let separate = dep.classifier.is_some() || dep.extension.is_some();
    let mut rows = Vec::new();
    let name = if separate {
        // The key of a module declared with a classifier or extension must differ from the plain one.
        let mut name = dep.artifact.clone();
        for part in [&dep.classifier, &dep.extension].into_iter().flatten() {
            name = format!("{name}-{part}");
        }
        let mut candidate = name.clone();
        let mut n = 2;
        while taken.contains(&candidate) {
            candidate = format!("{name}-{n}");
            n += 1;
        }
        rows.push(("group".to_owned(), Value::str(&dep.group)));
        rows.push(("artifact".to_owned(), Value::str(&dep.artifact)));
        candidate
    } else {
        dep.coords()
    };
    if let Some(v) = &dep.version {
        rows.push(("version".to_owned(), Value::str(v)));
    }
    if let Some(scope) = dep.scope.name() {
        rows.push(("scope".to_owned(), Value::str(scope)));
    }
    if let Some(c) = &dep.classifier {
        rows.push(("classifier".to_owned(), Value::str(c)));
    }
    if let Some(e) = &dep.extension {
        rows.push(("extension".to_owned(), Value::str(e)));
    }
    if !dep.exclude.is_empty() {
        rows.push(("exclude".to_owned(), Value::strings(&dep.exclude)));
    }
    if dep.non_transitive {
        rows.push(("transitive".to_owned(), Value::Bool(false)));
    }
    if dep.bom {
        rows.push(("bom".to_owned(), Value::Bool(true)));
    }
    if dep.exported {
        rows.push(("exported".to_owned(), Value::Bool(true)));
    }
    // `"g:a" = "1.0"` when the version is all there is.
    if !separate && rows.len() == 1 && rows[0].0 == "version" {
        return (name, rows.remove(0).1);
    }
    (name, Value::Inline(rows))
}

/// A path as `kargo.toml` writes it: forward slashes, `.` for the current directory.
pub fn relative_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.is_empty() { ".".to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_strings_keys_and_tables() {
        let mut s = Section::new("dependencies");
        s.row("a:b", Value::str("1.0"));
        s.row("with space", Value::Inline(vec![("version".into(), Value::str("a\"b\\c")), ("exclude".into(), Value::strings(["x:y"]))]));
        s.row("empty", Value::Inline(vec![]));
        let text = render(&[s, Section::new("empty"), Section::kept("target.jvm")]);
        assert_eq!(
            text,
            "[dependencies]\n\"a:b\" = \"1.0\"\n\"with space\" = { version = \"a\\\"b\\\\c\", exclude = [\"x:y\"] }\nempty = {}\n\n[target.jvm]\n"
        );
        toml::from_str::<toml::Table>(&text).unwrap();
    }

    #[test]
    fn manifest_has_one_row_per_module_and_a_key_for_each_classifier() {
        let mut package = Package { name: "demo".into(), version: "1.0.0".into(), ..Package::default() };
        package.dependencies.push(Dependency::new("g", "a", Some("1".into())));
        package.dependencies.push(Dependency::new("g", "a", Some("2".into())));
        let mut natives = Dependency::new("org.lwjgl", "lwjgl", Some("3.3.3".into()));
        natives.classifier = Some("natives-linux".into());
        package.dependencies.push(natives);
        package.dependencies.push(Dependency::new("org.lwjgl", "lwjgl", Some("3.3.3".into())));
        let text = package.manifest(None);
        assert!(text.contains("\"g:a\" = \"1\""), "{text}");
        assert!(!text.contains("\"2\""), "{text}");
        assert!(
            text.contains(
                "lwjgl-natives-linux = { group = \"org.lwjgl\", artifact = \"lwjgl\", version = \"3.3.3\", classifier = \"natives-linux\" }"
            ),
            "{text}"
        );
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(table["dependencies"].as_table().unwrap().len(), 3);
    }
}
