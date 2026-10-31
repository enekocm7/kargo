//! The `package.json` of a Wasm or JS library (`kind = "library"`): the compiled module, its
//! TypeScript declarations, the npm packages it imports and, from `[package]` and `[publish.npm]`,
//! the fields registries and tools show (`repository`, `homepage`, `bugs`, `keywords`, `sideEffects`,
//! `engines`). `kargo publish --npm` publishes the directory this file is in.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use kargo_manifest::{LibraryFormat, Manifest, NpmPublishSettings, NpmSideEffects, Package, WasmKind};

use crate::workspace::{Project, Workspace};

/// What a library package holds and how it is laid out.
pub struct LibraryPackage<'a> {
    /// The npm package name, scoped by `[publish.npm] scope` (see [`scoped_name`]).
    pub name: String,
    pub package: &'a Package,
    pub settings: Option<&'a NpmPublishSettings>,
    /// The module's file name without its extension.
    pub module: &'a str,
    /// The extension of the compiled module: `mjs` or `js`.
    pub extension: &'a str,
    /// The extension of its declarations: `d.mts` or `d.ts`.
    pub types_extension: &'a str,
    /// Whether the module is an ES module, as opposed to CommonJS or UMD.
    pub es: bool,
    /// The formats of a library bundle, when it has one: `<module>.mjs`, `<module>.cjs` and `<module>.umd.js` are written
    /// as the formats say, and `extension` and `es` are not used.
    pub formats: Option<&'a [LibraryFormat]>,
    /// The `files` patterns: what `npm publish` puts in the tarball.
    pub files: Vec<String>,
    /// The npm packages the module imports.
    pub dependencies: &'a BTreeMap<String, String>,
}

impl LibraryPackage<'_> {
    /// The `package.json` text.
    ///
    /// Written by hand because the order matters: TypeScript takes the first condition of `exports`
    /// that matches, so `types` has to come before `import` and `default`.
    pub fn json(&self) -> String {
        let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
        // A JSON value, indented to sit one level down.
        let nested =
            |value: serde_json::Value| serde_json::to_string_pretty(&value).expect("a JSON value serializes").replace('\n', "\n  ");
        let package = self.package;
        let entry = quoted(&format!("./{}.{}", self.module, self.extension));
        let types = quoted(&format!("./{}.{}", self.module, self.types_extension));
        let settings = self.settings;

        let mut fields: Vec<(&str, String)> = vec![("name", quoted(&self.name)), ("version", quoted(&package.version))];
        if let Some(description) = &package.description {
            fields.push(("description", quoted(description)));
        }
        if let Some(keywords) = settings.map(|s| &s.keywords).filter(|k| !k.is_empty()) {
            fields.push(("keywords", nested(serde_json::json!(keywords))));
        }
        if let Some(homepage) = &package.homepage {
            fields.push(("homepage", quoted(homepage)));
        }
        if let Some(url) = bugs_url(package, settings) {
            fields.push(("bugs", nested(serde_json::json!({ "url": url }))));
        }
        if let Some(license) = &package.license {
            fields.push(("license", quoted(license)));
        }
        if let Some(author) = package.authors.first() {
            fields.push(("author", quoted(author)));
        }
        if package.authors.len() > 1 {
            fields.push(("contributors", nested(serde_json::json!(&package.authors[1..]))));
        }
        if let Some(repository) = &package.repository {
            fields.push(("repository", nested(serde_json::json!({ "type": "git", "url": git_url(repository) }))));
        }
        match self.formats.filter(|formats| *formats != [LibraryFormat::Esm]) {
            Some(formats) => fields.extend(self.bundle_fields(formats)),
            None => {
                let module_type = if self.es { "module" } else { "commonjs" };
                fields.extend([("type", quoted(module_type)), ("main", entry.clone())]);
                if self.es {
                    fields.push(("module", entry.clone()));
                }
                let condition = if self.es { "import" } else { "require" };
                fields.extend([
                    ("types", types.clone()),
                    (
                        "exports",
                        format!(
                            "{{\n    \".\": {{\n      \"types\": {types},\n      \"{condition}\": {entry},\n      \"default\": {entry}\n    }},\n    \"./package.json\": \"./package.json\"\n  }}"
                        ),
                    ),
                ]);
            }
        }
        match settings.and_then(|s| s.side_effects.as_ref()) {
            Some(NpmSideEffects::Flag(flag)) => fields.push(("sideEffects", flag.to_string())),
            Some(NpmSideEffects::Files(files)) => fields.push(("sideEffects", nested(serde_json::json!(files)))),
            None => {}
        }
        fields.push(("files", nested(serde_json::json!(self.files))));
        if let Some(engines) = settings.map(|s| &s.engines).filter(|e| !e.is_empty()) {
            fields.push(("engines", nested(serde_json::json!(engines))));
        }
        if !self.dependencies.is_empty() {
            fields.push(("dependencies", nested(serde_json::json!(self.dependencies))));
        }
        let body: Vec<String> = fields.iter().map(|(key, value)| format!("  \"{key}\": {value}")).collect();
        format!("{{\n{}\n}}\n", body.join(",\n"))
    }
}

