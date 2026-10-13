//! Importing a Kargo project for a Kotlin language server: `kargo metadata`, turned into the
//! server's own project description.
//!
//! - The JetBrains Kotlin LSP reads `workspace.json` at the project root (its format for build
//!   systems it doesn't know) and reloads it when it changes. Kargo writes one module per
//!   package for main and one for test code, with the target's classpath as libraries (with
//!   their sources jars), the JDK and the Kotlin compiler settings and plugins. A multiplatform
//!   package gets a module per source set instead (see [`workspace_json`]).
//! - fwcd's kotlin-language-server runs a `kls-classpath` script at the project root. Kargo
//!   writes one that prints the test classpath it keeps in `target/lsp/classpath.txt`.
//!
//! Generated files are only replaced while they still hold what Kargo last wrote, so a
//! hand-written `workspace.json` is never overwritten.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use kargo_ops::ops::context::BuildContext;
use kargo_ops::ops::metadata::{self, MetadataOptions};
use kargo_ops::workspace::{ProfileKind, Project};
use kargo_toolchain::ServerKind;
use kargo_util::config::Config;

/// The project's `kargo metadata` document.
pub fn metadata(config: &Config, root: &Path, no_deps: bool) -> Result<Value> {
    let project = Project::discover(root, None)?;
    let ctx = BuildContext::new(config, &project, project.members()[0], ProfileKind::Dev)?;
    metadata::metadata(&ctx, &MetadataOptions { no_deps })
}

/// The project's fingerprint (see `kargo metadata --fingerprint`).
pub fn fingerprint(root: &Path) -> Result<String> {
    Ok(metadata::fingerprint(&Project::discover(root, None)?))
}

/// The directory of the project containing `dir`.
pub fn project_root(dir: &Path) -> Option<PathBuf> {
    Project::discover(dir, None).ok().map(|p| p.root().to_path_buf())
}

/// The result of importing a project.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The project's metadata: with its dependencies, or without them if they couldn't be resolved.
    pub metadata: Option<Value>,
    /// Why the import failed or is incomplete.
    pub error: Option<String>,
    pub written: Written,
}

/// Imports the project at `root` and writes the description `kind` reads. When dependencies
/// can't be resolved (offline, a broken `kargo.toml`), the project is described without them.
pub fn import(config: &Config, root: &Path, kind: Option<ServerKind>, targets: Targets) -> Outcome {
    let fingerprint = fingerprint(root).ok();
    let (metadata, mut error) = match metadata(config, root, false) {
        Ok(m) => (Some(m), None),
        Err(e) => (metadata(config, root, true).ok(), Some(format!("{e:#}"))),
    };
    let mut written = Written::default();
    if let (Some(kind), Some(m)) = (kind, &metadata) {
        match write(kind, m, targets) {
            Ok(w) => written = w,
            Err(e) => {
                error.get_or_insert(format!("{e:#}"));
            }
        }
        if error.is_none()
            && let Some(fingerprint) = &fingerprint
        {
            let record = format!(
                "{} {fingerprint}
",
                description(kind, targets)
            );
            let _ = kargo_util::fs::write_string_atomic(&state_dir(m).join("fingerprint"), &record);
        }
    }
    Outcome { metadata, error, written }
}

/// Whether the last complete import of the project at `root` for `kind` saw the project as it
/// is now, so its description is up to date.
pub fn is_fresh(root: &Path, kind: ServerKind, targets: Targets) -> bool {
    let Ok(project) = Project::discover(root, None) else { return false };
    let generated = match kind {
        ServerKind::KotlinLsp => project.root().join("workspace.json"),
        ServerKind::Kls => project.root().join(if cfg!(windows) { "kls-classpath.bat" } else { "kls-classpath" }),
    };
    let stored = std::fs::read_to_string(project.target_dir().join("lsp").join("fingerprint")).unwrap_or_default();
    generated.is_file() && stored.trim() == format!("{} {}", description(kind, targets), metadata::fingerprint(&project))
}

/// Imports the project at `root` again, for the server and targets of its last complete import
/// (by `kargo lsp` or `kargo ide`), if `kargo.toml` or `kargo.lock` changed since. Returns `None`
/// when the project was never imported or its description is up to date.
pub fn refresh(config: &Config, root: &Path) -> Option<Outcome> {
    let project = Project::discover(root, None).ok()?;
    let record = std::fs::read_to_string(project.target_dir().join("lsp").join("fingerprint")).ok()?;
    let (recorded, _) = record.trim().split_once(' ')?;
    let combinations = [(ServerKind::KotlinLsp, Targets::Jvm), (ServerKind::KotlinLsp, Targets::All), (ServerKind::Kls, Targets::Jvm)];
    let &(kind, targets) = combinations.iter().find(|(k, t)| description(*k, *t) == recorded)?;
    if is_fresh(project.root(), kind, targets) {
        return None;
    }
    Some(import(config, project.root(), Some(kind), targets))
}

/// What an import wrote, as its fingerprint record names it: the server, and for the Kotlin LSP
/// which targets `workspace.json` describes.
fn description(kind: ServerKind, targets: Targets) -> String {
    match kind {
        ServerKind::KotlinLsp => format!("{}{}", kind.as_str(), targets.record_suffix()),
        ServerKind::Kls => kind.as_str().to_owned(),
    }
}

/// What an import wrote.
#[derive(Debug, Default)]
pub struct Written {
    /// Generated files whose contents changed.
    pub changed: Vec<PathBuf>,
    /// Generated files Kargo left alone because someone else wrote them.
    pub foreign: Vec<PathBuf>,
}