impl LibraryPackage<'_> {
    /// `main`, `module`, `types`, `unpkg`, `jsdelivr` and `exports` of a library bundled in more than the ES module format:
    /// `import` loads the ES module and `require` the CommonJS one, each with the declarations for its kind of module, and the
    /// UMD file is for `<script>` tags (through unpkg and jsDelivr).
    fn bundle_fields(&self, formats: &[LibraryFormat]) -> Vec<(&'static str, String)> {
        let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
        let file = |format: LibraryFormat| formats.contains(&format).then(|| quoted(&format!("./{}.{}", self.module, format.extension())));
        let (esm, cjs, umd) = (file(LibraryFormat::Esm), file(LibraryFormat::Cjs), file(LibraryFormat::Umd));
        let types_esm = quoted(&format!("./{}.d.mts", self.module));
        let types_cjs = quoted(&format!("./{}.d.cts", self.module));
        let main = cjs.clone().or_else(|| esm.clone()).or_else(|| umd.clone()).expect("a library has a format");
        let mut fields = vec![("main", main.clone())];
        if let Some(esm) = &esm {
            fields.push(("module", esm.clone()));
        }
        fields.push(("types", if esm.is_some() { types_esm.clone() } else { types_cjs.clone() }));
        if let Some(umd) = &umd {
            fields.push(("unpkg", umd.clone()));
            fields.push(("jsdelivr", umd.clone()));
        }
        let mut conditions = Vec::new();
        if let Some(esm) = &esm {
            conditions.push(format!("      \"import\": {{\n        \"types\": {types_esm},\n        \"default\": {esm}\n      }}"));
        }
        if let Some(cjs) = &cjs {
            conditions.push(format!("      \"require\": {{\n        \"types\": {types_cjs},\n        \"default\": {cjs}\n      }}"));
        }
        conditions.push(format!("      \"default\": {main}"));
        fields.push((
            "exports",
            format!("{{\n    \".\": {{\n{}\n    }},\n    \"./package.json\": \"./package.json\"\n  }}", conditions.join(",\n")),
        ));
        fields
    }
}

/// The `files` patterns for what a bundle wrote into `dir`: the modules of each format, the Wasm module, the declarations
/// and the source maps, as far as there are any.
pub fn bundled_files(dir: &Path) -> Vec<String> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ["*.mjs", "*.cjs", "*.umd.js", "*.wasm", "*.d.mts", "*.d.cts", "*.map"]
        .into_iter()
        .filter(|pattern| {
            let suffix = &pattern[1..];
            names.iter().any(|n| n.ends_with(suffix))
        })
        .map(str::to_owned)
        .collect()
}

/// The name a library is published under: `name`, or `@<scope>/<name>` when `[publish.npm]` has a
/// `scope` and `name` has none of its own.
pub fn scoped_name(name: String, settings: Option<&NpmPublishSettings>) -> String {
    match settings.and_then(|s| s.scope.as_deref()) {
        Some(scope) if !name.starts_with('@') => format!("@{scope}/{name}"),
        _ => name,
    }
}

/// The npm packages `manifest` publishes (a Wasm and a Kotlin/JS library each make one), by name.
pub fn package_names(manifest: &Manifest) -> Vec<String> {
    let settings = manifest.publish.npm.as_ref();
    let wasm = manifest.targets.wasm.as_ref().filter(|w| w.kind == WasmKind::Library).map(|w| w.npm_package_name(&manifest.package));
    let js = manifest.targets.js.as_ref().filter(|j| j.kind == WasmKind::Library).map(|j| j.npm_package_name(&manifest.package));
    wasm.into_iter().chain(js).map(|name| scoped_name(name, settings)).collect()
}

/// The npm dependencies of `ws`'s package, with the ones that refer to another package of the project at
/// that package's version, which is what the registry can install. A reference is a `workspace:` version
/// (`workspace:*` is the exact version, `workspace:^` and `workspace:~` keep their prefix) or a `file:` or
/// `link:` path into the other package's directory.
pub fn published_dependencies(project: &Project, ws: &Workspace, dependencies: BTreeMap<String, String>) -> BTreeMap<String, String> {
    dependencies
        .into_iter()
        .map(|(name, spec)| {
            let rewritten = member_reference(project, ws, &name, &spec).map(|index| {
                let version = &project.package(index).manifest().package.version;
                match spec.strip_prefix("workspace:") {
                    Some("^") => format!("^{version}"),
                    Some("~") => format!("~{version}"),
                    _ => version.clone(),
                }
            });
            (name, rewritten.unwrap_or(spec))
        })
        .collect()
}

/// The packages of `project` that `index`'s npm dependencies refer to (see [`published_dependencies`]).
pub fn member_dependencies(project: &Project, index: usize) -> Vec<usize> {
    let ws = project.package(index);
    let manifest = ws.manifest();
    let tables = manifest.targets.wasm.iter().map(|w| &w.npm_dependencies).chain(manifest.targets.js.iter().map(|j| &j.npm_dependencies));
    let mut out: Vec<usize> =
        tables.flat_map(|deps| deps.iter()).filter_map(|(name, spec)| member_reference(project, ws, name, spec)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The package of `project` (not `ws` itself) that the npm dependency `name` at `spec` refers to.
fn member_reference(project: &Project, ws: &Workspace, name: &str, spec: &str) -> Option<usize> {
    let others = || (0..project.packages().len()).filter(|&i| !std::ptr::eq(project.package(i), ws));
    if spec.starts_with("workspace:") {
        return others().find(|&i| package_names(project.package(i).manifest()).iter().any(|n| n == name));
    }
    let path = spec.strip_prefix("file:").or_else(|| spec.strip_prefix("link:"))?;
    let target = normalize(&ws.root().join(path));
    others().find(|&i| target.starts_with(normalize(project.package(i).root())))
}

/// `path` with `.` and `..` resolved without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The `bugs` URL: `[publish.npm] bugs`, else the issues page of a GitHub or GitLab repository.
fn bugs_url(package: &Package, settings: Option<&NpmPublishSettings>) -> Option<String> {
    if let Some(url) = settings.and_then(|s| s.bugs.clone()) {
        return Some(url);
    }
    let repository = package.repository.as_deref()?.trim_end_matches('/').trim_end_matches(".git");
    let host = repository.split_once("://")?.1.split('/').next()?;
    (host == "github.com" || host == "gitlab.com").then(|| format!("{repository}/issues"))
}

/// The URL of a `repository` field: a web address becomes the `git+https://...git` form npm writes itself.
fn git_url(repository: &str) -> String {
    if repository.starts_with("git+") || repository.ends_with(".git") {
        return repository.to_owned();
    }
    match repository.strip_prefix("https://").or_else(|| repository.strip_prefix("http://")) {
        Some(rest) => format!("git+https://{}.git", rest.trim_end_matches('/')),
        None => repository.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        Package {
            name: "Hello".to_owned(),
            version: "1.2.3".to_owned(),
            group: None,
            description: Some("Greets".to_owned()),
            authors: vec!["Ada".to_owned(), "Grace".to_owned()],
            license: Some("MIT".to_owned()),
            homepage: Some("https://example.com/hello".to_owned()),
            repository: Some("https://github.com/acme/hello".to_owned()),
            publish: true,
        }
    }

    fn library<'a>(
        package: &'a Package,
        settings: Option<&'a NpmPublishSettings>,
        deps: &'a BTreeMap<String, String>,
    ) -> LibraryPackage<'a> {
        LibraryPackage {
            name: "@acme/hello".to_owned(),
            package,
            settings,
            module: "hello",
            extension: "mjs",
            types_extension: "d.mts",
            es: true,
            formats: None,
            files: vec!["*.mjs".to_owned(), "*.wasm".to_owned(), "*.d.mts".to_owned()],
            dependencies: deps,
        }
    }

    #[test]
    fn writes_the_fields_registries_show() {
        let package = package();
        let deps = BTreeMap::from([("@js-joda/core".to_owned(), "3.2.0".to_owned())]);
        let settings = NpmPublishSettings {
            keywords: vec!["kotlin".into(), "wasm".into()],
            side_effects: Some(NpmSideEffects::Flag(false)),
            engines: BTreeMap::from([("node".to_owned(), ">=20".to_owned())]),
            ..Default::default()
        };
        let text = library(&package, Some(&settings), &deps).json();
        assert!(
            text.contains("\"types\": \"./hello.d.mts\",\n      \"import\": \"./hello.mjs\",\n      \"default\""),
            "`types` comes first:\n{text}"
        );
        assert!(text.find("\"name\"").unwrap() < text.find("\"version\"").unwrap(), "{text}");
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["name"], "@acme/hello");
        assert_eq!(json["keywords"], serde_json::json!(["kotlin", "wasm"]));
        assert_eq!(json["homepage"], "https://example.com/hello");
        assert_eq!(json["bugs"]["url"], "https://github.com/acme/hello/issues");
        assert_eq!(json["repository"], serde_json::json!({ "type": "git", "url": "git+https://github.com/acme/hello.git" }));
        assert_eq!(json["sideEffects"], false);
        assert_eq!(json["engines"]["node"], ">=20");
        assert_eq!(json["files"], serde_json::json!(["*.mjs", "*.wasm", "*.d.mts"]));
        assert_eq!(json["dependencies"]["@js-joda/core"], "3.2.0");
        assert_eq!(json["contributors"][0], "Grace");
    }

    #[test]
    fn leaves_out_what_is_not_set() {
        let package = Package { description: None, authors: vec![], license: None, homepage: None, repository: None, ..package() };
        let deps = BTreeMap::new();
        let json: serde_json::Value = serde_json::from_str(&library(&package, None, &deps).json()).unwrap();
        for key in ["dependencies", "author", "license", "keywords", "homepage", "bugs", "repository", "sideEffects", "engines"] {
            assert!(json.get(key).is_none(), "{key} is not set");
        }
    }

    #[test]
    fn commonjs_modules_export_require() {
        let package = package();
        let deps = BTreeMap::new();
        let mut cjs = library(&package, None, &deps);
        (cjs.extension, cjs.types_extension, cjs.es) = ("js", "d.ts", false);
        let json: serde_json::Value = serde_json::from_str(&cjs.json()).unwrap();
        assert_eq!(json["type"], "commonjs");
        assert_eq!(json["exports"]["."]["require"], "./hello.js");
        assert!(json.get("module").is_none() && json["exports"]["."].get("import").is_none());
    }

    #[test]
    fn bundled_libraries_name_the_file_of_each_format() {
        let package = package();
        let deps = BTreeMap::from([("lodash".to_owned(), "^4.17.0".to_owned())]);
        let formats = [LibraryFormat::Esm, LibraryFormat::Cjs, LibraryFormat::Umd];
        let mut bundled = library(&package, None, &deps);
        (bundled.formats, bundled.files) =
            (Some(&formats[..]), vec!["*.mjs".into(), "*.cjs".into(), "*.umd.js".into(), "*.d.mts".into(), "*.d.cts".into()]);
        let json: serde_json::Value = serde_json::from_str(&bundled.json()).unwrap();
        assert_eq!((json["main"].as_str(), json["module"].as_str()), (Some("./hello.cjs"), Some("./hello.mjs")));
        assert_eq!(
            (json["types"].as_str(), json["unpkg"].as_str(), json["jsdelivr"].as_str()),
            (Some("./hello.d.mts"), Some("./hello.umd.js"), Some("./hello.umd.js"))
        );
        assert_eq!(json["exports"]["."]["import"]["default"], "./hello.mjs");
        assert_eq!(json["exports"]["."]["import"]["types"], "./hello.d.mts");
        assert_eq!(json["exports"]["."]["require"]["default"], "./hello.cjs");
        assert_eq!(json["exports"]["."]["require"]["types"], "./hello.d.cts");
        assert_eq!(json["exports"]["."]["default"], "./hello.cjs");
        assert!(json.get("type").is_none(), "a package of more than one format is not only one of them:\n{json}");
        assert_eq!(json["dependencies"]["lodash"], "^4.17.0");

        // Without UMD there is no file for `<script>` tags; without an ES module the declarations are of a `require`.
        let formats = [LibraryFormat::Cjs];
        let mut only_cjs = library(&package, None, &deps);
        (only_cjs.formats, only_cjs.files) = (Some(&formats[..]), vec!["*.cjs".into(), "*.d.cts".into()]);
        let json: serde_json::Value = serde_json::from_str(&only_cjs.json()).unwrap();
        assert_eq!((json["main"].as_str(), json["types"].as_str()), (Some("./hello.cjs"), Some("./hello.d.cts")));
        assert!(json.get("module").is_none() && json.get("unpkg").is_none());
        assert!(json["exports"]["."].get("import").is_none() && json["exports"]["."]["require"]["default"] == "./hello.cjs");
    }

    #[test]
    fn explicit_settings_win_over_derived_fields() {
        let package = package();
        let settings = NpmPublishSettings {
            bugs: Some("https://example.com/issues".into()),
            side_effects: Some(NpmSideEffects::Files(vec!["./polyfill.mjs".into()])),
            ..Default::default()
        };
        let deps = BTreeMap::new();
        let json: serde_json::Value = serde_json::from_str(&library(&package, Some(&settings), &deps).json()).unwrap();
        assert_eq!(json["bugs"]["url"], "https://example.com/issues");
        assert_eq!(json["sideEffects"], serde_json::json!(["./polyfill.mjs"]));
        let elsewhere = Package { repository: Some("https://code.example.com/hello".into()), ..package };
        assert_eq!(bugs_url(&elsewhere, None), None);
    }

    #[test]
    fn packages_of_a_workspace_depend_on_each_other_by_version() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, text: &str| {
            std::fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(path), text).unwrap();
        };
        let library = |name: &str, version: &str, deps: &str| {
            format!(
                "[package]\nname = \"{name}\"\nversion = \"{version}\"\n[target.wasm]\nkind = \"library\"\nenvironment = \"js\"\n\
                 [target.wasm.npm-dependencies]\n{deps}"
            )
        };
        write("kargo.toml", "[workspace]\nmembers = [\"app\", \"core\", \"ui\"]\n");
        write("core/kargo.toml", &library("core", "2.1.0", ""));
        write(
            "ui/kargo.toml",
            &library(
                "ui",
                "2.1.0",
                "core = \"workspace:*\"\nfancy = \"workspace:^\"\nbundled = \"file:../core/target/release/wasm\"\nleft-pad = \"^1.3.0\"\n",
            ),
        );
        write("app/kargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        let index = |name: &str| (0..project.packages().len()).find(|&i| project.package(i).name() == name).unwrap();
        let ui = project.package(index("ui"));

        let deps = BTreeMap::from([
            ("core".to_owned(), "workspace:*".to_owned()),
            ("core-twin".to_owned(), "workspace:^".to_owned()),
            ("bundled".to_owned(), "file:../core/target/release/wasm".to_owned()),
            ("elsewhere".to_owned(), "file:../../vendor/elsewhere".to_owned()),
            ("left-pad".to_owned(), "^1.3.0".to_owned()),
        ]);
        let published = published_dependencies(&project, ui, deps);
        assert_eq!(published["core"], "2.1.0", "`workspace:*` is the exact version");
        assert_eq!(published["core-twin"], "workspace:^", "a name no package publishes stays as it is");
        assert_eq!(published["bundled"], "2.1.0", "a path into the package's directory");
        assert_eq!(published["elsewhere"], "file:../../vendor/elsewhere");
        assert_eq!(published["left-pad"], "^1.3.0");

        // `ui` is published after `core`, which it depends on, though the workspace lists them the other way round.
        assert_eq!(member_dependencies(&project, index("ui")), [index("core")]);
        assert_eq!(crate::ops::publish::publish_order(&project, &[index("ui"), index("core")]), [index("core"), index("ui")]);
    }

    #[test]
    fn scope_applies_to_unscoped_names_only() {
        let settings = NpmPublishSettings { scope: Some("acme".into()), ..Default::default() };
        assert_eq!(scoped_name("hello".into(), Some(&settings)), "@acme/hello");
        assert_eq!(scoped_name("@other/hello".into(), Some(&settings)), "@other/hello");
        assert_eq!(scoped_name("hello".into(), None), "hello");
    }
}