/// Writes the description `kind` reads of the project described by `metadata`.
pub fn write(kind: ServerKind, metadata: &Value, targets: Targets) -> Result<Written> {
    let root = PathBuf::from(metadata["root"].as_str().unwrap_or_default());
    let state = state_dir(metadata);
    let mut written = Written::default();
    match kind {
        ServerKind::KotlinLsp => {
            let text = serde_json::to_string_pretty(&workspace_json(metadata, targets))? + "\n";
            generate(&root.join("workspace.json"), &text, &state, &mut written)?;
        }
        ServerKind::Kls => {
            let classpath_file = state.join("classpath.txt");
            let classpath = kargo_util::fs::join_paths(&test_classpath(metadata));
            let changed = std::fs::read_to_string(&classpath_file).ok().as_deref() != Some(classpath.as_str());
            if changed {
                kargo_util::fs::write_string_atomic(&classpath_file, &classpath)?;
            }
            let shown = classpath_file.display();
            let (script, text) = if cfg!(windows) {
                (root.join("kls-classpath.bat"), format!("@echo off\r\nrem {MARKER}\r\ntype \"{shown}\"\r\n"))
            } else {
                (root.join("kls-classpath"), format!("#!/bin/sh\n# {MARKER}\ncat '{shown}'\n"))
            };
            generate(&script, &text, &state, &mut written)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if !written.foreign.contains(&script) {
                    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            if changed && !written.changed.contains(&script) {
                written.changed.push(script);
            }
        }
    }
    Ok(written)
}

/// Written into generated scripts.
const MARKER: &str = "Generated by `kargo lsp` or `kargo ide`; it is replaced when kargo.toml or kargo.lock change.";

/// Where `kargo lsp` keeps its state: `target/lsp`.
fn state_dir(metadata: &Value) -> PathBuf {
    PathBuf::from(metadata["target-dir"].as_str().unwrap_or("target")).join("lsp")
}

/// Writes a generated file unless someone else wrote what is there now. Kargo remembers the
/// hash of what it wrote last in the state directory.
fn generate(path: &Path, text: &str, state: &Path, written: &mut Written) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let record = state.join(format!("{name}.sha256"));
    let current = std::fs::read_to_string(path).ok();
    if current.as_deref() == Some(text) {
        return Ok(());
    }
    if let Some(current) = &current {
        let ours = std::fs::read_to_string(&record).ok().is_some_and(|h| h.trim() == kargo_util::hash::sha256_bytes(current.as_bytes()));
        if !ours {
            written.foreign.push(path.to_path_buf());
            return Ok(());
        }
    }
    kargo_util::fs::write_string_atomic(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    kargo_util::fs::write_string_atomic(&record, &kargo_util::hash::sha256_bytes(text.as_bytes()))?;
    written.changed.push(path.to_path_buf());
    Ok(())
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The JVM target of a package, if it has one.
fn jvm_target(package: &Value) -> Option<&Value> {
    package["targets"].as_array()?.iter().find(|t| t["kind"] == "jvm")
}

/// The classpath (or klib path) of a scope: Maven artifacts with their paths, path dependencies
/// by package name. A Native target's is that of its first platform.
fn classpath<'a>(target: &'a Value, scope: &str) -> impl Iterator<Item = &'a Value> {
    target["dependencies"][0][scope].as_array().into_iter().flatten()
}

/// Every jar on the test classpaths of the project's JVM targets, for fwcd's server, plus the
/// output directories of path dependencies.
fn test_classpath(metadata: &Value) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let Some(jvm) = jvm_target(package) else { continue };
        for entry in classpath(jvm, "test") {
            let path = match entry["path"].as_str() {
                Some(p) => PathBuf::from(p),
                None => match package_named(metadata, str_of(entry, "package")).and_then(jvm_target) {
                    Some(dep) => PathBuf::from(str_of(dep, "classes-dir")),
                    None => continue,
                },
            };
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

fn package_named<'a>(metadata: &'a Value, name: &str) -> Option<&'a Value> {
    metadata["packages"].as_array()?.iter().find(|p| p["name"] == name)
}

fn url(path: &str) -> String {
    let path = path.replace('\\', "/");
    if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") }
}

/// Which targets `workspace.json` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Targets {
    /// The JVM target. The common code of a multiplatform package is analyzed as JVM code.
    #[default]
    Jvm,
    /// Wasm and Native as well (`kargo lsp --klib-targets`). The Kotlin LSP doesn't read klibs
    /// yet, so their libraries don't resolve.
    All,
}

impl Targets {
    /// How the fingerprint record of an import names the description, after the server's name.
    fn record_suffix(self) -> &'static str {
        match self {
            Targets::Jvm => "",
            Targets::All => "+klib",
        }
    }
}

/// The targets of a package that get modules: the JVM, and Wasm and Native with [`Targets::All`].
fn analyzed_targets(package: &Value, targets: Targets) -> Vec<&Value> {
    let analyzed = |t: &&Value| match t["kind"].as_str() {
        Some("jvm") => true,
        Some("wasm" | "native") => targets == Targets::All,
        _ => false,
    };
    package["targets"].as_array().into_iter().flatten().filter(analyzed).collect()
}

/// Whether a package has several targets, so its `src/` is common code compiled for each of
/// them, refined by `src-<target>/`.
fn is_multiplatform(package: &Value) -> bool {
    package["targets"].as_array().is_some_and(|t| t.len() > 1)
}

/// The modules holding a package's main code for `kind`: `<name>.main` for a package with one
/// target, `<name>.<kind>.main` and `<name>.common.main` for a multiplatform one.
fn code_modules(package: &Value, kind: &str, targets: Targets) -> Vec<String> {
    let name = str_of(package, "name");
    let analyzed = analyzed_targets(package, targets);
    let has_kind = analyzed.iter().any(|t| t["kind"] == kind);
    match (is_multiplatform(package), analyzed.is_empty()) {
        (_, true) => Vec::new(),
        (false, _) if has_kind => vec![format!("{name}.main")],
        (false, _) => Vec::new(),
        (true, _) if has_kind => vec![format!("{name}.{kind}.main"), format!("{name}.common.main")],
        (true, _) => vec![format!("{name}.common.main")],
    }
}

/// How the Kotlin plugin sees a target: its platform, the class of its compiler arguments and
/// the kind of its libraries.
struct Platform {
    /// The `targetPlatform` of its modules, as the Kotlin plugin serializes it: `JVM [21]`,
    /// `Wasm [JS]`, `Native [linux_x64]`; several are joined by `/`.
    name: String,
    /// The compiler arguments' class: `J`VM, `W`asm, `N`ative or `M`etadata (common code).
    arguments: &'static str,
    /// The type of its libraries: Kotlin's library kinds for klibs.
    library_type: &'static str,
}

fn platform(target: &Value) -> Platform {
    match str_of(target, "kind") {
        "jvm" => Platform { name: format!("JVM [{}]", str_of(target, "jvm-target")), arguments: "J", library_type: "COMPILE" },
        "wasm" if target["environment"] == "wasi" => {
            Platform { name: "Wasm [WASI]".into(), arguments: "W", library_type: "kotlin.wasm.wasi" }
        }
        "wasm" => Platform { name: "Wasm [JS]".into(), arguments: "W", library_type: "kotlin.wasm.js" },
        _ => Platform { name: format!("Native [{}]", konan_target(target)), arguments: "N", library_type: "kotlin.native" },
    }
}

/// The Kotlin/Native platform a Native target is analyzed for: the first of its platforms.
fn konan_target(target: &Value) -> &str {
    target["platforms"][0].as_str().unwrap_or_default()
}

/// Which source directories of a target a module gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Roots {
    /// Common and target code, and the resources.
    All,
    /// Common code: `src/` or `tests/`.
    Common,
    /// The target's own code (`src-<target>/` or `tests-<target>/`) and the resources.
    Own,
}

/// A target's main or test source and resource directories, with their source root types.
fn roots(target: &Value, test: bool, part: Roots) -> Vec<(String, &'static str)> {
    let (sources, resources) = if test { ("test-roots", "test-resource-roots") } else { ("source-roots", "resource-roots") };
    let (source_type, resource_type) = if test { ("java-test", "java-test-resource") } else { ("java-source", "java-resource") };
    let dirs = |key: &str| target[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
    // `kargo metadata` lists the common directory first.
    let mut out: Vec<(String, &str)> = Vec::new();
    for (i, dir) in dirs(sources).into_iter().enumerate() {
        let keep = match part {
            Roots::All => true,
            Roots::Common => i == 0,
            Roots::Own => i > 0,
        };
        if keep {
            out.push((dir, source_type));
        }
    }
    if part != Roots::Common {
        out.extend(dirs(resources).into_iter().map(|dir| (dir, resource_type)));
    }
    out
}

/// One module of `workspace.json`: a package's main or test code, all of it or one source set.
struct SourceSet<'a> {
    module: String,
    /// The target whose libraries and compiler settings the module gets.
    target: &'a Value,
    platform: Platform,
    roots: Vec<(String, &'static str)>,
    test: bool,
    /// Modules it depends on before its libraries: its main code, the common code it refines.
    modules: Vec<String>,
    /// The common source set it refines, whose `expect` declarations it implements.
    depends_on: Vec<String>,
    /// Modules whose `internal` declarations it sees.
    visible: Vec<String>,
    /// A source set of a multiplatform package.
    multiplatform: bool,
    /// Whether it gets the target's libraries; common code compiled for several platforms has none.
    libraries: bool,
    /// Whether it gets the target's output directories.
    outputs: bool,
}

/// `workspace.json` while it is being written.
struct Workspace<'a> {
    metadata: &'a Value,
    targets: Targets,
    sdk: Value,
    modules: Vec<Value>,
    libraries: Vec<Value>,
    kotlin_settings: Vec<Value>,
    java_settings: Vec<Value>,
}

/// The JetBrains Kotlin LSP's `workspace.json` for the project.
///
/// A package with one target gets a module for its main and one for its test code. A
/// multiplatform package gets one per source set: `<name>.common.main` for `src/` and
/// `<name>.<target>.main` for `src-<target>/` (and the same for tests), each refining the common
/// one, so `expect` and `actual` declarations find each other. Only the targets of [`Targets`]
/// get modules; Android code is left to Android Studio.
pub fn workspace_json(metadata: &Value, targets: Targets) -> Value {
    let jdk = &metadata["toolchain"]["jdk"];
    let sdk_name = jdk["version"].as_u64().map(|v| v.to_string());
    let mut sdks: Vec<Value> = Vec::new();
    if let (Some(name), Some(home)) = (&sdk_name, jdk["home"].as_str()) {
        sdks.push(json!({ "name": name, "type": "JavaSDK", "version": format!("JDK_{name}"), "homePath": home, "additionalData": "" }));
    }
    let sdk = match &sdk_name {
        Some(name) => json!({ "type": "sdk", "name": name, "kind": "JavaSDK" }),
        None => json!({ "type": "inheritedSdk" }),
    };
    let mut ws = Workspace {
        metadata,
        targets,
        sdk,
        modules: Vec::new(),
        libraries: Vec::new(),
        kotlin_settings: Vec::new(),
        java_settings: Vec::new(),
    };
    for package in metadata["packages"].as_array().into_iter().flatten() {
        ws.package(package);
    }
    json!({
        "modules": ws.modules,
        "libraries": ws.libraries,
        "sdks": sdks,
        "kotlinSettings": ws.kotlin_settings,
        "javaSettings": ws.java_settings,
    })
}

impl<'a> Workspace<'a> {
    fn package(&mut self, package: &'a Value) {
        let name = str_of(package, "name");
        let root = str_of(package, "root");
        self.modules.push(json!({
            "name": name,
            "dependencies": [{ "type": "moduleSource" }, self.sdk.clone()],
            "contentRoots": [{ "path": root, "excludedUrls": [url(&Path::new(root).join("target").to_string_lossy())] }],
            "externalProjectPath": root,
            "externalProjectId": name,
        }));
        let analyzed = analyzed_targets(package, self.targets);
        let suffix = |test: bool| if test { "test" } else { "main" };
        if !is_multiplatform(package)
            && let [target] = analyzed.as_slice()
        {
            for test in [false, true] {
                let main = format!("{name}.main");
                let own = if test { vec![main] } else { Vec::new() };
                self.add(
                    package,
                    SourceSet {
                        module: format!("{name}.{}", suffix(test)),
                        target,
                        platform: platform(target),
                        roots: roots(target, test, Roots::All),
                        test,
                        modules: own.clone(),
                        depends_on: Vec::new(),
                        visible: own,
                        multiplatform: false,
                        libraries: true,
                        outputs: true,
                    },
                );
            }
            return;
        }
        let Some(&first) = analyzed.first() else { return };
        // Common code is analyzed as JVM code when there is a JVM target: the Kotlin LSP gives
        // common code compiled for several platforms no libraries.
        let jvm = analyzed.iter().copied().find(|t| t["kind"] == "jvm");
        let common_main = format!("{name}.common.main");
        for test in [false, true] {
            let common = format!("{name}.common.{}", suffix(test));
            let (target, common_platform, libraries) = match jvm {
                Some(jvm) => (jvm, platform(jvm), true),
                None => {
                    let mut names: Vec<String> = analyzed.iter().map(|t| platform(t).name).collect();
                    names.sort();
                    (first, Platform { name: names.join("/"), arguments: "M", library_type: "" }, false)
                }
            };
            let own = if test { vec![common_main.clone()] } else { Vec::new() };
            self.add(
                package,
                SourceSet {
                    module: common.clone(),
                    target,
                    platform: common_platform,
                    roots: roots(target, test, Roots::Common),
                    test,
                    modules: own.clone(),
                    depends_on: Vec::new(),
                    visible: own,
                    multiplatform: true,
                    libraries,
                    outputs: false,
                },
            );
            for &target in &analyzed {
                let kind = str_of(target, "kind");
                let main = format!("{name}.{kind}.main");
                let (modules, visible) = if test {
                    (vec![main.clone(), common.clone(), common_main.clone()], vec![main, common_main.clone()])
                } else {
                    (vec![common.clone()], Vec::new())
                };
                self.add(
                    package,
                    SourceSet {
                        module: format!("{name}.{kind}.{}", suffix(test)),
                        target,
                        platform: platform(target),
                        roots: roots(target, test, Roots::Own),
                        test,
                        modules,
                        depends_on: vec![common.clone()],
                        visible,
                        multiplatform: true,
                        libraries: true,
                        outputs: true,
                    },
                );
            }
        }
    }

    fn add(&mut self, package: &Value, set: SourceSet) {
        let mut dependencies: Vec<Value> = set.modules.iter().map(|m| json!({ "type": "module", "name": m, "scope": "compile" })).collect();
        if set.libraries {
            let kind = str_of(set.target, "kind");
            let scopes: &[(&str, &str)] = if set.test { &[("test", "compile")] } else { &[("compile", "compile"), ("runtime", "runtime")] };
            for (scope, ide_scope) in scopes {
                for entry in classpath(set.target, scope) {
                    let found: Vec<Value> = match entry["path"].as_str() {
                        Some(file) => {
                            let library = self.library(entry, file, set.platform.library_type);
                            vec![json!({ "type": "library", "name": library, "scope": ide_scope })]
                        }
                        None => package_named(self.metadata, str_of(entry, "package"))
                            .map(|dep| code_modules(dep, kind, self.targets))
                            .unwrap_or_default()
                            .into_iter()
                            .map(|m| json!({ "type": "module", "name": m, "scope": ide_scope }))
                            .collect(),
                    };
                    // A library on both the compile and runtime classpath is a compile dependency.
                    for dependency in found {
                        if !dependencies.iter().any(|d| d["name"] == dependency["name"]) {
                            dependencies.push(dependency);
                        }
                    }
                }
            }
            if kind == "native" {
                for library in native_distribution(self.metadata, konan_target(set.target)) {
                    dependencies.push(json!({ "type": "library", "name": library["name"], "scope": "compile" }));
                    if !self.libraries.iter().any(|l| l["name"] == library["name"]) {
                        self.libraries.push(library);
                    }
                }
            }
        }
        dependencies.push(json!({ "type": "moduleSource" }));
        dependencies.push(self.sdk.clone());
        let name = str_of(package, "name");
        let root = str_of(package, "root");
        self.modules.push(json!({
            "name": set.module,
            "dependencies": dependencies,
            "contentRoots": set.roots.iter().map(|(path, ty)| json!({ "path": path, "sourceRoots": [{ "path": path, "type": ty }] })).collect::<Vec<_>>(),
            "externalProjectPath": root,
            "externalProjectId": name,
        }));
        self.kotlin_settings.push(kotlin_settings_entry(package, &set));
        if set.platform.arguments == "J" {
            let jvm_target = str_of(set.target, "jvm-target");
            let mut java = json!({
                "module": set.module,
                "inheritedCompilerOutput": false,
                "excludeOutput": true,
                "languageLevelId": format!("JDK_{}", jvm_target.replace('.', "_")),
                "manifestAttributes": {},
                "compilerArguments": set.target["javac-args"],
            });
            if set.outputs {
                let classes = str_of(set.target, if set.test { "test-classes-dir" } else { "classes-dir" });
                java[if set.test { "compilerOutputsForTests" } else { "compilerOutputs" }] = json!([classes]);
            }
            self.java_settings.push(java);
        }
    }

    /// The name of the library for a classpath entry, added to the project's libraries.
    fn library(&mut self, entry: &Value, file: &str, library_type: &str) -> String {
        let name = library_name(entry, &self.libraries, file);
        if !self.libraries.iter().any(|l| l["name"] == name.as_str()) {
            self.libraries.push(library_entry(&name, entry, library_type));
        }
        name
    }
}

/// A library's name: `Kargo: group:artifact:version`, with the file name when a module has
/// several files (classifiers).
fn library_name(entry: &Value, libraries: &[Value], jar: &str) -> String {
    let base = format!("Kargo: {}:{}", str_of(entry, "module"), str_of(entry, "version"));
    let taken = libraries.iter().find(|l| l["name"] == base.as_str());
    match taken {
        Some(library) if library["roots"][0]["path"] != jar => {
            let file = Path::new(jar).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{base} ({file})")
        }
        _ => base,
    }
}

fn library_entry(name: &str, entry: &Value, library_type: &str) -> Value {
    let mut roots = vec![json!({ "path": entry["path"] })];
    if let Some(sources) = entry["sources"].as_str() {
        roots.push(json!({ "path": sources, "type": "SOURCES" }));
    }
    let module = str_of(entry, "module");
    let (group, artifact) = module.split_once(':').unwrap_or(("", module));
    let version = str_of(entry, "version");
    json!({
        "name": name,
        "type": library_type,
        "roots": roots,
        "properties": { "attributes": { "groupId": group, "artifactId": artifact, "version": version, "baseVersion": version } },
    })
}

/// The klibs of the Kotlin/Native distribution for `konan_target`: the standard library and
/// the platform libraries (`posix`, ...), which Native code compiles against without
/// declaring them.
fn native_distribution(metadata: &Value, konan_target: &str) -> Vec<Value> {
    let native = &metadata["toolchain"]["kotlin-native"];
    let Some(home) = native["home"].as_str() else { return Vec::new() };
    let version = str_of(native, "version");
    let klib = Path::new(home).join("klib");
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    if klib.join("common").join("stdlib").is_dir() {
        found.push(("stdlib".to_owned(), klib.join("common").join("stdlib")));
    }
    if let Ok(dir) = std::fs::read_dir(klib.join("platform").join(konan_target)) {
        let mut platform: Vec<PathBuf> = dir.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
        platform.sort();
        for path in platform {
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            found.push((format!("{file} [{konan_target}]"), path));
        }
    }
    found
        .into_iter()
        .map(|(name, path)| {
            json!({
                "name": format!("Kotlin/Native {version}: {name}"),
                "type": "kotlin.native",
                "roots": [{ "path": path.display().to_string() }],
            })
        })
        .collect()
}

fn kotlin_settings_entry(package: &Value, set: &SourceSet) -> Value {
    let kotlin = &package["kotlin"];
    let target = set.target;
    let plugins: Vec<&Value> = target["compiler-plugins"].as_array().into_iter().flatten().collect();
    let mut arguments = json!({});
    if set.platform.arguments == "J" {
        arguments["jvmTarget"] = target["jvm-target"].clone();
    }
    arguments["pluginOptions"] = json!(plugins.iter().flat_map(|p| p["options"].as_array().into_iter().flatten()).collect::<Vec<_>>());
    arguments["pluginClasspaths"] = json!(plugins.iter().filter_map(|p| p["jar"].as_str()).collect::<Vec<_>>());
    for (key, field) in [("language-version", "languageVersion"), ("api-version", "apiVersion")] {
        if let Some(v) = kotlin[key].as_str() {
            arguments[field] = json!(v);
        }
    }
    // Without it, the compiler front end rejects `expect` and `actual` declarations.
    if set.multiplatform {
        arguments["multiPlatform"] = json!(true);
    }
    let additional: Vec<String> =
        target["compiler-args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(shell_word).collect();
    // A single JVM module keeps the JVM platform the compiler arguments imply.
    let target_platform = if !set.multiplatform && set.platform.arguments == "J" { Value::Null } else { json!(set.platform.name) };
    json!({
        "name": "Kotlin",
        "sourceRoots": set.roots.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        "configFileItems": [],
        "module": set.module,
        "useProjectSettings": false,
        "implementedModuleNames": [],
        "dependsOnModuleNames": set.depends_on,
        "additionalVisibleModuleNames": set.visible,
        "productionOutputPath": null,
        "testOutputPath": null,
        "sourceSetNames": [],
        "isTestModule": set.test,
        "externalProjectId": set.module,
        "isHmppEnabled": true,
        "pureKotlinSourceFolders": [],
        "kind": if set.multiplatform { "source_set_holder" } else { "default" },
        "compilerArguments": format!("{}{arguments}", set.platform.arguments),
        "additionalArguments": if additional.is_empty() { Value::Null } else { json!(additional.join(" ")) },
        "scriptTemplates": null,
        "scriptTemplatesClasspath": null,
        "copyJsLibraryFiles": false,
        "outputDirectoryForJsLibraryFiles": null,
        "targetPlatform": target_platform,
        "externalSystemRunTasks": [],
        "version": 5,
        "flushNeeded": false,
    })
}

/// An argument as one word of a command line, quoted only when it needs to be.
fn shell_word(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(|c: char| c.is_whitespace() || c == '"') {
        return arg.to_owned();
    }
    kargo_util::process::quote_arg(arg)
}

/// The `kargo/runnables` of the project: its programs, tests and tasks, each with the `kargo`
/// command line that runs it. With `file`, only those of the package containing it.
pub fn runnables(metadata: &Value, kargo: &str, file: Option<&Path>) -> Vec<Value> {
    let packages: Vec<&Value> = metadata["packages"].as_array().into_iter().flatten().filter(|p| p["member"] == true).collect();
    let selected: Vec<&Value> = match file {
        // The innermost package root containing the file.
        Some(file) => packages
            .iter()
            .filter(|p| file.starts_with(str_of(p, "root")))
            .max_by_key(|p| str_of(p, "root").len())
            .into_iter()
            .copied()
            .collect(),
        None => packages,
    };
    let mut out = Vec::new();
    for package in selected {
        let name = str_of(package, "name");
        let cwd = str_of(package, "root");
        let mut add = |label: String, kind: &str, target: Option<&str>, args: Vec<String>, extra: Value| {
            let mut runnable =
                json!({ "label": label, "kind": kind, "package": name, "target": target, "cwd": cwd, "program": kargo, "args": args });
            if let Value::Object(extra) = extra {
                runnable.as_object_mut().expect("an object").extend(extra);
            }
            out.push(runnable);
        };
        let args = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
        for target in package["targets"].as_array().into_iter().flatten() {
            let kind = str_of(target, "kind");
            let runnable = match kind {
                "jvm" => !target["main-class"].is_null(),
                "native" => !target["entry-point"].is_null(),
                "wasm" => target["wasm-kind"] == "application",
                "android" => true,
                _ => false,
            };
            if runnable {
                let mut extra = json!({ "mainClass": target["main-class"], "entryPoint": target["entry-point"] });
                if kind == "jvm" {
                    extra["debugArgs"] = json!(args(&["run", "-t", kind, "--debug"]));
                }
                add(format!("Run {name} ({kind})"), "run", Some(kind), args(&["run", "-t", kind]), extra);
            }
            let mut extra = json!({});
            if kind == "jvm" {
                extra["debugArgs"] = json!(args(&["test", "-t", kind, "--debug"]));
            }
            add(format!("Test {name} ({kind})"), "test", Some(kind), args(&["test", "-t", kind]), extra);
        }
        for bin in package["bins"].as_array().into_iter().flatten() {
            let bin_name = str_of(bin, "name");
            let extra = json!({ "mainClass": bin["main"], "entryPoint": bin["entry-point"], "path": bin["path"] });
            add(format!("Run {name} --bin {bin_name}"), "run", None, args(&["run", "--bin", bin_name]), extra);
        }
        for example in package["examples"].as_array().into_iter().flatten() {
            let example_name = str_of(example, "name");
            let extra = json!({ "path": example["path"] });
            add(format!("Run example {example_name}"), "run", None, args(&["run", "--example", example_name]), extra);
        }
        for task in package["tasks"].as_array().into_iter().flatten() {
            let task_name = str_of(task, "name");
            let extra = json!({ "description": task["description"] });
            add(format!("Task {task_name}"), "task", None, args(&["task", task_name]), extra);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "root": "/p",
            "target-dir": "/p/target",
            "toolchain": { "jdk": { "home": "/jdk", "version": 21 } },
            "packages": [{
                "name": "app",
                "member": true,
                "root": "/p",
                "kotlin": { "language-version": "2.2" },
                "bins": [{ "name": "tool", "main": "a.ToolKt" }],
                "examples": [{ "name": "demo", "path": "/p/examples/demo.kt" }],
                "tasks": [{ "name": "hello", "description": "Say hi" }],
                "targets": [{
                    "kind": "jvm",
                    "source-roots": ["/p/src", "/p/src-jvm"],
                    "test-roots": ["/p/tests"],
                    "resource-roots": ["/p/resources"],
                    "test-resource-roots": ["/p/test-resources"],
                    "classes-dir": "/p/target/dev/jvm/classes",
                    "test-classes-dir": "/p/target/dev/jvm/test-classes",
                    "jvm-target": "21",
                    "main-class": "a.MainKt",
                    "javac-args": [],
                    "compiler-args": ["-Xcontext-parameters"],
                    "compiler-plugins": [{ "options": ["plugin:x:k=v"], "jar": "/c/plugin.jar" }],
                    "dependencies": [{
                        "platform": "jvm",
                        "compile": [
                            { "package": "lib", "scope": "compile" },
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar" }
                        ],
                        "runtime": [
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar" },
                            { "module": "g:r", "version": "2", "path": "/c/r-2.jar", "sources": null }
                        ],
                        "test": [
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": null },
                            { "module": "g:t", "version": "3", "path": "/c/t-3.jar", "sources": null }
                        ]
                    }]
                }, { "kind": "wasm", "wasm-kind": "library" }]
            }, {
                "name": "lib",
                "member": false,
                "root": "/p/lib",
                "targets": [{ "kind": "jvm", "classes-dir": "/p/lib/target/dev/jvm/classes" }]
            }]
        })
    }

    #[test]
    fn describes_modules_libraries_and_settings() {
        let mut metadata = sample();
        metadata["packages"][0]["targets"].as_array_mut().unwrap().truncate(1);
        let ws = workspace_json(&metadata, Targets::Jvm);
        let names: Vec<&str> = ws["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["app", "app.main", "app.test", "lib", "lib.main", "lib.test"]);
        let main = &ws["modules"][1];
        let deps: Vec<String> = main["dependencies"].as_array().unwrap().iter().map(|d| format!("{}:{}", d["type"], d["name"])).collect();
        assert_eq!(
            deps,
            [
                "\"module\":\"lib.main\"",
                "\"library\":\"Kargo: g:a:1\"",
                "\"library\":\"Kargo: g:r:2\"",
                "\"moduleSource\":null",
                "\"sdk\":\"21\""
            ]
        );
        assert_eq!(main["dependencies"][2]["scope"], "runtime");
        assert_eq!(main["contentRoots"][2]["sourceRoots"][0], json!({ "path": "/p/resources", "type": "java-resource" }));
        let test = &ws["modules"][2];
        assert_eq!(test["dependencies"][0]["name"], "app.main");
        assert_eq!(test["contentRoots"][0]["sourceRoots"][0]["type"], "java-test");
        let a = &ws["libraries"][0];
        assert_eq!(a["roots"][1], json!({ "path": "/c/a-1-sources.jar", "type": "SOURCES" }));
        assert_eq!(a["properties"]["attributes"]["artifactId"], "a");
        assert_eq!(ws["sdks"][0]["homePath"], "/jdk");
        let settings = &ws["kotlinSettings"][0];
        assert_eq!(settings["module"], "app.main");
        let args: Value = serde_json::from_str(settings["compilerArguments"].as_str().unwrap().strip_prefix('J').unwrap()).unwrap();
        assert_eq!(
            args,
            json!({ "jvmTarget": "21", "languageVersion": "2.2", "pluginOptions": ["plugin:x:k=v"], "pluginClasspaths": ["/c/plugin.jar"] })
        );
        assert_eq!(settings["additionalArguments"], "-Xcontext-parameters");
        assert_eq!(ws["javaSettings"][1]["compilerOutputsForTests"], json!(["/p/target/dev/jvm/test-classes"]));
        assert_eq!(ws["javaSettings"][0]["languageLevelId"], "JDK_21");
    }

    /// The sample with Wasm and Native targets that have klibs, and `lib` as a JVM + Wasm package.
    fn multiplatform(native_home: &str) -> Value {
        let mut metadata = sample();
        metadata["toolchain"]["kotlin-native"] = json!({ "home": native_home, "version": "2.4.21" });
        let targets = metadata["packages"][0]["targets"].as_array_mut().unwrap();
        targets[1] = json!({
            "kind": "wasm",
            "environment": "browser",
            "source-roots": ["/p/src", "/p/src-wasm"],
            "test-roots": ["/p/tests", "/p/tests-wasm"],
            "compiler-plugins": [],
            "dependencies": [{
                "platform": "wasm-js",
                "compile": [
                    { "package": "lib", "scope": "compile" },
                    { "module": "g:a-wasm-js", "version": "1", "path": "/c/a-wasm-js-1.klib", "sources": null }
                ],
                "runtime": [],
                "test": [{ "module": "g:t-wasm-js", "version": "3", "path": "/c/t-wasm-js-3.klib", "sources": null }]
            }]
        });
        targets.push(json!({
            "kind": "native",
            "platforms": ["linux_x64", "mingw_x64"],
            "source-roots": ["/p/src", "/p/src-native"],
            "test-roots": ["/p/tests", "/p/tests-native"],
            "dependencies": [
                { "platform": "native-linux_x64", "compile": [{ "module": "g:a-linuxx64", "version": "1", "path": "/c/a-linuxx64-1.klib" }], "runtime": [], "test": [] },
                { "platform": "native-mingw_x64", "compile": [{ "module": "g:a-mingwx64", "version": "1", "path": "/c/a-mingwx64-1.klib" }], "runtime": [], "test": [] }
            ]
        }));
        metadata["packages"][1]["targets"].as_array_mut().unwrap().push(json!({ "kind": "wasm" }));
        metadata
    }

    fn names(values: &Value, key: &str) -> Vec<String> {
        values.as_array().unwrap().iter().map(|v| v[key].as_str().unwrap().to_owned()).collect()
    }

    fn module<'a>(ws: &'a Value, name: &str) -> &'a Value {
        ws["modules"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap_or_else(|| panic!("no module {name}"))
    }

    fn settings<'a>(ws: &'a Value, name: &str) -> &'a Value {
        ws["kotlinSettings"].as_array().unwrap().iter().find(|s| s["module"] == name).unwrap_or_else(|| panic!("no settings for {name}"))
    }

    fn arguments(settings: &Value) -> (char, Value) {
        let text = settings["compilerArguments"].as_str().unwrap();
        (text.chars().next().unwrap(), serde_json::from_str(&text[1..]).unwrap())
    }

    #[test]
    fn describes_the_source_sets_of_a_multiplatform_package() {
        let ws = workspace_json(&multiplatform("/none"), Targets::Jvm);
        assert_eq!(
            names(&ws["modules"], "name"),
            [
                "app",
                "app.common.main",
                "app.jvm.main",
                "app.common.test",
                "app.jvm.test",
                "lib",
                "lib.common.main",
                "lib.jvm.main",
                "lib.common.test",
                "lib.jvm.test"
            ]
        );

        // src/ is analyzed as JVM code, with the JVM classpath, so the standard library resolves.
        let common = module(&ws, "app.common.main");
        assert_eq!(common["contentRoots"].as_array().unwrap().len(), 1);
        assert_eq!(common["contentRoots"][0]["sourceRoots"][0], json!({ "path": "/p/src", "type": "java-source" }));
        let deps: Vec<String> = common["dependencies"].as_array().unwrap().iter().map(|d| format!("{}:{}", d["type"], d["name"])).collect();
        assert_eq!(
            deps,
            [
                "\"module\":\"lib.jvm.main\"",
                "\"module\":\"lib.common.main\"",
                "\"library\":\"Kargo: g:a:1\"",
                "\"library\":\"Kargo: g:r:2\"",
                "\"moduleSource\":null",
                "\"sdk\":\"21\""
            ]
        );
        let common_settings = settings(&ws, "app.common.main");
        assert_eq!(common_settings["targetPlatform"], "JVM [21]");
        assert_eq!(common_settings["kind"], "source_set_holder");
        let (class, args) = arguments(common_settings);
        assert_eq!(class, 'J');
        assert_eq!(args["multiPlatform"], true, "expect and actual are only allowed in multiplatform code");

        // src-jvm/ refines it and gets the resources.
        let jvm = module(&ws, "app.jvm.main");
        assert_eq!(names(&jvm["contentRoots"], "path"), ["/p/src-jvm", "/p/resources"]);
        assert_eq!(jvm["dependencies"][0], json!({ "type": "module", "name": "app.common.main", "scope": "compile" }));
        assert_eq!(settings(&ws, "app.jvm.main")["dependsOnModuleNames"], json!(["app.common.main"]));

        let test = module(&ws, "app.jvm.test");
        let modules: Vec<&Value> = test["dependencies"].as_array().unwrap().iter().take(3).map(|d| &d["name"]).collect();
        assert_eq!(modules, [&json!("app.jvm.main"), &json!("app.common.test"), &json!("app.common.main")]);
        let test_settings = settings(&ws, "app.jvm.test");
        assert_eq!(test_settings["dependsOnModuleNames"], json!(["app.common.test"]));
        assert_eq!(test_settings["additionalVisibleModuleNames"], json!(["app.jvm.main", "app.common.main"]));
        assert_eq!(settings(&ws, "app.common.test")["additionalVisibleModuleNames"], json!(["app.common.main"]));

        // Only the JVM source set gets the output directories.
        let java: Vec<&Value> = ws["javaSettings"].as_array().unwrap().iter().filter(|j| j["module"] == "app.jvm.main").collect();
        assert_eq!(java[0]["compilerOutputs"], json!(["/p/target/dev/jvm/classes"]));
        let common_java = ws["javaSettings"].as_array().unwrap().iter().find(|j| j["module"] == "app.common.main").unwrap();
        assert!(common_java.get("compilerOutputs").is_none(), "{common_java}");
    }

    #[test]
    fn describes_wasm_and_native_source_sets_with_their_klibs() {
        let home = tempfile::tempdir().unwrap();
        for dir in ["klib/common/stdlib", "klib/platform/linux_x64/posix", "klib/platform/mingw_x64/windows"] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        let ws = workspace_json(&multiplatform(&home.path().display().to_string()), Targets::All);
        let modules = names(&ws["modules"], "name");
        assert_eq!(
            modules[..9],
            [
                "app",
                "app.common.main",
                "app.jvm.main",
                "app.wasm.main",
                "app.native.main",
                "app.common.test",
                "app.jvm.test",
                "app.wasm.test",
                "app.native.test"
            ]
        );
        assert!(modules.contains(&"lib.wasm.main".to_owned()), "{modules:?}");

        let wasm = module(&ws, "app.wasm.main");
        assert_eq!(names(&wasm["contentRoots"], "path"), ["/p/src-wasm"]);
        let deps: Vec<String> = wasm["dependencies"].as_array().unwrap().iter().map(|d| format!("{}:{}", d["type"], d["name"])).collect();
        assert_eq!(
            deps,
            [
                "\"module\":\"app.common.main\"",
                "\"module\":\"lib.wasm.main\"",
                "\"module\":\"lib.common.main\"",
                "\"library\":\"Kargo: g:a-wasm-js:1\"",
                "\"moduleSource\":null",
                "\"sdk\":\"21\""
            ]
        );
        let library = ws["libraries"].as_array().unwrap().iter().find(|l| l["name"] == "Kargo: g:a-wasm-js:1").unwrap();
        assert_eq!(library["type"], "kotlin.wasm.js");
        let wasm_settings = settings(&ws, "app.wasm.main");
        assert_eq!(wasm_settings["targetPlatform"], "Wasm [JS]");
        assert_eq!(wasm_settings["dependsOnModuleNames"], json!(["app.common.main"]));
        let (class, args) = arguments(wasm_settings);
        assert_eq!(class, 'W');
        assert!(args.get("jvmTarget").is_none());
        assert!(!ws["javaSettings"].as_array().unwrap().iter().any(|j| j["module"] == "app.wasm.main"));

        // Native code is analyzed for its first platform, with the distribution's klibs.
        let native = module(&ws, "app.native.main");
        let libraries: Vec<&str> = native["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["type"] == "library")
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(libraries, ["Kargo: g:a-linuxx64:1", "Kotlin/Native 2.4.21: stdlib", "Kotlin/Native 2.4.21: posix [linux_x64]"]);
        let stdlib = ws["libraries"].as_array().unwrap().iter().find(|l| l["name"] == "Kotlin/Native 2.4.21: stdlib").unwrap();
        assert_eq!(stdlib["type"], "kotlin.native");
        assert_eq!(stdlib["roots"][0]["path"], home.path().join("klib").join("common").join("stdlib").display().to_string());
        let (class, _) = arguments(settings(&ws, "app.native.main"));
        assert_eq!(class, 'N');
        assert_eq!(settings(&ws, "app.native.main")["targetPlatform"], "Native [linux_x64]");
    }

    #[test]
    fn common_code_without_a_jvm_target_is_analyzed_for_every_platform() {
        let mut metadata = multiplatform("/none");
        metadata["packages"][0]["targets"].as_array_mut().unwrap().remove(0);
        metadata["packages"].as_array_mut().unwrap().truncate(1);

        // Without --klib-targets, nothing describes the package's code.
        let ws = workspace_json(&metadata, Targets::Jvm);
        assert_eq!(names(&ws["modules"], "name"), ["app"]);

        let ws = workspace_json(&metadata, Targets::All);
        let common = settings(&ws, "app.common.main");
        assert_eq!(common["targetPlatform"], "Native [linux_x64]/Wasm [JS]");
        assert_eq!(arguments(common).0, 'M');
        let libraries =
            module(&ws, "app.common.main")["dependencies"].as_array().unwrap().iter().filter(|d| d["type"] == "library").count();
        assert_eq!(libraries, 0, "the Kotlin LSP has no common variants of libraries");
        assert!(!ws["javaSettings"].as_array().unwrap().iter().any(|j| j["module"] == "app.common.main"));
    }

    #[test]
    fn a_single_wasm_target_gets_one_module_for_its_platform() {
        let mut metadata = multiplatform("/none");
        let targets = metadata["packages"][0]["targets"].as_array_mut().unwrap();
        targets.retain(|t| t["kind"] == "wasm");
        targets[0]["environment"] = json!("wasi");
        metadata["packages"].as_array_mut().unwrap().truncate(1);
        let ws = workspace_json(&metadata, Targets::All);
        assert_eq!(names(&ws["modules"], "name"), ["app", "app.main", "app.test"]);
        assert_eq!(names(&module(&ws, "app.main")["contentRoots"], "path"), ["/p/src", "/p/src-wasm"]);
        let main = settings(&ws, "app.main");
        assert_eq!(main["targetPlatform"], "Wasm [WASI]");
        assert_eq!(main["kind"], "default");
        assert!(arguments(main).1.get("multiPlatform").is_none());
        assert_eq!(ws["libraries"][0]["type"], "kotlin.wasm.wasi");
    }

    #[test]
    fn names_the_description_in_the_fingerprint_record() {
        assert_eq!(description(ServerKind::KotlinLsp, Targets::Jvm), "kotlin-lsp");
        assert_eq!(description(ServerKind::KotlinLsp, Targets::All), "kotlin-lsp+klib");
        assert_eq!(description(ServerKind::Kls, Targets::All), "kls");
    }

    #[test]
    fn lists_the_test_classpath_for_kls() {
        let cp = test_classpath(&sample());
        assert_eq!(cp, [PathBuf::from("/c/a-1.jar"), PathBuf::from("/c/t-3.jar")]);
    }

    #[test]
    fn lists_runnables() {
        let all = runnables(&sample(), "kargo", None);
        let labels: Vec<&str> = all.iter().map(|r| r["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Run app (jvm)", "Test app (jvm)", "Test app (wasm)", "Run app --bin tool", "Run example demo", "Task hello"]);
        assert_eq!(all[0]["args"], json!(["run", "-t", "jvm"]));
        assert_eq!(all[0]["debugArgs"], json!(["run", "-t", "jvm", "--debug"]));
        assert_eq!(all[0]["mainClass"], "a.MainKt");
        assert_eq!(all[0]["cwd"], "/p");
        assert_eq!(all[5]["args"], json!(["task", "hello"]));
        assert_eq!(all[5]["description"], "Say hi");
        assert_eq!(runnables(&sample(), "kargo", Some(Path::new("/p/src/Main.kt"))).len(), 6);
        assert!(runnables(&sample(), "kargo", Some(Path::new("/elsewhere/x.kt"))).is_empty());
    }

    #[test]
    fn leaves_foreign_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut metadata = sample();
        metadata["root"] = json!(root.display().to_string());
        metadata["target-dir"] = json!(root.join("target").display().to_string());

        let written = write(ServerKind::KotlinLsp, &metadata, Targets::Jvm).unwrap();
        assert_eq!(written.changed, [root.join("workspace.json")]);
        assert!(write(ServerKind::KotlinLsp, &metadata, Targets::Jvm).unwrap().changed.is_empty(), "unchanged");
        metadata["packages"][0]["name"] = json!("renamed");
        assert_eq!(write(ServerKind::KotlinLsp, &metadata, Targets::Jvm).unwrap().changed.len(), 1, "Kargo's own file is replaced");

        std::fs::write(root.join("workspace.json"), "{}").unwrap();
        let written = write(ServerKind::KotlinLsp, &metadata, Targets::Jvm).unwrap();
        assert!(written.changed.is_empty());
        assert_eq!(written.foreign, [root.join("workspace.json")]);
        assert_eq!(std::fs::read_to_string(root.join("workspace.json")).unwrap(), "{}");

        let written = write(ServerKind::Kls, &metadata, Targets::Jvm).unwrap();
        let script = root.join(if cfg!(windows) { "kls-classpath.bat" } else { "kls-classpath" });
        assert_eq!(written.changed, std::slice::from_ref(&script));
        assert!(std::fs::read_to_string(&script).unwrap().contains("classpath.txt"));
        let classpath = std::fs::read_to_string(root.join("target/lsp/classpath.txt")).unwrap();
        assert!(classpath.contains("t-3.jar"), "{classpath}");
    }
}
