//! The message loop of Kargo's Kotlin language server.
//!
//! It runs on a thread inside `kargo lsp` and talks LSP over in-memory pipes, so `kargo lsp`
//! treats it like any other Kotlin server behind it (and keeps answering `kargo.toml`,
//! runnables and check on save itself).
//!
//! The project comes from `kargo metadata`: every package's source, test and example roots,
//! split into source sets by target, and its path dependencies, so a file sees exactly the
//! declarations Kargo compiles it with. Files are parsed when the project loads and again
//! whenever they change; the libraries load in the background.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use rayon::prelude::*;
use serde_json::{Value, json};

use kargo_maven::Repository;
use kargo_ops::workspace::Project;
use kargo_util::config::Config;

use super::actions;
use super::compiled;
use super::diagnostics;
use super::features::{self, CompletionOptions, Target};
use super::index::{Index, Origin, Platforms, SetKind, SourceSet, Unit, UnitId};
use super::jdk::Jdk;
use super::library::{self, CompiledLibrary, SourcesJar, UnitCache};
use super::resolve::{Analyzer, Packages};
use super::{editing, hierarchy, inlay, links, ranges, rename, semantic, testing};
use crate::jsonrpc::{self, Kind};
use crate::{check, text};

/// Starts the server on a thread. Returns the stream it writes to and the one it reads from.
pub fn spawn(config: Config) -> (Box<dyn BufRead + Send>, Box<dyn Write + Send>) {
    let (to_server, input) = pipe();
    let (from_server, output) = pipe();
    thread::spawn(move || {
        if let Err(e) = serve(config, BufReader::new(to_server), output) {
            eprintln!("kargo lsp: the Kotlin server stopped: {e:#}");
        }
    });
    (Box::new(BufReader::new(from_server)), Box::new(input))
}

fn pipe() -> (io::PipeReader, io::PipeWriter) {
    io::pipe().expect("failed to create a pipe")
}

/// What the server's loop handles: an editor message, or what a load of the project sent.
/// `Fetching` and `Fetched` begin and end downloading the sources jars of `modules` dependencies,
/// of which `found` had one. `Formatted` is the answer to the `textDocument/formatting` request
/// `id` for the document `uri`, which had the text `original` when it was asked.
enum Event {
    Client(Option<Value>),
    /// An event of the project served by state number `0`, from its workers.
    For(usize, Box<Event>),
    /// A message the project served by state number `0` sends to the editor.
    Out(usize, Value),
    Project {
        generation: u64,
        model: Box<Model>,
        units: Vec<Arc<Unit>>,
    },
    Libraries {
        generation: u64,
        units: Vec<Arc<Unit>>,
        jdk: Option<Arc<Jdk>>,
    },
    Fetching {
        generation: u64,
        modules: usize,
    },
    Fetched {
        generation: u64,
        found: usize,
    },
    Formatted {
        id: Value,
        uri: String,
        original: String,
        outcome: anyhow::Result<Option<String>>,
        /// Asked by the `kargo/formatFile` command: the server applies the edit with
        /// `workspace/applyEdit` and answers the command with `null`.
        apply: bool,
    },
    /// The dependencies of the Kargo script at `path`, declared as `declared`, resolved against the
    /// libraries of generation `generation`: `index` holds those libraries with the script's own, or
    /// is `None` when they could not be resolved, and the script then sees the project's alone.
    Scripted {
        path: PathBuf,
        declared: Vec<String>,
        generation: u64,
        index: Option<Arc<Index>>,
    },
    /// How far a search for references or a rename has come, for the editor's `$/progress`.
    Searching {
        token: Value,
        percentage: u32,
    },
    /// The load number `generation` has nothing more to send.
    Loaded {
        generation: u64,
    },
    /// What a scan of the source roots found: every file now there, and the ones that changed
    /// since the last scan, with their text and modification time; and the TypeScript files of
    /// the JavaScript sources with their modification times.
    Rescanned {
        generation: u64,
        present: HashSet<PathBuf>,
        changed: Vec<(PathBuf, String, SystemTime)>,
        typescript: HashMap<PathBuf, SystemTime>,
    },
    /// The Kotlin declarations of the project's TypeScript modules changed on disk: every file of
    /// them, for the libraries of load number `generation`.
    JsDeclarations {
        generation: u64,
        units: Vec<Arc<Unit>>,
    },
    /// The answer to a references or rename request, which a worker thread computed.
    Searched {
        id: Value,
        generation: u64,
        token: Option<Value>,
        outcome: Result<Value, Failure>,
    },
}

/// How one project's state talks to the loop: its events carry its number, and what it says to the
/// editor goes through the loop, which merges the answers of several projects.
#[derive(Clone)]
struct Tx {
    index: usize,
    tx: Sender<Event>,
}

impl Tx {
    fn send(&self, event: Event) -> Result<(), ()> {
        self.tx.send(Event::For(self.index, Box::new(event))).map_err(|_| ())
    }

    fn out(&self, message: Value) {
        let _ = self.tx.send(Event::Out(self.index, message));
    }
}

/// Formats the text of a Kotlin file; `None` when the file is not to be formatted.
pub(crate) type Formatter = Arc<dyn Fn(&Config, &Path, &str) -> anyhow::Result<Option<String>> + Send + Sync>;

/// The formatter that runs `kargo fmt`'s ktlint ([`kargo_ops::ops::fmt::format_text`]) for the
/// package the file belongs to. A file outside any package is left alone.
pub(crate) fn ktlint_formatter() -> Formatter {
    Arc::new(|config, path, text| {
        let Some(manifest) = check::manifest_for(path) else { return Ok(None) };
        let project = Project::load(&manifest)?;
        // The package with the deepest root above the file, for a workspace or path dependencies.
        let package = (0..project.packages().len())
            .filter(|&i| path.starts_with(project.package(i).root()))
            .max_by_key(|&i| project.package(i).root().components().count());
        let Some(package) = package else { return Ok(None) };
        kargo_ops::ops::fmt::format_text(config, &project, package, path, text)
    })
}

/// The error code of a request that failed because the document changed while it ran.
const CONTENT_MODIFIED: i64 = -32801;
/// The error code of a request that could not be carried out.
const REQUEST_FAILED: i64 = -32803;
/// The error code of a request the editor cancelled.
const REQUEST_CANCELLED: i64 = -32800;

/// A `$/progress` of loading the project or downloading sources jars, once the editor accepted its token.
struct Progress {
    generation: u64,
    token: String,
    create_id: String,
    title: &'static str,
    /// What the editor shows beside the title, updated until the progress begins.
    message: String,
    begun: bool,
}

/// What the server knows about the project's layout.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Source roots and the source set of their files, longest path first.
    pub roots: Vec<(PathBuf, SourceSet)>,
    /// Single files outside the roots (examples, programs with a `path`).
    pub files: Vec<(PathBuf, SourceSet)>,
    pub packages: Packages,
    pub kotlin: String,
    /// The platform of the Wasm target, `js` or `wasi`, whose standard library is loaded.
    pub wasm_platform: Option<String>,
    /// The directories of Kotlin declarations generated for the Wasm target's npm packages,
    /// which load as a library of Wasm code.
    pub npm_declarations: Vec<(PathBuf, Platforms)>,
    /// The directories of the Wasm target's own JavaScript and TypeScript modules (`js/`), and
    /// those of the Kotlin declarations Kargo generates for them, which load like `npm_declarations`
    /// and are generated again when a TypeScript file changes.
    pub js_sources: Vec<PathBuf>,
    pub js_declarations: Vec<(PathBuf, Platforms)>,
    pub repositories: Vec<Repository>,
    /// The `kargo.toml` of each package.
    pub manifests: Vec<PathBuf>,
}

impl Model {
    /// The origin of a file at `path`. A `.kts` script is never a source of the project, even under
    /// a source root: Kargo does not compile scripts, so the script is loose.
    pub fn origin(&self, path: &Path) -> Origin {
        if is_script_path(path) {
            return Origin::Loose;
        }
        if let Some((_, set)) = self.files.iter().find(|(p, _)| p == path) {
            return Origin::Source(set.clone());
        }
        self.roots.iter().find(|(root, _)| path.starts_with(root)).map_or(Origin::Loose, |(_, set)| Origin::Source(set.clone()))
    }
}

/// Whether `path` is a Kotlin script that the server analyzes. Gradle's build scripts
/// (`build.gradle.kts`, `settings.gradle.kts`) belong to the Kotlin plugin, not to Kargo.
pub fn is_script_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "kts") && !path.to_string_lossy().ends_with(".gradle.kts")
}

/// Whether the server analyzes the document at `path`, whose text is `text`: a `.kt` file, or a
/// Kargo script (see [`crate::script::is_kargo_script`]). A project's other `.kts` files are its
/// build scripts, which Kargo does not run.
fn is_kotlin_document(path: &Path, text: &str) -> bool {
    path.extension().is_some_and(|e| e == "kt") || (is_script_path(path) && crate::script::is_kargo_script(path, text))
}

/// The dependencies of a Kargo script and the libraries they resolve to. A script without
/// dependencies has none, and sees the project's libraries alone.
struct ScriptLibraries {
    /// What the script declares (see [`kargo_ops::script::declared_dependencies`]).
    declared: Vec<String>,
    /// The project's libraries with the script's dependencies, once they are resolved.
    index: Option<Arc<Index>>,
}

/// The package a file at `path` belongs in by its directory under its source root: `app.model`
/// for `src/app/model/User.kt`. `None` outside the roots and directly in one.
pub fn directory_package(roots: &[(PathBuf, SourceSet)], path: &Path) -> Option<String> {
    let (root, _) = roots.iter().find(|(root, _)| path.starts_with(root))?;
    let dir = path.parent()?.strip_prefix(root).ok()?;
    let segments: Vec<String> = dir.components().map(|c| features::escape_name(&c.as_os_str().to_string_lossy())).collect();
    (!segments.is_empty()).then(|| segments.join("."))
}

/// The project's layout from its `kargo metadata` (without dependencies).
pub fn model_from_metadata(meta: &Value) -> Model {
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = packages.iter().map(|p| p["name"].as_str().unwrap_or_default()).collect();
    let mut model = Model {
        kotlin: meta["toolchain"]["kotlin"].as_str().unwrap_or(kargo_manifest::DEFAULT_KOTLIN_VERSION).to_owned(),
        ..Model::default()
    };
    let mut direct: Vec<Vec<usize>> = Vec::new();
    for (pi, package) in packages.iter().enumerate() {
        let deps = package["path-dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["package"].as_str())
            .filter_map(|n| names.iter().position(|m| *m == n))
            .collect();
        direct.push(deps);
        model.manifests.push(package["manifest"].as_str().map(PathBuf::from).unwrap_or_default());
        let mut platforms = Platforms::NONE;
        let mut has_res = false;
        for target in package["targets"].as_array().into_iter().flatten() {
            let kind = target["kind"].as_str().unwrap_or_default().to_owned();
            platforms = platforms.union(Platforms::of_target(&kind));
            if kind == "wasm" {
                // `browser` is the `js` platform too.
                model.wasm_platform = Some(if target["environment"] == "wasi" { "wasi" } else { "js" }.to_owned());
            }
            // Declarations Kargo generates for the npm packages and own TypeScript modules of the Wasm and JS targets.
            if kind == "wasm" || kind == "js" {
                let platform = Platforms::of_target(&kind);
                if let Some(dir) = target["npm-declarations"].as_str() {
                    model.npm_declarations.push((PathBuf::from(dir), platform));
                }
                if let Some(dir) = target["js-declarations"].as_str() {
                    model.js_declarations.push((PathBuf::from(dir), platform));
                    for dir in target["js-sources"].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from) {
                        if !model.js_sources.contains(&dir) {
                            model.js_sources.push(dir);
                        }
                    }
                }
            }
            // The output directories of `[[generate]]` steps are in the lists below too; they have no place in
            // their pairs of common and target roots, so they are added on their own.
            let generated: Vec<&Value> = target["generated-roots"].as_array().into_iter().flatten().collect();
            // The directories of intermediate source sets follow the pair in the lists, and get their own sets.
            let groups: Vec<&Value> = target["source-sets"].as_array().into_iter().flatten().collect();
            let is_group_root = |root: &str| {
                groups.iter().any(|g| ["source-root", "test-root", "test-fixture-root"].iter().any(|k| g[*k].as_str() == Some(root)))
            };
            let is_generated = |root: &str| generated.iter().any(|g| g["dir"].as_str() == Some(root)) || is_group_root(root);
            let mut add = |key: &str, set: SetKind| {
                let roots = target[key].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|root| !is_generated(root));
                for (i, root) in roots.enumerate() {
                    // Common roots come first (`src/`, `tests/`), the target's after them.
                    let common = i % 2 == 0;
                    let set = SourceSet { package: pi, kind: set, target: (!common).then(|| kind.clone()) };
                    let root = PathBuf::from(root);
                    if !model.roots.iter().any(|(r, s)| *r == root && *s == set) {
                        model.roots.push((root, set));
                    }
                }
            };
            add("source-roots", SetKind::Main);
            add("test-roots", SetKind::Test);
            for group in &groups {
                let Some(name) = group["name"].as_str() else { continue };
                let targets: Vec<&str> = group["targets"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let platforms: Vec<&str> = group["platforms"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let group_target = SourceSet::group_target(name, &targets, &platforms);
                for (key, set) in [("source-root", SetKind::Main), ("test-root", SetKind::Test), ("test-fixture-root", SetKind::Test)] {
                    let Some(root) = group[key].as_str() else { continue };
                    let set = SourceSet { package: pi, kind: set, target: Some(group_target.clone()) };
                    if !model.roots.iter().any(|(r, s)| r == Path::new(root) && *s == set) {
                        model.roots.push((PathBuf::from(root), set));
                    }
                }
            }
            // The `BuildConfig` AGP generates for an Android module, once Gradle built it.
            for dir in target["android"]["generated-source-roots"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let set = SourceSet { package: pi, kind: SetKind::Main, target: Some(kind.clone()) };
                if !model.roots.iter().any(|(r, s)| r == Path::new(dir) && *s == set) {
                    model.roots.push((PathBuf::from(dir), set));
                }
            }
            for step in &generated {
                let set = match step["kind"].as_str() {
                    Some("sources") => SetKind::Main,
                    Some("test-sources") => SetKind::Test,
                    _ => continue,
                };
                let (Some(dir), common) = (step["dir"].as_str(), step["common"].as_bool().unwrap_or(true)) else { continue };
                let set = SourceSet { package: pi, kind: set, target: (!common).then(|| kind.clone()) };
                if !model.roots.iter().any(|(r, s)| r == Path::new(dir) && *s == set) {
                    model.roots.push((PathBuf::from(dir), set));
                }
            }
            // The generated `Res` class of Compose resources, which code in `src/` uses. Every target has the
            // same one, so the first is enough.
            if let Some(dir) = target["compose-resources"]["source-root"].as_str()
                && !std::mem::replace(&mut has_res, true)
            {
                let set = SourceSet { package: pi, kind: SetKind::Main, target: None };
                model.roots.push((PathBuf::from(dir), set));
            }
        }
        model.packages.platforms.push(platforms);
        for file in package["examples"].as_array().into_iter().flatten().filter_map(|e| e["path"].as_str()) {
            model.files.push((PathBuf::from(file), SourceSet { package: pi, kind: SetKind::Example, target: None }));
        }
        for file in package["bins"].as_array().into_iter().flatten().filter_map(|b| b["path"].as_str()) {
            model.files.push((PathBuf::from(file), SourceSet { package: pi, kind: SetKind::Main, target: None }));
        }
    }
    model.packages.visible = (0..packages.len()).map(|p| transitive(&direct, p)).collect();
    model.roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.as_os_str().len()));
    model
}

fn transitive(direct: &[Vec<usize>], from: usize) -> Vec<usize> {
    let mut out = vec![from];
    let mut i = 0;
    while i < out.len() {
        for &d in direct.get(out[i]).into_iter().flatten() {
            if !out.contains(&d) {
                out.push(d);
            }
        }
        i += 1;
    }
    out
}

/// Kargo's conventional layout at `root`, for a `kargo.toml` that doesn't load.
fn fallback_model(root: &Path) -> Model {
    let mut model = Model { kotlin: kargo_manifest::DEFAULT_KOTLIN_VERSION.to_owned(), ..Model::default() };
    model.packages.visible = vec![vec![0]];
    model.packages.platforms = vec![Platforms::JVM];
    model.manifests = vec![root.join(kargo_manifest::MANIFEST_NAME)];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (kind, target) = match name.as_str() {
                "src" => (SetKind::Main, None),
                "tests" => (SetKind::Test, None),
                "examples" => (SetKind::Example, None),
                n => match (n.strip_prefix("src-"), n.strip_prefix("tests-")) {
                    (Some(t), _) => (SetKind::Main, Some(t.to_owned())),
                    (_, Some(t)) => (SetKind::Test, Some(t.to_owned())),
                    _ => continue,
                },
            };
            model.roots.push((entry.path(), SourceSet { package: 0, kind, target }));
        }
    }
    model.roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.as_os_str().len()));
    model
}

/// The JDK the project builds with, for its sources: the installed JDK that its manifest asks
/// for, else the one on `JAVA_HOME` or the path, else the managed JDK Kargo installed for a
/// machine without one. A missing JDK isn't downloaded here; `kargo build` does that.
fn project_jdk(config: &Config, root: &Path) -> Option<Arc<Jdk>> {
    let manifest = kargo_manifest::Manifest::load(&root.join(kargo_manifest::MANIFEST_NAME)).ok();
    let installed = manifest.as_ref().and_then(|m| kargo_toolchain::jdks::find_installed(config, &m.toolchain.jdk).ok().flatten());
    let (home, version) = match installed {
        Some(installed) => (installed.jdk().home, installed.major),
        None => match kargo_toolchain::Jdk::find().ok()? {
            Some(jdk) => (jdk.home, jdk.version),
            None => {
                let needs = manifest.as_ref().map(kargo_ops::ops::context::jdk_needs).unwrap_or_default();
                let managed = kargo_toolchain::jdks::find_fallback(config, needs).ok()??;
                (managed.home, managed.major)
            }
        },
    };
    Jdk::open(&home, &config.home().join("cache"), &format!("JDK {version}")).map(Arc::new)
}

/// Parses every Kotlin and Java file of the project.
fn scan(model: &Model) -> Vec<Arc<Unit>> {
    let mut paths: Vec<PathBuf> = model.roots.iter().flat_map(|(root, _)| kargo_util::fs::collect_files(root, &["kt", "java"])).collect();
    paths.extend(model.files.iter().map(|(p, _)| p.clone()).filter(|p| p.is_file()));
    paths.sort();
    paths.dedup();
    paths
        .par_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            Some(Arc::new(Unit::new(path.clone(), text, model.origin(path))))
        })
        .collect()
}

/// The sources jars of the dependencies in `meta` (with dependencies) that are in the cache, each
/// with the platforms it is on: main code where a target's compile classpath has it, and test
/// code where only its test classpath has it. The standard library and `kotlin-test` are left to
/// [`library::stdlib_jars`].
fn dependency_jars(meta: &Value) -> Vec<SourcesJar> {
    let mut jars: BTreeMap<PathBuf, SourcesJar> = BTreeMap::new();
    for package in meta["packages"].as_array().into_iter().flatten() {
        for target in package["targets"].as_array().into_iter().flatten() {
            let platforms = Platforms::of_target(target["kind"].as_str().unwrap_or_default());
            for classpaths in target["dependencies"].as_array().into_iter().flatten() {
                let compile: BTreeSet<&str> =
                    classpaths["compile"].as_array().into_iter().flatten().filter_map(|e| e["module"].as_str()).collect();
                for scope in ["compile", "test"] {
                    let main = scope == "compile";
                    for entry in classpaths[scope].as_array().into_iter().flatten() {
                        let (Some(sources), Some(module)) = (entry["sources"].as_str(), entry["module"].as_str()) else { continue };
                        if is_stdlib_module(module) || (!main && compile.contains(module)) {
                            continue;
                        }
                        let jar = jars.entry(PathBuf::from(sources)).or_insert_with(|| {
                            let artifact = module.split_once(':').map_or(module, |(_, artifact)| artifact);
                            let version = entry["version"].as_str().unwrap_or_default();
                            SourcesJar {
                                name: format!("{artifact} {version}"),
                                jar: PathBuf::from(sources),
                                main: Platforms::NONE,
                                test: Platforms::NONE,
                                java: true,
                            }
                        });
                        if main {
                            jar.main = jar.main.union(platforms);
                        } else {
                            jar.test = jar.test.union(platforms);
                        }
                    }
                }
            }
            // Test suites have their own test classpath (such as `testcontainers` for `integration`),
            // which editors need to resolve imports in `integration-tests/`.
            for suite in target["test-suites"].as_array().into_iter().flatten() {
                let compile: BTreeSet<&str> = suite
                    .get("test-dependencies")
                    .and_then(|d| d.as_array())
                    .into_iter()
                    .flatten()
                    .flat_map(|c| c["compile"].as_array().into_iter().flatten())
                    .filter_map(|e| e["module"].as_str())
                    .collect();
                for classpaths in suite["test-dependencies"].as_array().into_iter().flatten() {
                    for entry in classpaths["test"].as_array().into_iter().flatten() {
                        let (Some(sources), Some(module)) = (entry["sources"].as_str(), entry["module"].as_str()) else { continue };
                        if is_stdlib_module(module) || compile.contains(module) {
                            continue;
                        }
                        let jar = jars.entry(PathBuf::from(sources)).or_insert_with(|| {
                            let artifact = module.split_once(':').map_or(module, |(_, artifact)| artifact);
                            let version = entry["version"].as_str().unwrap_or_default();
                            SourcesJar {
                                name: format!("{artifact} {version}"),
                                jar: PathBuf::from(sources),
                                main: Platforms::NONE,
                                test: Platforms::NONE,
                                java: true,
                            }
                        });
                        jar.test = jar.test.union(platforms);
                    }
                }
            }
        }
    }
    jars.into_values().collect()
}

/// Whether `module` is the standard library or `kotlin-test`, whose sources jars
/// [`library::stdlib_jars`] loads for the platforms of the project.
fn is_stdlib_module(module: &str) -> bool {
    let Some(artifact) = module.strip_prefix("org.jetbrains.kotlin:") else { return false };
    artifact == "kotlin-stdlib"
        || artifact == "kotlin-test"
        || artifact.starts_with("kotlin-stdlib-")
        || artifact.starts_with("kotlin-test-wasm-")
        || artifact == "kotlin-test-js"
}

/// The dependencies in `meta` without a sources jar in the cache, whose compiled jars and klibs
/// are read instead, each with the platforms it is on (as for [`dependency_jars`]), and the
/// Kotlin/Native platform libraries.
fn dependency_compiled(meta: &Value) -> Vec<CompiledLibrary> {
    let mut libraries: BTreeMap<PathBuf, CompiledLibrary> = BTreeMap::new();
    for package in meta["packages"].as_array().into_iter().flatten() {
        for target in package["targets"].as_array().into_iter().flatten() {
            let platforms = Platforms::of_target(target["kind"].as_str().unwrap_or_default());
            for classpaths in target["dependencies"].as_array().into_iter().flatten() {
                let compile: BTreeSet<&str> =
                    classpaths["compile"].as_array().into_iter().flatten().filter_map(|e| e["module"].as_str()).collect();
                for scope in ["compile", "test"] {
                    let main = scope == "compile";
                    for entry in classpaths[scope].as_array().into_iter().flatten() {
                        let (Some(path), Some(module)) = (entry["path"].as_str(), entry["module"].as_str()) else { continue };
                        if is_stdlib_module(module) || (!main && compile.contains(module)) {
                            continue;
                        }
                        let path = PathBuf::from(path);
                        if !is_library_file(&path) {
                            continue;
                        }
                        // The cinterop klib of a module comes with the module's sources jar, which
                        // never has the declarations cinterop generated.
                        let has_sources = !entry["sources"].is_null();
                        if has_sources
                            && !libraries.contains_key(&path)
                            && !(path.extension().is_some_and(|e| e == "klib") && compiled::is_interop_klib(&path))
                        {
                            continue;
                        }
                        let library = libraries.entry(path.clone()).or_insert_with(|| {
                            let artifact = module.split_once(':').map_or(module, |(_, artifact)| artifact);
                            let version = entry["version"].as_str().unwrap_or_default();
                            let name =
                                if has_sources { format!("{artifact} {version} (cinterop)") } else { format!("{artifact} {version}") };
                            let javadoc = entry["javadoc"].as_str().map(PathBuf::from);
                            CompiledLibrary { name, path, main: Platforms::NONE, test: Platforms::NONE, known: None, javadoc }
                        });
                        if main {
                            library.main = library.main.union(platforms);
                        } else {
                            library.test = library.test.union(platforms);
                        }
                    }
                }
            }
            for suite in target["test-suites"].as_array().into_iter().flatten() {
                for classpaths in suite["test-dependencies"].as_array().into_iter().flatten() {
                    for entry in classpaths["test"].as_array().into_iter().flatten() {
                        let (Some(path), Some(module)) = (entry["path"].as_str(), entry["module"].as_str()) else { continue };
                        if is_stdlib_module(module) {
                            continue;
                        }
                        let path = PathBuf::from(path);
                        if !is_library_file(&path) {
                            continue;
                        }
                        let has_sources = !entry["sources"].is_null();
                        if has_sources
                            && !libraries.contains_key(&path)
                            && !(path.extension().is_some_and(|e| e == "klib") && compiled::is_interop_klib(&path))
                        {
                            continue;
                        }
                        let library = libraries.entry(path.clone()).or_insert_with(|| {
                            let artifact = module.split_once(':').map_or(module, |(_, artifact)| artifact);
                            let version = entry["version"].as_str().unwrap_or_default();
                            let name =
                                if has_sources { format!("{artifact} {version} (cinterop)") } else { format!("{artifact} {version}") };
                            let javadoc = entry["javadoc"].as_str().map(PathBuf::from);
                            CompiledLibrary { name, path, main: Platforms::NONE, test: Platforms::NONE, known: None, javadoc }
                        });
                        library.test = library.test.union(platforms);
                    }
                }
            }
        }
    }
    let mut out: Vec<CompiledLibrary> = libraries.into_values().collect();
    out.extend(platform_libraries(meta));
    out.extend(android_platforms(meta));
    out
}

/// Whether `path` is a compiled library the server reads: a jar, a klib, or an Android library (`.aar`),
/// whose `classes.jar` is read.
fn is_library_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jar" || e == "klib" || e == "aar")
}

/// The `android.jar` of the platform each Android target compiles against (`platform-jar` of its
/// `android` object), the way the JDK is for JVM code: Android code gets `android.*` from it.
fn android_platforms(meta: &Value) -> Vec<CompiledLibrary> {
    let mut jars: BTreeMap<PathBuf, CompiledLibrary> = BTreeMap::new();
    for target in meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["targets"].as_array().into_iter().flatten())
        .filter(|t| t["kind"] == "android")
    {
        let Some(jar) = target["android"]["platform-jar"].as_str().map(PathBuf::from) else { continue };
        // `<sdk>/platforms/android-36/android.jar`
        let platform = jar.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        jars.entry(jar.clone()).or_insert_with(|| CompiledLibrary {
            name: format!("Android SDK ({platform})"),
            path: jar,
            main: Platforms::JVM,
            test: Platforms::JVM,
            known: None,
            javadoc: None,
        });
    }
    jars.into_values().collect()
}

/// The Kotlin/Native platform libraries (`posix`, `zlib`, ...) of the first Native platform in
/// `meta`, from the distribution that builds the project, for Native code.
fn platform_libraries(meta: &Value) -> Vec<CompiledLibrary> {
    let Some(home) = meta["toolchain"]["kotlin-native"]["home"].as_str() else { return Vec::new() };
    let platform = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["targets"].as_array().into_iter().flatten())
        .filter(|t| t["kind"] == "native")
        .find_map(|t| t["platforms"].as_array().and_then(|p| p.first()).and_then(Value::as_str));
    let Some(platform) = platform else { return Vec::new() };
    let dir = Path::new(home).join("klib").join("platform").join(platform);
    let mut out: Vec<CompiledLibrary> = std::fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            let lib = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let short = lib.rsplit('.').next().unwrap_or(&lib).to_owned();
            CompiledLibrary {
                name: format!("Kotlin/Native {short} ({platform})"),
                path,
                main: Platforms::NATIVE,
                test: Platforms::NONE,
                known: None,
                javadoc: None,
            }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The Kotlin/Native standard library's klib in the distribution that builds the project, for
/// what its sources jar doesn't give Native code (`kotlinx.cinterop`, `kotlin.native`, ...):
/// `stdlib` are the standard library files already loaded, whose top-level declarations on
/// Native are left out of it.
fn native_stdlib(meta: &Value, stdlib: &[Arc<Unit>]) -> Option<CompiledLibrary> {
    let home = meta["toolchain"]["kotlin-native"]["home"].as_str()?;
    let path = Path::new(home).join("klib").join("common").join("stdlib");
    if !path.is_dir() {
        return None;
    }
    let mut known = BTreeSet::new();
    for unit in stdlib {
        let Origin::Library { main, .. } = &unit.origin else { continue };
        if !main.contains(Platforms::NATIVE) {
            continue;
        }
        let file = &unit.file;
        for &id in &file.top {
            let decl = &file.decls[id];
            if !decl.is_private() && !decl.is_internal() {
                known.insert(if file.package.is_empty() { decl.name.clone() } else { format!("{}.{}", file.package, decl.name) });
            }
        }
    }
    Some(CompiledLibrary {
        name: "Kotlin/Native stdlib".into(),
        path,
        main: Platforms::NATIVE,
        test: Platforms::NONE,
        known: Some(Arc::new(known)),
        javadoc: None,
    })
}

/// The dependencies in `meta` without a sources jar or a Javadoc jar in the cache, as
/// `group:artifact` and version: the ones [`dependency_compiled`] reads from their compiled jars
/// and klibs with no documentation.
fn missing_sources(meta: &Value) -> Vec<(String, String)> {
    let mut out: BTreeSet<(String, String)> = BTreeSet::new();
    for package in meta["packages"].as_array().into_iter().flatten() {
        for target in package["targets"].as_array().into_iter().flatten() {
            for classpaths in target["dependencies"].as_array().into_iter().flatten() {
                for entry in ["compile", "test"].iter().flat_map(|scope| classpaths[*scope].as_array().into_iter().flatten()) {
                    let (Some(path), Some(module), Some(version)) =
                        (entry["path"].as_str(), entry["module"].as_str(), entry["version"].as_str())
                    else {
                        continue;
                    };
                    if entry["sources"].is_null()
                        && entry["javadoc"].is_null()
                        && !is_stdlib_module(module)
                        && is_library_file(Path::new(path))
                    {
                        out.insert((module.to_owned(), version.to_owned()));
                    }
                }
            }
        }
    }
    out.into_iter().collect()
}

/// What a load of the project does besides reading it.
#[derive(Clone)]
struct LoadOptions {
    /// Whether the dependencies are loaded, not only the standard library.
    dependencies: bool,
    /// Whether missing sources jars of dependencies are downloaded.
    fetch_sources: bool,
    /// The modules whose sources jars were looked for already, found or not, so a reload
    /// doesn't ask the repositories again.
    tried: Arc<std::sync::Mutex<BTreeSet<(String, String)>>>,
}

/// Loads the project at `root` and its libraries, sending each as it is ready. Dependencies
/// without a sources jar in the cache are read from their compiled jars first; then their
/// sources jars, or Javadoc jars where they have none, are downloaded (as `kargo fetch --sources`
/// does) and the libraries load again.
fn load(config: Config, root: PathBuf, generation: u64, tx: Tx, cache: UnitCache, options: LoadOptions) {
    load_parts(config, root, generation, &tx, cache, options);
    let _ = tx.send(Event::Loaded { generation });
}

fn load_parts(config: Config, root: PathBuf, generation: u64, tx: &Tx, cache: UnitCache, options: LoadOptions) {
    let model = match crate::project::metadata(&config, &root, true) {
        Ok(meta) => model_from_metadata(&meta),
        Err(_) => fallback_model(&root),
    };
    let model = Model {
        repositories: crate::deps::repositories(
            &read(&root.join(kargo_manifest::MANIFEST_NAME)),
            &root.join(kargo_manifest::MANIFEST_NAME),
        ),
        ..model
    };
    let units = scan(&model);
    if tx.send(Event::Project { generation, model: Box::new(model.clone()), units }).is_err() {
        return;
    }
    let platforms = model.packages.platforms.iter().fold(Platforms::NONE, |all, p| all.union(*p));
    let stdlib = library::stdlib_jars(&config, &model.kotlin, model.repositories.clone(), platforms, model.wasm_platform.as_deref());
    // Without the standard library's sources, the built-in outline stands in for it.
    let outline = !stdlib.iter().any(|j| j.name.starts_with("kotlin-stdlib"));
    let libraries = |meta: Option<&Value>| {
        // Parsed together with the standard library, so a file both carry is one unit.
        let mut jars = stdlib.clone();
        jars.extend(meta.map(dependency_jars).unwrap_or_default());
        let mut units = library::units(&config, &cache, &jars);
        let mut outline_units = if outline { library::stub_units(&config) } else { Vec::new() };
        if let Some(meta) = meta {
            let mut compiled = dependency_compiled(meta);
            if platforms.contains(Platforms::NATIVE) {
                // Only the standard library's own files say what the Native one adds.
                let mut known = library::units(&config, &cache, &stdlib);
                known.extend(outline_units.iter().cloned());
                compiled.extend(native_stdlib(meta, &known));
            }
            units.extend(library::compiled_units(&config, &cache, &compiled));
        }
        units.append(&mut outline_units);
        for (dir, platform) in &model.npm_declarations {
            units.extend(library::directory_units("npm", dir, *platform, Platforms::NONE));
        }
        for (dir, platform) in &model.js_declarations {
            units.extend(library::directory_units("js", dir, *platform, Platforms::NONE));
        }
        units
    };
    // The declarations of the project's own TypeScript modules need no install: written before the first libraries.
    if !model.js_declarations.is_empty() {
        let _ = crate::project::js_declarations(&root);
    }
    let jdk = project_jdk(&config, &root);
    let send = |units| tx.send(Event::Libraries { generation, units, jdk: jdk.clone() }).is_ok();
    if !send(libraries(None)) || !options.dependencies {
        return;
    }
    // Like the build, installs the npm packages and writes their declarations if they changed.
    if !model.npm_declarations.is_empty() && crate::project::npm_declarations(&config, &root).is_ok() && !send(libraries(None)) {
        return;
    }
    let Ok(meta) = crate::project::metadata(&config, &root, false) else { return };
    if (!dependency_jars(&meta).is_empty() || !dependency_compiled(&meta).is_empty()) && !send(libraries(Some(&meta))) {
        return;
    }
    if !options.fetch_sources {
        return;
    }
    let missing: Vec<(String, String)> = {
        let tried = options.tried.lock().expect("lock poisoned");
        missing_sources(&meta).into_iter().filter(|m| !tried.contains(m)).collect()
    };
    if missing.is_empty() || tx.send(Event::Fetching { generation, modules: missing.len() }).is_err() {
        return;
    }
    let results = crate::project::fetch_sources(&config, &root, &missing).unwrap_or_default();
    let mut found = 0;
    {
        let mut tried = options.tried.lock().expect("lock poisoned");
        for (module, result) in missing.into_iter().zip(results) {
            // A failed download is tried again on the next load.
            if let Ok(fetched) = result {
                found += usize::from(fetched);
                tried.insert(module);
            }
        }
    }
    if tx.send(Event::Fetched { generation, found }).is_err() || found == 0 {
        return;
    }
    if let Ok(meta) = crate::project::metadata(&config, &root, false) {
        send(libraries(Some(&meta)));
    }
}

/// The path of a `file:` URI, with the drive letter in upper case on Windows (editors send
/// `c%3A`, Kargo's own paths have `C:`), so the same file has one path.
fn file_path(uri: &str) -> Option<PathBuf> {
    let path = text::uri_to_path(uri)?;
    let text = path.to_string_lossy();
    match text.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_lowercase() => {
            Some(PathBuf::from(format!("{}{}", drive.to_ascii_uppercase() as char, &text[1..])))
        }
        _ => Some(path),
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Runs the server until the stream closes or `exit` arrives.
pub fn serve(config: Config, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> anyhow::Result<()> {
    serve_with(config, input, output, true, ktlint_formatter())
}

pub(crate) fn serve_with(
    config: Config,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    dependencies: bool,
    formatter: Formatter,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    let client_tx = tx.clone();
    thread::spawn(move || {
        let mut input = input;
        loop {
            let message = jsonrpc::read(&mut input).ok().flatten();
            let end = message.is_none();
            if client_tx.send(Event::Client(message)).is_err() || end {
                break;
            }
        }
    });
    let mut hub = Hub {
        config,
        tx,
        rx,
        out: Box::new(output),
        dependencies,
        formatter,
        states: Vec::new(),
        init: Value::Null,
        queued: VecDeque::new(),
        parents: HashMap::new(),
        subs: HashMap::new(),
        last_completion: 0,
    };
    loop {
        hub.poll_due();
        let event = match hub.queued.pop_front() {
            Some(message) => Event::Client(message),
            None => match hub.rx.recv_timeout(hub.poll_wait()) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };
        if !hub.handle(event)? {
            break;
        }
    }
    Ok(())
}

/// A `workspace/symbol` request asked of every project, until all have answered.
struct FanOut {
    id: Value,
    remaining: usize,
    symbols: Vec<Value>,
    failure: Option<Value>,
    /// The ids the projects were asked with.
    subs: Vec<Value>,
}

/// The server: one [`State`] per project of the editor's workspace folders. Messages about a file
/// go to the project it is in; the editor's other messages go to the first one or to all of them.
struct Hub {
    config: Config,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    out: Box<dyn Write + Send>,
    dependencies: bool,
    formatter: Formatter,
    /// The projects, by number; a closed folder leaves its slot empty, so the numbers in ids hold.
    states: Vec<Option<State>>,
    /// The parameters of `initialize`, for the folders that open later.
    init: Value,
    /// Editor messages that arrived while waiting for a load.
    queued: VecDeque<Option<Value>>,
    /// The symbol searches in flight, by the editor's request id.
    parents: HashMap<String, FanOut>,
    /// The project and parent of each request a symbol search sent, by its id.
    subs: HashMap<String, (String, usize)>,
    /// The project that answered the last completion, which resolves its items.
    last_completion: usize,
}

impl Hub {
    fn handle(&mut self, event: Event) -> anyhow::Result<bool> {
        match event {
            Event::Client(None) => return Ok(false),
            Event::Client(Some(message)) => return self.client(message),
            Event::For(index, event) => {
                if let Some(state) = self.state(index) {
                    state.loaded(*event);
                }
            }
            Event::Out(index, message) => self.out_message(index, message)?,
            _ => {}
        }
        Ok(true)
    }

    fn state(&mut self, index: usize) -> Option<&mut State> {
        self.states.get_mut(index).and_then(Option::as_mut)
    }

    fn live(&self) -> Vec<usize> {
        self.states.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).collect()
    }

    fn write(&mut self, message: &Value) -> anyhow::Result<()> {
        jsonrpc::write(&mut self.out, message).map_err(Into::into)
    }

    fn poll_due(&mut self) {
        for state in self.states.iter_mut().flatten() {
            state.poll_due();
        }
    }

    fn poll_wait(&self) -> Duration {
        self.states.iter().flatten().map(State::poll_wait).min().unwrap_or(Duration::from_secs(3600))
    }

    /// The project a file belongs to: the one whose root is the deepest above it, else the first.
    fn route(&self, uri: &str) -> usize {
        let path = file_path(uri);
        let deepest = self.states.iter().enumerate().filter_map(|(i, state)| {
            let root = state.as_ref()?.root.as_ref()?;
            path.as_ref().filter(|p| p.starts_with(root)).map(|_| (root.components().count(), i))
        });
        deepest.max().map(|(_, i)| i).or_else(|| self.live().first().copied()).unwrap_or(0)
    }

    /// Waits a while for the project and the standard library, the first time they load, so
    /// the first requests see them. Editor messages that arrive meanwhile are queued.
    fn wait_for_load(&mut self, index: usize) {
        let start = Instant::now();
        loop {
            let Some(state) = self.state(index) else { return };
            if state.root.is_none() || (state.project_loaded && state.libraries_loaded) {
                return;
            }
            let limit = if state.project_loaded { LIBRARY_WAIT } else { PROJECT_WAIT };
            let Some(remaining) = limit.checked_sub(start.elapsed()) else { return };
            match self.rx.recv_timeout(remaining) {
                Ok(Event::Client(message)) => self.queued.push_back(message),
                Ok(other) => {
                    let _ = self.handle(other);
                }
                Err(_) => return,
            }
        }
    }

    /// Writes what a project said to the editor, or takes it in when it answers a symbol search.
    fn out_message(&mut self, index: usize, mut message: Value) -> anyhow::Result<()> {
        if jsonrpc::kind(&message) != Kind::Response {
            return self.write(&message);
        }
        let Some((parent_key, _)) = self.subs.remove(&message["id"].to_string()) else { return self.write(&message) };
        let Some(parent) = self.parents.get_mut(&parent_key) else { return Ok(()) };
        match message["result"].as_array_mut() {
            Some(symbols) => {
                for symbol in symbols.iter_mut() {
                    if symbol["data"].is_object() {
                        symbol["data"]["project"] = json!(index);
                    }
                }
                parent.symbols.append(symbols);
            }
            None => parent.failure = Some(message),
        }
        parent.remaining -= 1;
        if parent.remaining > 0 {
            return Ok(());
        }
        let parent = self.parents.remove(&parent_key).expect("a symbol search");
        let reply = match parent.failure {
            Some(mut failure) if parent.symbols.is_empty() => {
                failure["id"] = parent.id;
                failure
            }
            _ => jsonrpc::response(&parent.id, json!(parent.symbols.into_iter().take(500).collect::<Vec<_>>())),
        };
        self.write(&reply)
    }

    fn client(&mut self, message: Value) -> anyhow::Result<bool> {
        let method = jsonrpc::method(&message).to_owned();
        match jsonrpc::kind(&message) {
            Kind::Response => {
                // The editor answers the requests of project `n` under the id `kotlin/n/...`.
                let index = message["id"].as_str().and_then(|id| id.split('/').nth(1)).and_then(|n| n.parse::<usize>().ok());
                if let Some(state) = index.and_then(|i| self.state(i)) {
                    state.message(message)?;
                }
            }
            Kind::Request => match method.as_str() {
                "initialize" => self.initialize(&message)?,
                "shutdown" => self.write(&jsonrpc::response(&message["id"], Value::Null))?,
                _ => self.request(message)?,
            },
            Kind::Notification => return self.notification(&method, message),
        }
        Ok(true)
    }

    /// Opens a project for each workspace folder (or the one root), and answers with the
    /// capabilities of the first.
    fn initialize(&mut self, message: &Value) -> anyhow::Result<()> {
        let params = message["params"].clone();
        self.init = params.clone();
        let folders = folder_roots(&params["workspaceFolders"]);
        let mut result = if folders.len() > 1 {
            let mut first = None;
            for root in &folders {
                let result = self.open_folder(root);
                first.get_or_insert(result);
            }
            first.unwrap_or(Value::Null)
        } else {
            self.add_state(&params)
        };
        result["capabilities"]["workspace"]["workspaceFolders"] = json!({ "supported": true, "changeNotifications": true });
        self.write(&jsonrpc::response(&message["id"], result))
    }

    /// Starts the state of the project at `root`; returns its `initialize` result.
    fn open_folder(&mut self, root: &Path) -> Value {
        let uri = text::path_to_uri(root);
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut params = self.init.clone();
        params["rootUri"] = json!(uri);
        params["rootPath"] = Value::Null;
        params["workspaceFolders"] = json!([{ "uri": uri, "name": name }]);
        self.add_state(&params)
    }

    fn add_state(&mut self, params: &Value) -> Value {
        let index = self.states.len();
        let tx = Tx { index, tx: self.tx.clone() };
        let mut state = State::new(self.config.clone(), tx, index, self.dependencies, Arc::clone(&self.formatter));
        let result = state.initialize(params);
        self.states.push(Some(state));
        result
    }

    /// The editor opened or closed workspace folders.
    fn folders_changed(&mut self, params: &Value) -> anyhow::Result<()> {
        for root in folder_roots(&params["event"]["removed"]) {
            let found = self.states.iter().position(|s| s.as_ref().is_some_and(|s| s.root.as_deref() == Some(root.as_path())));
            if let Some(state) = found.and_then(|i| self.states[i].take()) {
                for uri in state.open.keys() {
                    self.write(&jsonrpc::notification("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] })))?;
                }
            }
        }
        for root in folder_roots(&params["event"]["added"]) {
            if !self.states.iter().flatten().any(|s| s.root.as_deref() == Some(root.as_path())) {
                self.open_folder(&root);
            }
        }
        Ok(())
    }

    fn request(&mut self, message: Value) -> anyhow::Result<()> {
        let method = jsonrpc::method(&message).to_owned();
        let params = &message["params"];
        if method == "workspace/symbol" && self.live().len() > 1 {
            return self.search_symbols(message);
        }
        let uri = [&params["textDocument"]["uri"], &params["item"]["uri"], &params["files"][0]["oldUri"], &params["arguments"][0]["uri"]]
            .into_iter()
            .find_map(Value::as_str)
            .map(str::to_owned);
        let first = self.live().first().copied().unwrap_or(0);
        let index = match method.as_str() {
            "completionItem/resolve" => self.last_completion,
            "workspaceSymbol/resolve" => params["data"]["project"].as_u64().map_or(first, |n| n as usize),
            _ => uri.map_or(first, |uri| self.route(&uri)),
        };
        if method == "textDocument/completion" {
            self.last_completion = index;
        }
        self.wait_for_load(index);
        match self.state(index) {
            Some(state) => {
                state.message(message)?;
                Ok(())
            }
            None => self.write(&jsonrpc::response(&message["id"], Value::Null)),
        }
    }

    /// Asks every project for its symbols; the answers are merged as they arrive.
    fn search_symbols(&mut self, message: Value) -> anyhow::Result<()> {
        let key = message["id"].to_string();
        let live = self.live();
        let parent = FanOut { id: message["id"].clone(), remaining: live.len(), symbols: Vec::new(), failure: None, subs: Vec::new() };
        self.parents.insert(key.clone(), parent);
        for index in live {
            self.wait_for_load(index);
            let sub = json!(format!("{key}#{index}"));
            self.subs.insert(sub.to_string(), (key.clone(), index));
            if let Some(parent) = self.parents.get_mut(&key) {
                parent.subs.push(sub.clone());
            }
            let mut message = message.clone();
            message["id"] = sub;
            // One bar for every project would be confusing.
            if let Some(params) = message["params"].as_object_mut() {
                params.remove("workDoneToken");
            }
            if let Some(state) = self.state(index) {
                state.message(message)?;
            }
        }
        Ok(())
    }

    fn notification(&mut self, method: &str, message: Value) -> anyhow::Result<bool> {
        let params = &message["params"];
        match method {
            "exit" => return Ok(false),
            "initialized" => {
                // One registration of the watchers serves every project.
                let first = self.live().first().copied();
                if let Some(state) = first.and_then(|i| self.state(i)) {
                    state.message(message)?;
                }
            }
            "$/cancelRequest" => {
                let subs = self.parents.get(&params["id"].to_string()).map(|p| p.subs.clone()).unwrap_or_default();
                for state in self.states.iter_mut().flatten() {
                    state.message(message.clone())?;
                }
                for sub in subs {
                    let cancel = jsonrpc::notification("$/cancelRequest", json!({ "id": sub }));
                    for state in self.states.iter_mut().flatten() {
                        state.message(cancel.clone())?;
                    }
                }
            }
            "workspace/didChangeWorkspaceFolders" => self.folders_changed(params)?,
            // A renamed file may move between projects; each takes out and reads what is its own.
            "workspace/didRenameFiles" | "workspace/didDeleteFiles" => {
                for state in self.states.iter_mut().flatten() {
                    state.message(message.clone())?;
                }
            }
            "workspace/didChangeWatchedFiles" => {
                let mut by_project: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
                for change in params["changes"].as_array().into_iter().flatten() {
                    let index = self.route(change["uri"].as_str().unwrap_or_default());
                    by_project.entry(index).or_default().push(change.clone());
                }
                for (index, changes) in by_project {
                    if let Some(state) = self.state(index) {
                        state.message(jsonrpc::notification(method, json!({ "changes": changes })))?;
                    }
                }
            }
            _ => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    let index = self.route(uri);
                    if let Some(state) = self.state(index) {
                        state.message(message)?;
                    }
                }
            }
        }
        Ok(true)
    }
}

/// The project roots of workspace `folders` (`{ uri, name }`), each at the root of its Kargo project.
fn folder_roots(folders: &Value) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in folders.as_array().into_iter().flatten().filter_map(|f| f["uri"].as_str()).filter_map(file_path) {
        let root = crate::project::project_root(&dir).unwrap_or(dir);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// How long the first requests wait for the project's files and for the standard library.
const PROJECT_WAIT: Duration = Duration::from_secs(30);
const LIBRARY_WAIT: Duration = Duration::from_secs(5);
/// How often the source roots are scanned for new, changed and deleted files when the editor
/// does not watch them.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

struct State {
    config: Config,
    tx: Tx,
    /// The number of this project among the server's, in the ids of its requests.
    index: usize,
    root: Option<PathBuf>,
    model: Model,
    /// Every project file, by path; open documents hold the editor's text.
    files: HashMap<PathBuf, Arc<Unit>>,
    /// Open documents: URI to path.
    open: HashMap<String, PathBuf>,
    lib: Arc<Index>,
    /// Bumped whenever `lib` is replaced, so the scripts' libraries built on the old one are dropped.
    libraries_generation: u64,
    /// The open Kargo scripts that declare dependencies, by path.
    scripts: HashMap<PathBuf, ScriptLibraries>,
    proj: Option<Snapshot>,
    /// The classified identifiers of each file (see [`semantic`]) at the generation they were computed.
    classified: HashMap<PathBuf, (u64, Arc<Vec<semantic::Classified>>)>,
    /// Which load of the project is current.
    load_generation: u64,
    /// Bumped on every change, so stale completion items are not resolved.
    generation: u64,
    snippets: bool,
    /// Names the editor inserted in one edit lately (a chosen completion, a paste), by when, so
    /// completion offers them first.
    recent: HashMap<String, u64>,
    clock: u64,
    /// The last completion answered, to give it again for the same request.
    completions: Option<(CompletionKey, Value)>,
    watch: bool,
    /// Whether the editor takes `documentChanges` in workspace edits, and can create files there.
    document_changes: bool,
    create_files: bool,
    /// Whether the editor takes `rename` operations in workspace edits.
    rename_files: bool,
    /// `initializationOptions`: rename the file of a renamed class, move the directory of a
    /// renamed package.
    rename_option: bool,
    move_option: bool,
    /// `initializationOptions`: the kinds of inlay hints shown.
    inlay_options: inlay::Options,
    /// `initializationOptions`: whether unused code and deprecated usages are reported.
    diagnostic_options: diagnostics::Options,
    /// The version of each open document, as the editor sent it.
    versions: HashMap<PathBuf, i64>,
    /// Whether the editor shows `$/progress`.
    work_done_progress: bool,
    load_options: LoadOptions,
    progress: Option<Progress>,
    /// The progress of the current load.
    loading: Option<Progress>,
    cache: UnitCache,
    next_id: u64,
    /// Whether the current load delivered the project's files, and the libraries.
    project_loaded: bool,
    libraries_loaded: bool,
    formatter: Formatter,
    /// The searches running on worker threads, by request id, and whether the editor cancelled them.
    searches: HashMap<String, Arc<AtomicBool>>,
    /// Whether `workspace/symbol` results may leave out ranges for `workspaceSymbol/resolve`.
    symbol_resolve: bool,
    /// Whether the editor applies `workspace/applyEdit`, which the `kargo/formatFile` command needs.
    apply_edit: bool,
    /// `initializationOptions.status`: send `kargo/status` notifications.
    status: bool,
    /// The modification time of each file the last scans read, so a scan reads only changed files.
    stamps: HashMap<PathBuf, SystemTime>,
    /// The TypeScript files of the JavaScript sources the last scan found, with their modification times.
    typescript_stamps: HashMap<PathBuf, SystemTime>,
    rescanning: bool,
    next_poll: Instant,
}

/// What a completion answer depends on: the document, the position, what triggered it and the
/// state of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompletionKey {
    uri: String,
    offset: usize,
    trigger: Option<String>,
    generation: u64,
    snippets: bool,
}

/// The most names remembered as recently used.
const RECENT_NAMES: usize = 256;

/// The project index and the unit number of each file in it.
type Snapshot = (Arc<Index>, Arc<HashMap<PathBuf, u32>>);

impl State {
    fn new(config: Config, tx: Tx, index: usize, dependencies: bool, formatter: Formatter) -> Self {
        Self {
            config,
            tx,
            index,
            root: None,
            model: Model::default(),
            files: HashMap::new(),
            open: HashMap::new(),
            lib: Arc::new(Index::new(true, Vec::new())),
            libraries_generation: 0,
            scripts: HashMap::new(),
            proj: None,
            classified: HashMap::new(),
            load_generation: 0,
            generation: 0,
            snippets: false,
            recent: HashMap::new(),
            clock: 0,
            completions: None,
            watch: false,
            document_changes: false,
            create_files: false,
            rename_files: false,
            rename_option: true,
            move_option: true,
            inlay_options: inlay::Options::default(),
            diagnostic_options: diagnostics::Options::default(),
            versions: HashMap::new(),
            work_done_progress: false,
            load_options: LoadOptions { dependencies, fetch_sources: true, tried: Arc::default() },
            progress: None,
            loading: None,
            cache: UnitCache::default(),
            next_id: 0,
            project_loaded: false,
            libraries_loaded: false,
            formatter,
            searches: HashMap::new(),
            symbol_resolve: false,
            apply_edit: false,
            status: false,
            stamps: HashMap::new(),
            typescript_stamps: HashMap::new(),
            rescanning: false,
            next_poll: Instant::now() + RESCAN_INTERVAL,
        }
    }

    fn send(&mut self, message: &Value) -> anyhow::Result<()> {
        self.tx.out(message.clone());
        Ok(())
    }

    fn loaded(&mut self, event: Event) {
        match event {
            Event::Project { generation, model, units } if generation == self.load_generation => {
                self.report_loading("Indexing the standard library");
                self.project_loaded = true;
                self.model = *model;
                let open: HashMap<PathBuf, Arc<Unit>> =
                    self.open.values().filter_map(|p| self.files.get(p).map(|u| (p.clone(), Arc::clone(u)))).collect();
                self.files = units.into_iter().map(|u| (u.path.clone(), u)).collect();
                for (path, unit) in open {
                    let origin = self.model.origin(&path);
                    self.files.insert(path.clone(), Arc::new(Unit::new(path, unit.text.clone(), origin)));
                }
                self.changed();
                self.republish_diagnostics();
            }
            Event::Libraries { generation, units, jdk } if generation == self.load_generation => {
                self.libraries_loaded = true;
                self.lib = Arc::new(Index::new(true, units).with_jdk(jdk));
                self.libraries_generation += 1;
                self.resolve_scripts();
                self.changed();
                // A file opened before its libraries loaded was checked without them.
                // A file opened before its libraries loaded was checked without them.
                self.republish_diagnostics();
                self.report_loading("Indexing the dependencies");
            }
            Event::Scripted { path, declared, generation, index } => {
                // Dropped when the script changed its dependencies or the libraries changed since.
                let current = self.scripts.get_mut(&path).filter(|s| s.declared == declared);
                if let Some(script) = current.filter(|_| generation == self.libraries_generation) {
                    script.index = index;
                    self.generation += 1;
                }
            }
            Event::Loaded { generation } if generation == self.load_generation => {
                self.end_loading();
                self.status("indexing", false);
            }
            Event::Fetching { generation, modules } => {
                self.end_loading();
                self.end_progress(None);
                let message = format!("{modules} librar{}", if modules == 1 { "y" } else { "ies" });
                self.progress = self.start_progress(generation, "sources", "Fetching dependency sources", message);
            }
            Event::Fetched { generation, found } if self.progress.as_ref().is_some_and(|p| p.generation == generation) => {
                self.end_progress(Some(format!("{found} sources or Javadoc jar{}", if found == 1 { "" } else { "s" })));
            }
            Event::Formatted { id, uri, original, outcome, apply } => {
                let current = self.open.get(&uri).and_then(|p| self.files.get(p)).map(|u| u.text.as_str());
                let reply = if current != Some(original.as_str()) {
                    // The edit would apply to text that is gone.
                    jsonrpc::error(&id, CONTENT_MODIFIED, "the document changed while it was being formatted")
                } else {
                    match outcome {
                        Ok(Some(formatted)) if apply => {
                            if let Some(edit) = text::edit(&original, &formatted) {
                                self.next_id += 1;
                                let request_id = format!("kotlin/{}/{}", self.index, self.next_id);
                                let params = json!({ "label": "kargo fmt", "edit": { "changes": { uri: [edit] } } });
                                let _ = self.send(&jsonrpc::request(&request_id, "workspace/applyEdit", params));
                            }
                            jsonrpc::response(&id, Value::Null)
                        }
                        Ok(Some(formatted)) => {
                            jsonrpc::response(&id, json!(text::edit(&original, &formatted).into_iter().collect::<Vec<_>>()))
                        }
                        Ok(None) if apply => jsonrpc::response(&id, Value::Null),
                        Ok(None) => jsonrpc::response(&id, json!([])),
                        Err(e) => jsonrpc::error(&id, REQUEST_FAILED, format!("{e:#}")),
                    }
                };
                let _ = self.send(&reply);
            }
            Event::Rescanned { generation, present, changed, typescript } => {
                self.rescanning = false;
                if generation == self.load_generation {
                    self.rescanned(present, changed);
                    if typescript != self.typescript_stamps {
                        self.typescript_stamps = typescript;
                        self.regenerate_js();
                    }
                }
            }
            Event::JsDeclarations { generation, units } if generation == self.load_generation => {
                let dirs = &self.model.js_declarations;
                let mut all: Vec<Arc<Unit>> =
                    self.lib.units.iter().filter(|u| !dirs.iter().any(|(d, _)| u.path.starts_with(d))).cloned().collect();
                all.extend(units);
                self.lib = Arc::new(Index::new(true, all).with_jdk(self.lib.jdk.clone()));
                self.libraries_generation += 1;
                self.resolve_scripts();
                self.changed();
                // Kotlin code calling into the modules resolves differently now.
                self.republish_diagnostics();
            }
            Event::Searching { token, percentage } => {
                let value = json!({ "kind": "report", "percentage": percentage });
                let _ = self.send(&jsonrpc::notification("$/progress", json!({ "token": token, "value": value })));
            }
            Event::Searched { id, generation, token, outcome } => {
                let cancelled = self.searches.remove(&id.to_string()).is_some_and(|c| c.load(Ordering::Relaxed));
                if let Some(token) = token {
                    let value = json!({ "kind": "end" });
                    let _ = self.send(&jsonrpc::notification("$/progress", json!({ "token": token, "value": value })));
                }
                let reply = if cancelled {
                    jsonrpc::error(&id, REQUEST_CANCELLED, "the request was cancelled")
                } else if generation != self.generation {
                    // Positions in the answer would not match the text that is there now.
                    jsonrpc::error(&id, CONTENT_MODIFIED, "the project changed while it was being searched")
                } else {
                    match outcome {
                        Ok(result) => jsonrpc::response(&id, result),
                        Err(Failure::NotFound) => jsonrpc::error(&id, jsonrpc::METHOD_NOT_FOUND, "this request is not supported"),
                        Err(Failure::Invalid(text)) => jsonrpc::error(&id, -32602, text),
                    }
                };
                let _ = self.send(&reply);
            }
            _ => {}
        }
    }

    /// Asks the editor to show a progress, if it can; it begins once the editor accepts the token.
    fn start_progress(&mut self, generation: u64, kind: &str, title: &'static str, message: String) -> Option<Progress> {
        if !self.work_done_progress {
            return None;
        }
        self.next_id += 1;
        let create_id = format!("kotlin/{}/{}", self.index, self.next_id);
        let token = format!("{create_id}/{kind}");
        let _ = self.send(&jsonrpc::request(&create_id, "window/workDoneProgress/create", json!({ "token": token })));
        Some(Progress { generation, token, create_id, title, message, begun: false })
    }

    /// Ends the `$/progress` of downloading sources jars, if the editor showed it.
    fn end_progress(&mut self, message: Option<String>) {
        if let Some(progress) = self.progress.take() {
            self.end(progress, message);
        }
    }

    /// Ends the `$/progress` of loading the project, if the editor showed it.
    fn end_loading(&mut self) {
        if let Some(progress) = self.loading.take() {
            self.end(progress, None);
        }
    }

    fn end(&mut self, progress: Progress, message: Option<String>) {
        if progress.begun {
            let mut value = json!({ "kind": "end" });
            if let Some(message) = message {
                value["message"] = json!(message);
            }
            let _ = self.send(&jsonrpc::notification("$/progress", json!({ "token": progress.token, "value": value })));
        }
    }

    /// Says what the load is doing now.
    fn report_loading(&mut self, message: &str) {
        let Some(progress) = &mut self.loading else { return };
        progress.message = message.to_owned();
        if progress.begun {
            let params = json!({ "token": progress.token, "value": { "kind": "report", "message": message } });
            let _ = self.send(&jsonrpc::notification("$/progress", params));
        }
    }

    /// The editor answered one of the server's requests: begins the progress whose token it accepted.
    fn response(&mut self, message: &Value) -> anyhow::Result<()> {
        for loading in [false, true] {
            let slot = if loading { &mut self.loading } else { &mut self.progress };
            let Some(progress) = slot else { continue };
            if message["id"].as_str() != Some(progress.create_id.as_str()) || progress.begun {
                continue;
            }
            if !message["error"].is_null() {
                *slot = None;
                return Ok(());
            }
            progress.begun = true;
            let value = json!({ "kind": "begin", "title": progress.title, "message": progress.message, "cancellable": false });
            let params = json!({ "token": progress.token, "value": value });
            return self.send(&jsonrpc::notification("$/progress", params));
        }
        Ok(())
    }

    fn changed(&mut self) {
        self.proj = None;
        self.generation += 1;
    }

    fn reload(&mut self) {
        let Some(root) = self.root.clone() else { return };
        self.load_generation += 1;
        self.end_loading();
        self.loading = self.start_progress(self.load_generation, "load", "Loading Kotlin project", "reading the sources".to_owned());
        self.status("indexing", true);
        let (config, tx, cache, generation, options) =
            (self.config.clone(), self.tx.clone(), Arc::clone(&self.cache), self.load_generation, self.load_options.clone());
        thread::spawn(move || load(config, root, generation, tx, cache, options));
    }

    fn message(&mut self, message: Value) -> anyhow::Result<bool> {
        let method = jsonrpc::method(&message).to_owned();
        let params = &message["params"];
        match jsonrpc::kind(&message) {
            Kind::Response => self.response(&message)?,
            Kind::Notification => match method.as_str() {
                "exit" => return Ok(false),
                "initialized" => self.register_watchers()?,
                "$/cancelRequest" => {
                    if let Some(cancel) = self.searches.get(&params["id"].to_string()) {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
                "textDocument/didOpen" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    let text = params["textDocument"]["text"].as_str().unwrap_or_default().to_owned();
                    if let Some(path) = file_path(uri).filter(|path| is_kotlin_document(path, &text)) {
                        self.open.insert(uri.to_owned(), path.clone());
                        if let Some(version) = params["textDocument"]["version"].as_i64() {
                            self.versions.insert(path.clone(), version);
                        }
                        self.set_file(path.clone(), text);
                        self.script_declarations(&path);
                        self.publish_diagnostics(uri)?;
                    }
                }
                "textDocument/didChange" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    if let Some(path) = self.open.get(uri).cloned() {
                        let mut text = self.files.get(&path).map(|u| u.text.clone()).unwrap_or_default();
                        let changes = params["contentChanges"].as_array().map(Vec::as_slice).unwrap_or_default();
                        for change in changes.iter().filter(|c| c.get("range").is_some()) {
                            for word in features::words(change["text"].as_str().unwrap_or_default()) {
                                self.note_used(word);
                            }
                        }
                        text::apply_changes(&mut text, changes);
                        if let Some(version) = params["textDocument"]["version"].as_i64() {
                            self.versions.insert(path.clone(), version);
                        }
                        self.set_file(path.clone(), text);
                        self.script_declarations(&path);
                        self.publish_diagnostics(uri)?;
                    }
                }
                "textDocument/didSave" => {
                    if self.polling() {
                        self.rescan();
                    }
                }
                "workspace/didRenameFiles" | "workspace/didDeleteFiles" => {
                    let files = params["files"].as_array().map(Vec::as_slice).unwrap_or_default();
                    let path = |file: &Value, key: &str| file[key].as_str().and_then(file_path);
                    let (removed, added): (Vec<PathBuf>, Vec<PathBuf>) = if method == "workspace/didRenameFiles" {
                        (files.iter().filter_map(|f| path(f, "oldUri")).collect(), files.iter().filter_map(|f| path(f, "newUri")).collect())
                    } else {
                        (files.iter().filter_map(|f| path(f, "uri")).collect(), Vec::new())
                    };
                    self.files_moved(&removed, &added);
                }
                "textDocument/didClose" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    if let Some(path) = self.open.remove(uri) {
                        self.versions.remove(&path);
                        self.scripts.remove(&path);
                        self.send(&jsonrpc::notification("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] })))?;
                        // Back to what is on disk, or gone.
                        match std::fs::read_to_string(&path) {
                            Ok(text) if self.model.origin(&path) != Origin::Loose => self.set_file(path, text),
                            _ => {
                                self.files.remove(&path);
                                self.changed();
                            }
                        }
                    }
                }
                "workspace/didChangeWatchedFiles" => {
                    let mut reload = false;
                    let mut typescript = false;
                    for change in params["changes"].as_array().into_iter().flatten() {
                        let uri = change["uri"].as_str().unwrap_or_default();
                        let Some(path) = file_path(uri) else { continue };
                        let name = text::file_name(uri);
                        if name == kargo_manifest::MANIFEST_NAME || name == "kargo.lock" {
                            reload = true;
                        } else if self.is_typescript_source(&path) {
                            typescript = true;
                        } else if (uri.ends_with(".kt") || uri.ends_with(".java")) && !self.open.contains_key(uri) {
                            match std::fs::read_to_string(&path) {
                                Ok(text) if change["type"] != 3 && self.model.origin(&path) != Origin::Loose => self.set_file(path, text),
                                _ => {
                                    self.files.remove(&path);
                                    self.changed();
                                }
                            }
                        }
                    }
                    if reload {
                        self.reload();
                    } else if typescript {
                        self.regenerate_js();
                    }
                }
                _ => {}
            },
            Kind::Request => {
                let id = message["id"].clone();
                if method == "textDocument/formatting" {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_owned();
                    self.format(id, uri, false)?;
                    return Ok(true);
                }
                if method == "workspace/executeCommand" && params["command"] == FORMAT_COMMAND {
                    let uri = params["arguments"][0]["uri"].as_str().unwrap_or_default().to_owned();
                    self.format(id, uri, true)?;
                    return Ok(true);
                }
                if matches!(
                    method.as_str(),
                    "textDocument/references" | "textDocument/rename" | "workspace/willRenameFiles" | "workspace/symbol"
                ) {
                    self.search(id, &method, params)?;
                    return Ok(true);
                }
                let reply = match self.request(&method, params) {
                    Ok(result) => jsonrpc::response(&id, result),
                    Err(Failure::NotFound) => jsonrpc::error(&id, jsonrpc::METHOD_NOT_FOUND, format!("`{method}` is not supported")),
                    Err(Failure::Invalid(text)) => jsonrpc::error(&id, -32602, text),
                };
                self.send(&reply)?;
            }
        }
        Ok(true)
    }

    /// Formats an open document with ktlint on a worker thread, which answers when it is done.
    /// The JVM takes a while to start, and the other requests must not wait for it.
    /// With `apply` (the `kargo/formatFile` command), the server applies the edit itself.
    fn format(&mut self, id: Value, uri: String, apply: bool) -> anyhow::Result<()> {
        let open = self.open.get(&uri).and_then(|path| self.files.get(path).map(|unit| (path.clone(), unit.text.clone())));
        let Some((path, original)) = open else { return self.send(&jsonrpc::response(&id, Value::Null)) };
        let (config, tx, formatter) = (self.config.clone(), self.tx.clone(), Arc::clone(&self.formatter));
        thread::spawn(move || {
            let outcome = formatter(&config, &path, &original);
            let _ = tx.send(Event::Formatted { id, uri, original, outcome, apply });
        });
        Ok(())
    }

    /// Sends `kargo/status` for `task` (`indexing`), when the editor asked for it.
    fn status(&mut self, task: &str, active: bool) {
        if self.status {
            let _ = self.send(&jsonrpc::notification("kargo/status", json!({ "task": task, "active": active })));
        }
    }

    /// Takes files the editor renamed or deleted out of the index at once, and reads the new
    /// ones, without waiting for the file watcher. A path may be a directory.
    fn files_moved(&mut self, removed: &[PathBuf], added: &[PathBuf]) {
        let gone: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|path| removed.iter().any(|r| path.starts_with(r)))
            .filter(|path| !self.open.values().any(|open| open == *path))
            .cloned()
            .collect();
        for path in gone {
            self.files.remove(&path);
            self.stamps.remove(&path);
            self.changed();
        }
        let mut found = Vec::new();
        for path in added {
            if path.is_dir() {
                for entry in walkdir::WalkDir::new(path).into_iter().flatten().filter(|e| e.file_type().is_file()) {
                    found.push(entry.into_path());
                }
            } else {
                found.push(path.clone());
            }
        }
        for path in found {
            let source = path.extension().is_some_and(|e| e == "kt" || e == "java");
            if !source || self.open.values().any(|open| *open == path) || self.model.origin(&path) == Origin::Loose {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                self.set_file(path, text);
            }
        }
    }

    /// Answers a references or rename request on a worker thread: the search resolves every
    /// same-named identifier of the project, which takes a while in a large one, and the other
    /// requests must not wait for it. The editor can cancel it with `$/cancelRequest`, and sees
    /// `$/progress` when it sent a `workDoneToken`.
    fn search(&mut self, id: Value, method: &str, params: &Value) -> anyhow::Result<()> {
        let (proj, ids) = self.snapshot();
        let document = file_path(params["textDocument"]["uri"].as_str().unwrap_or_default());
        let (lib, packages, tx, generation) =
            (self.libraries_for(document.as_deref()), self.model.packages.clone(), self.tx.clone(), self.generation);
        let cancel = Arc::new(AtomicBool::new(false));
        self.searches.insert(id.to_string(), Arc::clone(&cancel));
        let token = Some(params["workDoneToken"].clone()).filter(|t| !t.is_null());
        if let Some(token) = &token {
            let title = match method {
                "textDocument/references" => "Searching for references",
                "workspace/symbol" => "Searching for symbols",
                _ => "Renaming",
            };
            let value = json!({ "kind": "begin", "title": title, "cancellable": true, "percentage": 0 });
            self.send(&jsonrpc::notification("$/progress", json!({ "token": token, "value": value })))?;
        }
        let (method, params, symbol_resolve) = (method.to_owned(), params.clone(), self.symbol_resolve);
        let env = rename::Env {
            roots: self.model.roots.iter().map(|(root, _)| root.clone()).collect(),
            manifests: self.model.manifests.clone(),
            versions: self.versions.clone(),
            document_changes: self.document_changes,
            resource_renames: self.rename_files,
            rename_files: self.rename_option,
            move_directories: self.move_option,
        };
        thread::spawn(move || {
            let last = std::cell::Cell::new(u32::MAX);
            let report = |done: usize, total: usize| {
                let Some(token) = &token else { return };
                let percentage = (done * 100 / total.max(1)) as u32;
                if last.replace(percentage) != percentage {
                    let _ = tx.send(Event::Searching { token: token.clone(), percentage });
                }
            };
            let cancelled = || cancel.load(Ordering::Relaxed);
            let control = features::Control { cancelled: Some(&cancelled), progress: Some(&report) };
            let outcome = run_search(&method, &params, &lib, &proj, &ids, &packages, &env, &control, symbol_resolve);
            let _ = tx.send(Event::Searched { id, generation, token, outcome });
        });
        Ok(())
    }

    /// Pushes the diagnostics of the open document `uri`: those that need no resolution
    /// ([`diagnostics`]), and those that need the project's declarations (operators and
    /// deprecated usages). `kargo lsp` merges them with the check's.
    fn publish_diagnostics(&mut self, uri: &str) -> anyhow::Result<()> {
        let Some(path) = self.open.get(uri).cloned() else { return Ok(()) };
        let Some(unit) = self.files.get(&path).cloned() else { return Ok(()) };
        let mut problems = diagnostics::problems(&unit.text, &unit.file, self.diagnostic_options);
        // Operators and deprecated usages need the project's declarations.
        let (proj, ids) = self.snapshot();
        if let Some(&id) = ids.get(&path) {
            let lib = self.libraries_for(Some(&path));
            let packages = self.model.packages.clone();
            let an = Analyzer::new(&lib, &proj, &packages);
            let u = UnitId { lib: false, unit: id };
            problems.extend(super::operators::problems(&an, u));
            problems.extend(super::kdoc::problems(&an, u));
            if self.diagnostic_options.deprecated {
                problems.extend(diagnostics::deprecated_usages(&an, u));
            }
            problems.sort_by_key(|p| (p.span.start, p.span.end));
        }
        let diagnostics: Vec<Value> = problems.iter().map(|p| p.to_json(&unit.text)).collect();
        self.send(&jsonrpc::notification("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": diagnostics })))
    }

    /// Pushes the diagnostics of every open document again, after the declarations they resolve
    /// against changed.
    fn republish_diagnostics(&mut self) {
        for uri in self.open.keys().cloned().collect::<Vec<_>>() {
            let _ = self.publish_diagnostics(&uri);
        }
    }

    /// Remembers that `word` was inserted, forgetting the oldest names past [`RECENT_NAMES`].
    fn note_used(&mut self, word: &str) {
        self.clock += 1;
        self.recent.insert(word.to_owned(), self.clock);
        if self.recent.len() > RECENT_NAMES {
            let cutoff = self.clock.saturating_sub(RECENT_NAMES as u64);
            self.recent.retain(|_, &mut at| at > cutoff);
        }
    }

    fn set_file(&mut self, path: PathBuf, text: String) {
        let origin = self.model.origin(&path);
        let unit = Arc::new(Unit::new(path.clone(), text, origin));
        let replaced = self.files.insert(path.clone(), Arc::clone(&unit)).is_some();
        // A changed file updates its own entries; a new one shifts the unit numbers.
        let patched = replaced
            && self.proj.as_mut().is_some_and(|(index, ids)| {
                let slot = ids.get(&path).copied();
                match (slot, Arc::get_mut(index)) {
                    (Some(slot), Some(index)) => index.replace_unit(slot, unit),
                    _ => false,
                }
            });
        if patched {
            self.generation += 1;
        } else {
            self.changed();
        }
    }

    /// Where the strings of the file at `path` name files from, for its document links: the
    /// file's directory, and the directory of its package with that package's `resources/`.
    fn link_bases(&self, path: &Path) -> Vec<PathBuf> {
        let mut bases: Vec<PathBuf> = path.parent().map(Path::to_path_buf).into_iter().collect();
        let package = self
            .model
            .manifests
            .iter()
            .filter_map(|m| m.parent())
            .filter(|dir| path.starts_with(dir))
            .max_by_key(|dir| dir.as_os_str().len());
        if let Some(dir) = package.map(Path::to_path_buf).or_else(|| self.root.clone()) {
            bases.push(dir.join("resources"));
            bases.push(dir);
        }
        bases
    }

    /// The libraries the file at `path` sees: the project's, and those of a Kargo script's own
    /// dependencies on top of them once they are resolved.
    fn libraries_for(&self, path: Option<&Path>) -> Arc<Index> {
        path.and_then(|p| self.scripts.get(p)).and_then(|s| s.index.clone()).unwrap_or_else(|| Arc::clone(&self.lib))
    }

    /// Follows the dependencies the script at `path` declares, which changed with its text: a
    /// script without dependencies is forgotten, and the others are resolved again unless their
    /// dependencies are the same.
    fn script_declarations(&mut self, path: &Path) {
        if !is_script_path(path) {
            return;
        }
        let Some(text) = self.files.get(path).map(|unit| unit.text.clone()) else { return };
        let declared = kargo_ops::script::declared_dependencies(&text);
        if declared.is_empty() {
            if self.scripts.remove(path).is_some() {
                self.generation += 1;
            }
            return;
        }
        if self.scripts.get(path).is_some_and(|s| s.declared == declared) {
            return;
        }
        self.scripts.insert(path.to_owned(), ScriptLibraries { declared: declared.clone(), index: None });
        self.resolve_script(path, &text, declared);
    }

    /// Resolves the dependencies of every open script again, against the libraries that just changed.
    fn resolve_scripts(&mut self) {
        let scripts: Vec<(PathBuf, Vec<String>)> = self.scripts.iter().map(|(p, s)| (p.clone(), s.declared.clone())).collect();
        for (path, declared) in scripts {
            if let Some(text) = self.files.get(&path).map(|unit| unit.text.clone()) {
                self.resolve_script(&path, &text, declared);
            }
        }
    }

    /// Resolves the dependencies of the script at `path` on a worker thread, with the project's
    /// libraries as they are now. Until the libraries are loaded there is nothing to add them to,
    /// so [`State::resolve_scripts`] does it when they are.
    fn resolve_script(&self, path: &Path, text: &str, declared: Vec<String>) {
        if !self.libraries_loaded {
            return;
        }
        let (config, tx, cache) = (self.config.clone(), self.tx.clone(), Arc::clone(&self.cache));
        let (generation, base, jdk) = (self.libraries_generation, self.lib.units.clone(), self.lib.jdk.clone());
        let (fetch, path, text) = (self.load_options.fetch_sources, path.to_owned(), text.to_owned());
        thread::spawn(move || {
            let index = kargo_ops::script::dependency_metadata(&config, &text, fetch).ok().map(|meta| {
                let mut units = base;
                units.extend(library::units(&config, &cache, &dependency_jars(&meta)));
                units.extend(library::compiled_units(&config, &cache, &dependency_compiled(&meta)));
                Arc::new(Index::new(true, units).with_jdk(jdk))
            });
            let _ = tx.send(Event::Scripted { path, declared, generation, index });
        });
    }

    /// Whether the editor does not tell the server about changed files, so the server looks.
    fn polling(&self) -> bool {
        !self.watch && self.root.is_some() && self.project_loaded
    }

    /// How long the loop may wait for a message before [`State::poll_due`] is due.
    fn poll_wait(&self) -> Duration {
        if self.polling() { self.next_poll.saturating_duration_since(Instant::now()) } else { Duration::from_secs(3600) }
    }

    /// Scans the source roots when it is time to.
    fn poll_due(&mut self) {
        if self.polling() && Instant::now() >= self.next_poll {
            self.rescan();
        }
    }

    /// Looks for new, changed and deleted source files on a worker thread, which reads only the
    /// files whose modification time changed.
    fn rescan(&mut self) {
        self.next_poll = Instant::now() + RESCAN_INTERVAL;
        if self.rescanning || !self.project_loaded {
            return;
        }
        self.rescanning = true;
        let (model, stamps, tx, generation) = (self.model.clone(), self.stamps.clone(), self.tx.clone(), self.load_generation);
        thread::spawn(move || {
            let typescript = model
                .js_sources
                .iter()
                .flat_map(|dir| kargo_util::fs::collect_files(dir, &["ts", "mts"]))
                .filter_map(|path| Some((path.clone(), std::fs::metadata(&path).and_then(|m| m.modified()).ok()?)))
                .collect();
            let mut paths: Vec<PathBuf> =
                model.roots.iter().flat_map(|(root, _)| kargo_util::fs::collect_files(root, &["kt", "java"])).collect();
            paths.extend(model.files.iter().map(|(p, _)| p.clone()).filter(|p| p.is_file()));
            let present: HashSet<PathBuf> = paths.iter().cloned().collect();
            let changed = paths
                .into_iter()
                .filter_map(|path| {
                    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
                    if stamps.get(&path) == Some(&stamp) {
                        return None;
                    }
                    let text = std::fs::read_to_string(&path).ok()?;
                    Some((path, text, stamp))
                })
                .collect();
            let _ = tx.send(Event::Rescanned { generation, present, changed, typescript });
        });
    }

    /// Takes in what a scan found: files that appeared or changed on disk, and files that are gone.
    fn rescanned(&mut self, present: HashSet<PathBuf>, changed: Vec<(PathBuf, String, SystemTime)>) {
        for (path, text, stamp) in changed {
            self.stamps.insert(path.clone(), stamp);
            let open = self.open.values().any(|p| *p == path);
            if !open && self.files.get(&path).is_none_or(|unit| unit.text != text) {
                self.set_file(path, text);
            }
        }
        let gone: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|path| !present.contains(*path) && self.model.origin(path) != Origin::Loose)
            .filter(|path| !self.open.values().any(|p| p == *path))
            .cloned()
            .collect();
        for path in gone {
            self.stamps.remove(&path);
            self.files.remove(&path);
            self.changed();
        }
    }

    /// Whether `path` is a TypeScript file of the JavaScript sources, whose Kotlin declarations
    /// [`Self::regenerate_js`] writes.
    fn is_typescript_source(&self, path: &Path) -> bool {
        let name = path.to_string_lossy();
        (name.ends_with(".ts") || name.ends_with(".mts")) && self.model.js_sources.iter().any(|dir| path.starts_with(dir))
    }

    /// Writes the Kotlin declarations of the project's TypeScript modules again on a worker thread,
    /// as `kargo build` would, without a build: only files whose text changes are rewritten, and the
    /// libraries take them in when any did ([`Event::JsDeclarations`]).
    fn regenerate_js(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if self.model.js_declarations.is_empty() {
            return;
        }
        let (tx, generation, dirs) = (self.tx.clone(), self.load_generation, self.model.js_declarations.clone());
        thread::spawn(move || {
            if crate::project::js_declarations(&root).unwrap_or(false) {
                let units =
                    dirs.iter().flat_map(|(dir, platform)| library::directory_units("js", dir, *platform, Platforms::NONE)).collect();
                let _ = tx.send(Event::JsDeclarations { generation, units });
            }
        });
    }

    fn register_watchers(&mut self) -> anyhow::Result<()> {
        if !self.watch {
            return Ok(());
        }
        self.next_id += 1;
        let id = format!("kotlin/{}/{}", self.index, self.next_id);
        let watchers = json!([
            { "globPattern": "**/*.kt" },
            { "globPattern": "**/*.java" },
            { "globPattern": "**/kargo.toml" },
            { "globPattern": "**/kargo.lock" },
            { "globPattern": "**/*.ts" },
            { "globPattern": "**/*.mts" }
        ]);
        let params = json!({ "registrations": [{ "id": "kargo-kotlin-watch", "method": "workspace/didChangeWatchedFiles", "registerOptions": { "watchers": watchers } }] });
        self.send(&jsonrpc::request(&id, "client/registerCapability", params))
    }

    /// The project index, rebuilt after changes.
    fn snapshot(&mut self) -> Snapshot {
        if let Some((index, ids)) = &self.proj {
            return (Arc::clone(index), Arc::clone(ids));
        }
        let mut units: Vec<Arc<Unit>> = self.files.values().cloned().collect();
        units.sort_by(|a, b| a.path.cmp(&b.path));
        let ids: Arc<HashMap<PathBuf, u32>> = Arc::new(units.iter().enumerate().map(|(i, u)| (u.path.clone(), i as u32)).collect());
        let index = Arc::new(Index::new(false, units));
        self.proj = Some((Arc::clone(&index), Arc::clone(&ids)));
        (index, ids)
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, Failure> {
        match method {
            "initialize" => return Ok(self.initialize(params)),
            "shutdown" => return Ok(Value::Null),
            _ => {}
        }
        let (proj, ids) = self.snapshot();
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let lib = self.libraries_for(file_path(uri).as_deref());
        let packages = self.model.packages.clone();
        let an = Analyzer::new(&lib, &proj, &packages);
        let generation = self.generation;
        // A Java file is read for its declarations; this server answers requests for Kotlin files only.
        let unit = file_path(uri)
            .and_then(|p| ids.get(&p).copied())
            .filter(|&u| !proj.units[u as usize].is_java())
            .map(|u| UnitId { lib: false, unit: u });
        let at = |unit: UnitId| -> (usize, Option<usize>) {
            let u = an.unit(unit);
            let offset = text::offset(&u.text, &params["position"]);
            (offset, features::token_at(&an, unit, offset))
        };
        match method {
            "textDocument/completion" => {
                let Some(unit) = unit else { return Ok(json!({ "isIncomplete": false, "items": [] })) };
                let trigger = params["context"]["triggerCharacter"].as_str();
                if trigger.is_some_and(|t| t != "." && t != ":" && t != "@") {
                    return Ok(json!({ "isIncomplete": true, "items": [] }));
                }
                let (offset, _) = at(unit);
                let key =
                    CompletionKey { uri: uri.to_owned(), offset, trigger: trigger.map(str::to_owned), generation, snippets: self.snippets };
                if let Some((cached, list)) = &self.completions
                    && *cached == key
                {
                    return Ok(list.clone());
                }
                let opts =
                    CompletionOptions { snippets: self.snippets, package: directory_package(&self.model.roots, &an.unit(unit).path) };
                let mut list = features::completion(&an, unit, offset, opts, generation);
                features::rank_recent(&mut list, &self.recent);
                self.completions = Some((key, list.clone()));
                Ok(list)
            }
            "completionItem/resolve" => {
                if params["data"]["generation"].as_u64() != Some(generation) {
                    return Ok(params.clone());
                }
                Ok(features::resolve_item(&an, params.clone()))
            }
            "textDocument/hover" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (offset, tok) = at(unit);
                let hover = match tok {
                    Some(tok) => features::hover(&an, unit, tok),
                    None => features::reference_hover(&an, unit, offset),
                };
                Ok(hover.unwrap_or(Value::Null))
            }
            "textDocument/signatureHelp" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (offset, _) = at(unit);
                Ok(features::signature_help(&an, unit, offset).unwrap_or(Value::Null))
            }
            "textDocument/definition" | "textDocument/declaration" | "textDocument/typeDefinition" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (offset, tok) = at(unit);
                let mut targets = match tok {
                    Some(tok) => features::targets_at(&an, unit, tok),
                    // A name in a KDoc comment.
                    None => super::kdoc::reference_at(&an, unit, offset).map(|(_, targets)| targets).unwrap_or_default(),
                };
                if method == "textDocument/typeDefinition" {
                    targets = type_targets(&an, &targets);
                }
                let locations: Vec<Value> = targets.iter().take(1).filter_map(|t| features::location(&an, t)).collect();
                Ok(json!(locations))
            }
            "textDocument/implementation" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(sym) = tok.and_then(|t| features::targets_at(&an, unit, t).into_iter().find_map(|t| t.sym())) else {
                    return Ok(json!([]));
                };
                let found = hierarchy::implementations(&an, sym);
                let locations = found
                    .iter()
                    .filter_map(|&sym| features::location(&an, &Target::Sym(super::resolve::Member { sym, subst: Default::default() })));
                Ok(json!(locations.collect::<Vec<_>>()))
            }
            "textDocument/prepareCallHierarchy" | "textDocument/prepareTypeHierarchy" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(tok) = tok else { return Ok(Value::Null) };
                Ok(if method.contains("Call") {
                    hierarchy::prepare_calls(&an, unit, tok)
                } else {
                    hierarchy::prepare_types(&an, unit, tok)
                })
            }
            "callHierarchy/incomingCalls" => Ok(hierarchy::incoming(&an, &params["item"])),
            "callHierarchy/outgoingCalls" => Ok(hierarchy::outgoing(&an, &params["item"])),
            "typeHierarchy/supertypes" => Ok(hierarchy::supertypes(&an, &params["item"])),
            "typeHierarchy/subtypes" => Ok(hierarchy::direct_subtypes(&an, &params["item"])),
            "textDocument/documentHighlight" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (offset, tok) = at(unit);
                let Some(group) = group_of(&an, unit, offset, tok) else { return Ok(json!([])) };
                let mut refs = features::references(&an, &group, true);
                refs.extend(features::doc_references(&an, &group, &features::Control::default()).unwrap_or_default());
                refs.extend(super::operators::operator_uses(&an, &group, &features::Control::default()).unwrap_or_default());
                let text = &an.unit(unit).text;
                Ok(json!(
                    refs.iter()
                        .filter(|(u, _)| *u == unit)
                        .map(|(_, r)| json!({ "range": text::range(text, r.clone()) }))
                        .collect::<Vec<_>>()
                ))
            }
            "textDocument/prepareRename" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let u = an.unit(unit);
                let found = rename::subject(&an, unit, text::offset(&u.text, &params["position"])).map_err(Failure::Invalid)?;
                Ok(json!({ "range": text::range(&u.text, found.span), "placeholder": found.placeholder }))
            }
            "textDocument/documentSymbol" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                Ok(features::document_symbols(&an, unit))
            }
            "textDocument/codeAction" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                let text = &an.unit(unit).text;
                let start = text::offset(text, &params["range"]["start"]);
                let end = text::offset(text, &params["range"]["end"]).max(start);
                let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                let request = actions::Request {
                    unit,
                    range: start..end,
                    only: strings(&params["context"]["only"]),
                    diagnostics: params["context"]["diagnostics"].as_array().cloned().unwrap_or_default(),
                    roots: &self.model.roots,
                    manifests: &self.model.manifests,
                    document_changes: self.document_changes,
                    create_files: self.create_files,
                };
                let mut found = actions::code_actions(&an, &request);
                if self.apply_edit && request.wants(actions::FORMAT) && self.open.contains_key(uri) {
                    found.push(json!({
                        "title": "Run `kargo fmt` on file",
                        "kind": actions::FORMAT,
                        "command": { "title": "kargo fmt", "command": FORMAT_COMMAND, "arguments": [{ "uri": uri }] },
                    }));
                }
                Ok(json!(found))
            }
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/range" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let path = an.unit(unit).path.clone();
                let tokens = match self.classified.get(&path) {
                    Some((g, tokens)) if *g == generation => Arc::clone(tokens),
                    _ => {
                        let tokens = Arc::new(semantic::classify(&an, unit));
                        self.classified.insert(path, (generation, Arc::clone(&tokens)));
                        tokens
                    }
                };
                let text = &an.unit(unit).text;
                let range = (method.ends_with("range"))
                    .then(|| text::offset(text, &params["range"]["start"])..text::offset(text, &params["range"]["end"]));
                Ok(semantic::encode(text, &tokens, range))
            }
            "textDocument/inlayHint" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                let text = &an.unit(unit).text;
                let range = (!params["range"].is_null())
                    .then(|| text::offset(text, &params["range"]["start"])..text::offset(text, &params["range"]["end"]));
                Ok(inlay::hints(&an, unit, range, self.inlay_options))
            }
            "textDocument/foldingRange" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                Ok(ranges::folding_ranges(an.file(unit), &an.unit(unit).text))
            }
            "textDocument/selectionRange" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let positions = params["positions"].as_array().cloned().unwrap_or_default();
                Ok(ranges::selection_ranges(an.file(unit), &an.unit(unit).text, &positions))
            }
            "workspaceSymbol/resolve" => Ok(features::resolve_symbol(&an, params.clone())),
            "textDocument/documentLink" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                let u = an.unit(unit);
                Ok(links::document_links(&u.file, &u.text, &self.link_bases(&u.path)))
            }
            "textDocument/linkedEditingRange" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let u = an.unit(unit);
                Ok(editing::linked_editing(&u.file, &u.text, text::offset(&u.text, &params["position"])))
            }
            "textDocument/onTypeFormatting" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                let u = an.unit(unit);
                let offset = text::offset(&u.text, &params["position"]);
                let ch = params["ch"].as_str().unwrap_or_default();
                Ok(json!(editing::on_type(&u.file, &u.text, offset, ch, &editing::Indent::from_options(&params["options"]))))
            }
            "textDocument/inlineValue" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                let u = an.unit(unit);
                let span = |range: &Value| text::offset(&u.text, &range["start"])..text::offset(&u.text, &range["end"]);
                let stopped = &params["context"]["stoppedLocation"];
                Ok(inlay::inline_values(&u.file, &u.text, span(&params["range"]), span(stopped)))
            }
            "kargo/testTree" => {
                let only = file_path(uri);
                let files: Vec<testing::TestFile> = proj
                    .units
                    .iter()
                    .filter(|u| !u.is_java() && only.as_ref().is_none_or(|p| *p == u.path))
                    .filter_map(|u| match &u.origin {
                        Origin::Source(set) if set.kind == SetKind::Test => Some(testing::TestFile {
                            unit: u,
                            target: set.some_target(),
                            cwd: self.model.manifests.get(set.package).and_then(|m| m.parent()),
                        }),
                        _ => None,
                    })
                    .collect();
                let program = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "kargo".to_owned());
                Ok(json!(testing::test_tree(&files, &program)))
            }
            _ => Err(Failure::NotFound),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let completion = &params["capabilities"]["textDocument"]["completion"];
        self.snippets = completion["completionItem"]["snippetSupport"] == true;
        self.watch = params["capabilities"]["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"] == true;
        self.work_done_progress = params["capabilities"]["window"]["workDoneProgress"] == true;
        self.symbol_resolve = params["capabilities"]["workspace"]["symbol"]["resolveSupport"]["properties"]
            .as_array()
            .is_some_and(|properties| properties.iter().any(|p| p == "location.range"));
        let encoding = text::Encoding::negotiate(&params["capabilities"]["general"]["positionEncodings"]);
        text::set_encoding(encoding);
        let workspace_edit = &params["capabilities"]["workspace"]["workspaceEdit"];
        self.document_changes = workspace_edit["documentChanges"] == true;
        self.create_files = workspace_edit["resourceOperations"].as_array().is_some_and(|ops| ops.iter().any(|o| o == "create"));
        self.rename_files = workspace_edit["resourceOperations"].as_array().is_some_and(|ops| ops.iter().any(|o| o == "rename"));
        self.rename_option = params["initializationOptions"]["renameFiles"] != false;
        self.move_option = params["initializationOptions"]["movePackageDirectories"] != false;
        self.inlay_options = inlay::Options::from_json(&params["initializationOptions"]);
        self.diagnostic_options = diagnostics::Options::from_json(&params["initializationOptions"]);
        self.load_options.fetch_sources = params["initializationOptions"]["fetchSources"] != false;
        self.apply_edit = params["capabilities"]["workspace"]["applyEdit"] == true;
        self.status = params["initializationOptions"]["status"] == true;
        self.root = params["rootUri"]
            .as_str()
            .and_then(file_path)
            .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
            .or_else(|| params["workspaceFolders"][0]["uri"].as_str().and_then(file_path));
        self.reload();
        json!({
            "capabilities": {
                "positionEncoding": encoding.name(),
                "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
                "completionProvider": { "triggerCharacters": [".", "@", "["], "resolveProvider": true },
                "hoverProvider": true,
                "signatureHelpProvider": { "triggerCharacters": ["(", ","], "retriggerCharacters": ["="] },
                "definitionProvider": true,
                "declarationProvider": true,
                "typeDefinitionProvider": true,
                "implementationProvider": true,
                "callHierarchyProvider": true,
                "typeHierarchyProvider": true,
                "referencesProvider": true,
                "documentHighlightProvider": true,
                "renameProvider": { "prepareProvider": true },
                "documentSymbolProvider": true,
                "semanticTokensProvider": semantic::legend(),
                "foldingRangeProvider": true,
                "inlayHintProvider": true,
                "selectionRangeProvider": true,
                "documentFormattingProvider": true,
                "documentOnTypeFormattingProvider": {
                    "firstTriggerCharacter": editing::TRIGGERS[0],
                    "moreTriggerCharacter": &editing::TRIGGERS[1..],
                },
                "documentLinkProvider": { "resolveProvider": false },
                "linkedEditingRangeProvider": true,
                "inlineValueProvider": true,
                "workspaceSymbolProvider": { "resolveProvider": true },
                "codeActionProvider": { "codeActionKinds": actions::KINDS },
                "executeCommandProvider": { "commands": [FORMAT_COMMAND] },
                "workspace": {
                    "fileOperations": {
                        "willRename": {
                            "filters": [
                                { "pattern": { "glob": "**/*.kt", "matches": "file" } },
                                { "pattern": { "glob": "**", "matches": "folder" } }
                            ]
                        },
                        "didRename": { "filters": source_filters() },
                        "didDelete": { "filters": source_filters() },
                    }
                },
                "experimental": { "kargo": { "testTree": true } },
            },
            "serverInfo": { "name": "kargo-kotlin", "version": env!("CARGO_PKG_VERSION") },
        })
    }
}

enum Failure {
    NotFound,
    Invalid(String),
}

/// The references or the rename of the identifier at the request's position, or the symbols of
/// the workspace that match a query.
#[allow(clippy::too_many_arguments)]
fn run_search(
    method: &str,
    params: &Value,
    lib: &Index,
    proj: &Index,
    ids: &HashMap<PathBuf, u32>,
    packages: &Packages,
    env: &rename::Env,
    control: &features::Control,
    symbol_resolve: bool,
) -> Result<Value, Failure> {
    let an = Analyzer::new(lib, proj, packages);
    if method == "workspace/symbol" {
        let query = params["query"].as_str().unwrap_or_default();
        return features::workspace_symbols(&an, query, symbol_resolve, control)
            .ok_or_else(|| Failure::Invalid("the request was cancelled".into()));
    }
    if method == "workspace/willRenameFiles" {
        let files: Vec<(PathBuf, PathBuf)> = params["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((file_path(f["oldUri"].as_str()?)?, file_path(f["newUri"].as_str()?)?)))
            .collect();
        return rename::will_rename(&an, env, &files, control).map_err(Failure::Invalid);
    }
    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
    let unit = file_path(uri)
        .and_then(|p| ids.get(&p).copied())
        .filter(|&u| !proj.units[u as usize].is_java())
        .map(|u| UnitId { lib: false, unit: u });
    let Some(unit) = unit else { return Ok(Value::Null) };
    let u = an.unit(unit);
    let offset = text::offset(&u.text, &params["position"]);
    let tok = features::token_at(&an, unit, offset);
    // The answer to a cancelled search is replaced by the cancellation error.
    let cancelled = || Failure::Invalid("the request was cancelled".into());
    if method == "textDocument/references" {
        let Some(group) = group_of(&an, unit, offset, tok) else { return Ok(json!([])) };
        let include = params["context"]["includeDeclaration"] != false;
        let mut refs = features::search(&an, &group, include, control).ok_or_else(cancelled)?;
        refs.extend(features::doc_references(&an, &group, control).ok_or_else(cancelled)?);
        refs.extend(super::operators::operator_uses(&an, &group, control).ok_or_else(cancelled)?);
        return Ok(json!(refs.iter().map(|(u, r)| location(&an, *u, r.clone())).collect::<Vec<_>>()));
    }
    let found = rename::subject(&an, unit, text::offset(&u.text, &params["position"])).map_err(Failure::Invalid)?;
    let new_name = params["newName"].as_str().unwrap_or_default();
    rename::rename(&an, env, &found, new_name, control).map_err(Failure::Invalid)
}

/// The command of the "Run `kargo fmt` on file" action: formats the file whose `uri` its argument
/// holds, and applies the edit with `workspace/applyEdit`.
pub const FORMAT_COMMAND: &str = "kargo/formatFile";

/// The files whose renames and deletions the server hears of at once: Kotlin and Java sources,
/// and folders that may hold them.
fn source_filters() -> Value {
    json!([
        { "pattern": { "glob": "**/*.kt", "matches": "file" } },
        { "pattern": { "glob": "**/*.java", "matches": "file" } },
        { "pattern": { "glob": "**", "matches": "folder" } }
    ])
}

fn location(an: &Analyzer, u: UnitId, range: std::ops::Range<usize>) -> Value {
    let unit = an.unit(u);
    json!({ "uri": text::path_to_uri(&unit.path), "range": text::range(&unit.text, range) })
}

/// The classes of the types of `targets`, for go to type definition.
fn type_targets(an: &Analyzer, targets: &[Target]) -> Vec<Target> {
    let ty = match targets.first() {
        Some(Target::Sym(m)) => an.value_ty(m.sym, &m.subst),
        Some(Target::It { unit, brace }) => an.lambda_ctx(*unit, *brace).and_then(|c| c.params.first().cloned().flatten()),
        _ => None,
    };
    match ty {
        Some(super::resolve::Ty::Class { sym, .. } | super::resolve::Ty::Static(sym)) => {
            vec![Target::Sym(super::resolve::Member { sym, subst: Default::default() })]
        }
        _ => Vec::new(),
    }
}

fn group_at(an: &Analyzer, unit: UnitId, tok: usize) -> Option<Vec<super::index::SymId>> {
    let sym = features::targets_at(an, unit, tok).into_iter().find_map(|t| t.sym())?;
    Some(features::rename_group(an, sym))
}

/// The declarations the name at `offset` refers to: the identifier at `tok`, or else a name in a
/// KDoc comment.
fn group_of(an: &Analyzer, unit: UnitId, offset: usize, tok: Option<usize>) -> Option<Vec<super::index::SymId>> {
    match tok {
        Some(tok) => group_at(an, unit, tok),
        None => super::kdoc::group_at(an, unit, offset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_package_of_a_directory_under_its_source_root() {
        let set = SourceSet { package: 0, kind: SetKind::Main, target: None };
        let roots = vec![(PathBuf::from("/p/src-jvm"), set.clone()), (PathBuf::from("/p/src"), set)];
        assert_eq!(directory_package(&roots, Path::new("/p/src/app/model/User.kt")).as_deref(), Some("app.model"));
        assert_eq!(directory_package(&roots, Path::new("/p/src-jvm/app/Main.kt")).as_deref(), Some("app"));
        // A keyword is backquoted.
        assert_eq!(directory_package(&roots, Path::new("/p/src/app/fun/F.kt")).as_deref(), Some("app.`fun`"));
        assert_eq!(directory_package(&roots, Path::new("/p/src/Main.kt")), None);
        assert_eq!(directory_package(&roots, Path::new("/elsewhere/Main.kt")), None);
    }

    #[test]
    fn reads_the_layout_from_metadata() {
        let meta = json!({
            "toolchain": { "kotlin": "2.4.21" },
            "packages": [
                {
                    "name": "app",
                    "path-dependencies": [{ "package": "lib" }],
                    "targets": [
                        {
                            "kind": "jvm",
                            "source-roots": ["/p/src", "/p/src-jvm", "/p/target/generated/debug/protos", "/p/target/generated/debug/config"],
                            "test-roots": ["/p/tests", "/p/tests-jvm", "/p/test-fixtures", "/p/test-fixtures-jvm", "/p/target/generated/debug/mocks"],
                            "generated-roots": [
                                { "name": "protos", "kind": "sources", "dir": "/p/target/generated/debug/protos", "common": false },
                                { "name": "config", "kind": "sources", "dir": "/p/target/generated/debug/config", "common": true },
                                { "name": "mocks", "kind": "test-sources", "dir": "/p/target/generated/debug/mocks", "common": true },
                                { "name": "assets", "kind": "resources", "dir": "/p/target/generated/debug/assets", "common": true },
                            ],
                            "compose-resources": { "source-root": "/p/target/debug/jvm/generated/compose-resources/kotlin" },
                        },
                        {
                            "kind": "wasm",
                            "source-roots": ["/p/src", "/p/src-wasm"],
                            "test-roots": ["/p/tests", "/p/tests-wasm"],
                            "npm-declarations": "/p/target/debug/generated/npm",
                        },
                    ],
                    "examples": [{ "name": "demo", "path": "/p/examples/demo.kt" }],
                },
                { "name": "lib", "path-dependencies": [{ "package": "core" }], "targets": [
                    { "kind": "native", "source-roots": ["/p/lib/src", "/p/lib/src-native"], "test-roots": [] },
                    { "kind": "js", "source-roots": ["/p/lib/src", "/p/lib/src-js"], "test-roots": ["/p/lib/tests", "/p/lib/tests-js"] },
                ] },
                { "name": "core", "targets": [] },
            ],
        });
        let model = model_from_metadata(&meta);
        assert_eq!(model.kotlin, "2.4.21");
        assert_eq!(model.wasm_platform.as_deref(), Some("js"));
        // Loaded as a library, not as sources of the project.
        assert_eq!(model.npm_declarations, [(PathBuf::from("/p/target/debug/generated/npm"), Platforms::WASM)]);
        assert_eq!(model.origin(Path::new("/p/target/debug/generated/npm/express/Default.kt")), Origin::Loose);
        assert_eq!(
            model.packages.platforms,
            [Platforms::JVM.union(Platforms::WASM), Platforms::NATIVE.union(Platforms::JS), Platforms::NONE]
        );
        assert_eq!(model.packages.visible, [vec![0, 1, 2], vec![1, 2], vec![2]]);
        let set = |path: &str| match model.origin(Path::new(path)) {
            Origin::Source(s) => (s.package, s.kind, s.target),
            other => panic!("{path}: {other:?}"),
        };
        assert_eq!(set("/p/src/Main.kt"), (0, SetKind::Main, None));
        assert_eq!(set("/p/src-wasm/a/B.kt"), (0, SetKind::Main, Some("wasm".into())));
        assert_eq!(set("/p/tests-jvm/T.kt"), (0, SetKind::Test, Some("jvm".into())));
        assert_eq!(set("/p/test-fixtures/F.kt"), (0, SetKind::Test, None));
        assert_eq!(set("/p/examples/demo.kt"), (0, SetKind::Example, None));
        assert_eq!(set("/p/target/debug/jvm/generated/compose-resources/kotlin/Res.kt"), (0, SetKind::Main, None));
        assert_eq!(set("/p/lib/src-native/N.kt"), (1, SetKind::Main, Some("native".into())));
        assert_eq!(set("/p/lib/src-js/J.kt"), (1, SetKind::Main, Some("js".into())));
        assert_eq!(set("/p/lib/tests-js/JT.kt"), (1, SetKind::Test, Some("js".into())));
        // What `[[generate]]` steps write: shared by every target, or the target's own.
        assert_eq!(set("/p/target/generated/debug/config/BuildConfig.kt"), (0, SetKind::Main, None));
        assert_eq!(set("/p/target/generated/debug/protos/Person.kt"), (0, SetKind::Main, Some("jvm".into())));
        assert_eq!(set("/p/target/generated/debug/mocks/M.kt"), (0, SetKind::Test, None));
        assert_eq!(model.origin(Path::new("/p/target/generated/debug/assets/a.txt")), Origin::Loose, "resources hold no Kotlin");
        assert_eq!(model.origin(Path::new("/elsewhere/X.kt")), Origin::Loose);
    }

    #[test]
    fn reads_the_intermediate_source_sets_from_metadata() {
        let group = json!({
            "name": "jvmAndAndroid",
            "targets": ["jvm", "android"],
            "refines": [],
            "source-root": "/p/src-jvmAndAndroid",
            "test-root": "/p/tests-jvmAndAndroid",
            "test-fixture-root": "/p/test-fixtures-jvmAndAndroid",
        });
        let meta = json!({
            "packages": [{
                "name": "app",
                "targets": [
                    {
                        "kind": "jvm",
                        "source-roots": ["/p/src", "/p/src-jvm", "/p/src-jvmAndAndroid"],
                        "test-roots": ["/p/tests", "/p/tests-jvm", "/p/test-fixtures", "/p/test-fixtures-jvm", "/p/benches", "/p/benches-jvm",
                            "/p/tests-jvmAndAndroid", "/p/test-fixtures-jvmAndAndroid"],
                        "source-sets": [group],
                    },
                    {
                        "kind": "android",
                        "source-roots": ["/p/src", "/p/src-android", "/p/src-jvmAndAndroid"],
                        "test-roots": ["/p/tests", "/p/tests-android"],
                        "source-sets": [group],
                    },
                ],
            }],
        });
        let model = model_from_metadata(&meta);
        let set = |path: &str| match model.origin(Path::new(path)) {
            Origin::Source(s) => s,
            other => panic!("{path}: {other:?}"),
        };
        // The common and the target's directories keep their places in the pairs.
        assert_eq!(set("/p/src/A.kt").target, None);
        assert_eq!(set("/p/src-jvm/A.kt").target.as_deref(), Some("jvm"));
        assert_eq!(set("/p/src-android/A.kt").target.as_deref(), Some("android"));
        assert_eq!(set("/p/tests-jvm/A.kt").target.as_deref(), Some("jvm"));
        let shared = set("/p/src-jvmAndAndroid/A.kt");
        assert_eq!((shared.kind, shared.group()), (SetKind::Main, Some(("jvmAndAndroid", vec!["jvm", "android"]))));
        assert_eq!(shared.some_target(), Some("jvm"));
        assert_eq!(set("/p/tests-jvmAndAndroid/A.kt").kind, SetKind::Test);
        assert_eq!(set("/p/test-fixtures-jvmAndAndroid/A.kt").kind, SetKind::Test);
        assert_eq!(model.roots.iter().filter(|(r, _)| r == Path::new("/p/src-jvmAndAndroid")).count(), 1);
    }

    #[test]
    fn reads_the_generated_code_of_android_modules() {
        let meta = json!({
            "packages": [{
                "name": "app",
                "targets": [{
                    "kind": "android",
                    "source-roots": ["/p/src", "/p/src-android"],
                    "test-roots": ["/p/tests", "/p/tests-android"],
                    "android": { "generated-source-roots": ["/p/target/android/build/generated/source/buildConfig/debug"] },
                }],
            }],
        });
        let model = model_from_metadata(&meta);
        let file = Path::new("/p/target/android/build/generated/source/buildConfig/debug/com/example/BuildConfig.java");
        match model.origin(file) {
            Origin::Source(s) => assert_eq!((s.package, s.kind, s.target), (0, SetKind::Main, Some("android".into()))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_the_android_platform_and_libraries_of_android_targets() {
        let entry = |module: &str, path: &str| json!({ "module": module, "version": "1.0", "path": path, "sources": null });
        let meta = json!({
            "packages": [{
                "name": "droid",
                "targets": [{
                    "kind": "android",
                    "android": { "platform-jar": "/sdk/platforms/android-36/android.jar" },
                    "dependencies": [{ "platform": "android", "compile": [
                        entry("androidx.core:core", "/m/core-1.0.aar"),
                        entry("com.squareup.moshi:moshi", "/m/moshi-1.0.jar"),
                    ], "test": [] }],
                }],
            }],
        });
        let libraries = dependency_compiled(&meta);
        let names: Vec<(&str, Platforms)> = libraries.iter().map(|l| (l.name.as_str(), l.main)).collect();
        assert_eq!(
            names,
            [("core 1.0", Platforms::JVM), ("moshi 1.0", Platforms::JVM), ("Android SDK (android-36)", Platforms::JVM)],
            "{names:?}"
        );
        assert!(libraries[0].path.ends_with("core-1.0.aar") && libraries[2].path.ends_with("android.jar"));
        // A project without the platform's jar (the SDK doesn't have it yet) just has no Android SDK.
        let meta = json!({ "packages": [{ "targets": [{ "kind": "android", "android": { "platform-jar": null } }] }] });
        assert!(dependency_compiled(&meta).is_empty());
    }

    #[test]
    fn reads_compiled_libraries_without_sources_jars() {
        let home = tempfile::tempdir().unwrap();
        let platform = home.path().join("klib").join("platform").join("linux_x64");
        for lib in ["org.jetbrains.kotlin.native.platform.posix", "org.jetbrains.kotlin.native.platform.zlib"] {
            std::fs::create_dir_all(platform.join(lib)).unwrap();
        }
        // A cinterop klib comes with its module's sources jar, like the module's own klib.
        let deps = tempfile::tempdir().unwrap();
        let interop = deps.path().join("e-cinterop-c.klib");
        let own = deps.path().join("e.klib");
        for (klib, manifest) in [
            (
                &interop,
                "interop=true
package=c
",
            ),
            (
                &own,
                "unique_name=e
",
            ),
        ] {
            std::fs::create_dir_all(klib.join("default")).unwrap();
            std::fs::write(klib.join("default").join("manifest"), manifest).unwrap();
        }
        let (interop, own) = (interop.display().to_string(), own.display().to_string());
        let entry = |module: &str, path: &str, sources: Option<&str>| json!({ "module": module, "version": "1.0", "path": path, "sources": sources });
        let meta = json!({
            "toolchain": { "kotlin-native": { "home": home.path().display().to_string() } },
            "packages": [{
                "name": "app",
                "targets": [
                    { "kind": "jvm", "dependencies": [{ "platform": "jvm", "compile": [
                        entry("org.jetbrains.kotlin:kotlin-stdlib", "/m/kotlin-stdlib.jar", None),
                        entry("com.example:with-sources", "/m/a.jar", Some("/m/a-sources.jar")),
                        entry("com.example:without", "/m/b.jar", None),
                        entry("com.example:without", "/m/b.jar", None),
                    ], "test": [entry("com.example:tested", "/m/c.jar", None)] }] },
                    { "kind": "native", "platforms": ["linux_x64"], "dependencies": [{ "platform": "native-linux_x64", "compile": [
                        entry("org.jetbrains.kotlin:kotlin-stdlib-native", "/m/stdlib.klib", None),
                        entry("com.example:native", "/m/d.klib", None),
                        entry("com.example:e", &own, Some("/m/e-sources.jar")),
                        entry("com.example:e", &interop, Some("/m/e-sources.jar")),
                    ] }] },
                ],
            }],
        });
        assert!(
            dependency_jars(&meta).iter().all(|j| j.jar == Path::new("/m/a-sources.jar") || j.jar == Path::new("/m/e-sources.jar")),
            "only sources jars are listed"
        );
        let libs: Vec<(String, Platforms, Platforms)> = dependency_compiled(&meta).into_iter().map(|c| (c.name, c.main, c.test)).collect();
        let expected = [
            ("native 1.0", Platforms::NATIVE, Platforms::NONE),
            ("tested 1.0", Platforms::NONE, Platforms::JVM),
            ("without 1.0", Platforms::JVM, Platforms::NONE),
            ("e 1.0 (cinterop)", Platforms::NATIVE, Platforms::NONE),
            ("Kotlin/Native posix (linux_x64)", Platforms::NATIVE, Platforms::NONE),
            ("Kotlin/Native zlib (linux_x64)", Platforms::NATIVE, Platforms::NONE),
        ];
        let mut names: Vec<&str> = libs.iter().map(|(n, ..)| n.as_str()).collect();
        // Dependencies come in path order, then the platform libraries.
        names[..4].sort();
        assert_eq!(
            names,
            [
                "e 1.0 (cinterop)",
                "native 1.0",
                "tested 1.0",
                "without 1.0",
                "Kotlin/Native posix (linux_x64)",
                "Kotlin/Native zlib (linux_x64)"
            ]
        );
        for (name, main, test) in expected {
            assert_eq!(libs.iter().find(|(n, ..)| n == name).map(|(_, m, t)| (*m, *t)), Some((main, test)), "{name}");
        }
    }

    #[test]
    fn reads_the_native_stdlib_for_what_its_sources_lack() {
        let home = tempfile::tempdir().unwrap();
        let meta = json!({ "toolchain": { "kotlin-native": { "home": home.path().display().to_string() } } });
        assert!(native_stdlib(&meta, &[]).is_none(), "no distribution, no library");
        std::fs::create_dir_all(home.path().join("klib").join("common").join("stdlib")).unwrap();
        let unit = |path: &str, text: &str, main: Platforms| {
            Arc::new(Unit::new(PathBuf::from(path), text.into(), Origin::Library { name: "stdlib".into(), main, test: Platforms::NONE }))
        };
        let stdlib = [
            unit(
                "/s/a.kt",
                "package kotlin.io\nfun println(message: Any?) {}\nprivate fun hidden() {}",
                Platforms::WASM.union(Platforms::NATIVE),
            ),
            unit("/s/b.kt", "package kotlin.js\nclass JsAny", Platforms::WASM),
        ];
        let library = native_stdlib(&meta, &stdlib).unwrap();
        assert_eq!(library.main, Platforms::NATIVE);
        let known: Vec<&str> = library.known.as_deref().unwrap().iter().map(String::as_str).collect();
        assert_eq!(known, ["kotlin.io.println"], "only public declarations the sources give Native code");
    }

    #[test]
    fn lists_the_dependencies_missing_sources_jars() {
        let entry = |module: &str, path: &str, sources: Option<&str>| json!({ "module": module, "version": "1.0", "path": path, "sources": sources });
        let meta = json!({ "packages": [{ "name": "app", "targets": [{ "kind": "jvm", "dependencies": [{
            "compile": [
                entry("org.jetbrains.kotlin:kotlin-stdlib", "/m/kotlin-stdlib.jar", None),
                entry("com.example:with-sources", "/m/a.jar", Some("/m/a-sources.jar")),
                entry("com.example:without", "/m/b.jar", None),
                entry("com.example:pom-only", "/m/c.pom", None),
            ],
            "test": [entry("com.example:without", "/m/b.jar", None), entry("com.example:tested", "/m/d.jar", None)],
        }] }] }] });
        let missing = missing_sources(&meta);
        assert_eq!(missing, [("com.example:tested".to_owned(), "1.0".to_owned()), ("com.example:without".to_owned(), "1.0".to_owned())]);
    }

    /// Writes `files` to a zip at `path`.
    fn zip(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, text) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// Publishes `group:artifact:version` with a POM and a jar (and a sources jar with `sources`) to a Maven repository.
    fn publish(repo: &Path, group: &str, artifact: &str, version: &str, sources: &[(&str, &str)]) {
        let dir = repo.join(group.replace('.', "/")).join(artifact).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let pom = format!("<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version></project>");
        std::fs::write(dir.join(format!("{artifact}-{version}.pom")), pom).unwrap();
        zip(
            &dir.join(format!("{artifact}-{version}.jar")),
            &[(
                "META-INF/MANIFEST.MF",
                "Manifest-Version: 1.0
",
            )],
        );
        if !sources.is_empty() {
            zip(&dir.join(format!("{artifact}-{version}-sources.jar")), sources);
        }
    }

    #[test]
    fn downloads_missing_sources_jars_and_completes_from_them() {
        let repo = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        publish(
            repo.path(),
            "g",
            "lib",
            "1.0",
            &[(
                "g/Greeter.kt",
                "package g

class Greeter {
    fun greet(): String = \"hi\"
}
",
            )],
        );
        for artifact in ["kotlin-stdlib", "kotlin-test-junit5"] {
            publish(repo.path(), "org.jetbrains.kotlin", artifact, kargo_manifest::DEFAULT_KOTLIN_VERSION, &[]);
        }
        publish(repo.path(), "org.junit.jupiter", "junit-jupiter", kargo_ops::builtins::JUNIT_VERSION, &[]);
        let url = format!("file:///{}", repo.path().display().to_string().replace('\\', "/").trim_start_matches('/'));
        let manifest = format!(
            "[package]
name = \"app\"
version = \"0.1.0\"
[dependencies]
\"g:lib\" = \"1.0\"
[repositories]
local = \"{url}\"
"
        );
        std::fs::write(dir.path().join("kargo.toml"), manifest).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let main = "fun main() {
    g.Greeter().
}
";
        std::fs::write(dir.path().join("src/Main.kt"), main).unwrap();

        let config =
            Config::new(home.path().to_path_buf(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet)).with_offline(true);
        let (server_in, mut input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let mut output = BufReader::new(output);
        let server = thread::spawn(move || serve(config, BufReader::new(server_in), server_out));
        let mut send = |message: Value| jsonrpc::write(&mut input, &message).unwrap();
        let capabilities = json!({ "window": { "workDoneProgress": true } });
        send(jsonrpc::request("1", "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": capabilities })));
        send(jsonrpc::notification("initialized", json!({})));
        let uri = text::path_to_uri(&dir.path().join("src/Main.kt"));
        send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": main } }),
        ));

        // The download is reported with `$/progress` once the editor accepts the token.
        let mut progress = Vec::new();
        let mut loading = Vec::new();
        loop {
            let message = jsonrpc::read(&mut output).unwrap().expect("a message");
            if message["method"] == "window/workDoneProgress/create" {
                send(jsonrpc::response(&message["id"], Value::Null));
            } else if message["method"] == "$/progress" {
                let value = message["params"]["value"].clone();
                if message["params"]["token"].as_str().is_some_and(|t| t.ends_with("/load")) {
                    loading.push(value);
                    continue;
                }
                progress.push(value);
                if message["params"]["value"]["kind"] == "end" {
                    break;
                }
            }
        }
        // Loading the project is reported before the download, and has ended by then.
        assert_eq!(loading[0]["title"], "Loading Kotlin project", "{loading:?}");
        assert_eq!(loading.last().unwrap()["kind"], "end", "{loading:?}");
        assert_eq!(progress[0]["title"], "Fetching dependency sources", "{progress:?}");
        // g:lib, kotlin-test-junit5 and junit-jupiter lack one; only g:lib publishes one.
        assert_eq!(progress[0]["message"], "3 libraries", "{progress:?}");
        assert_eq!(progress.last().unwrap()["message"], "1 sources or Javadoc jar", "{progress:?}");

        // The library loads again, now from its sources, without a restart.
        let position = json!({ "line": 1, "character": 16 });
        let mut id = 2;
        let labels = loop {
            send(jsonrpc::request(
                &id.to_string(),
                "textDocument/completion",
                json!({ "textDocument": { "uri": uri }, "position": position }),
            ));
            let reply = loop {
                let message = jsonrpc::read(&mut output).unwrap().expect("a message");
                if message["id"].as_str() == Some(id.to_string().as_str()) && message.get("method").is_none() {
                    break message;
                }
            };
            let labels: Vec<String> =
                reply["result"]["items"].as_array().into_iter().flatten().filter_map(|i| i["label"].as_str().map(str::to_owned)).collect();
            if labels.iter().any(|l| l.starts_with("greet")) || id > 100 {
                break labels;
            }
            id += 1;
            thread::sleep(Duration::from_millis(50));
        };
        assert!(labels.iter().any(|l| l.starts_with("greet")), "{labels:?}");
        let fetched = home.path();
        assert!(
            walkdir::WalkDir::new(fetched).into_iter().flatten().any(|e| e.file_name() == "lib-1.0-sources.jar"),
            "cached in {}",
            fetched.display()
        );

        send(jsonrpc::request("999", "shutdown", Value::Null));
        send(jsonrpc::notification("exit", Value::Null));
        server.join().unwrap().unwrap();
    }

    #[test]
    fn resolves_a_scripts_dependencies_for_the_script_alone() {
        let repo = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        publish(
            repo.path(),
            "g",
            "lib",
            "1.0",
            &[(
                "g/Greeter.kt",
                "package g

class Greeter {
    fun greet(): String = \"hi\"
}
",
            )],
        );
        publish(repo.path(), "org.jetbrains.kotlin", "kotlin-stdlib", kargo_manifest::DEFAULT_KOTLIN_VERSION, &[]);
        // Maven Central is mirrored to the repository, so the script's dependency resolves offline.
        let url = format!("file:///{}", repo.path().display().to_string().replace('\\', "/").trim_start_matches('/'));
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[mirrors]
\"*\" = \"{url}\"
"
            ),
        )
        .unwrap();
        let settings = kargo_util::settings::Settings::load(home.path(), None).unwrap();
        let config = Config::new(home.path().to_path_buf(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet))
            .with_settings(settings)
            .unwrap()
            .with_offline(true);

        // A script outside any project: its folder has no kargo.toml.
        let script = "@file:DependsOn(\"g:lib:1.0\")
val greeter = g.Greeter().
";
        let uri = text::path_to_uri(&dir.path().join("run.kts"));
        let (server_in, mut input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let mut output = BufReader::new(output);
        let server = thread::spawn(move || serve(config, BufReader::new(server_in), server_out));
        let mut send = |message: Value| jsonrpc::write(&mut input, &message).unwrap();
        send(jsonrpc::request("1", "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} })));
        send(jsonrpc::notification("initialized", json!({})));
        send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": script } }),
        ));

        // The dependency resolves in the background, so the completion is asked again until it has.
        let position = json!({ "line": 1, "character": 26 });
        let mut id = 2;
        let labels = loop {
            send(jsonrpc::request(
                &id.to_string(),
                "textDocument/completion",
                json!({ "textDocument": { "uri": uri }, "position": position }),
            ));
            let reply = loop {
                let message = jsonrpc::read(&mut output).unwrap().expect("a message");
                if message["id"].as_str() == Some(id.to_string().as_str()) && message.get("method").is_none() {
                    break message;
                }
            };
            let labels: Vec<String> =
                reply["result"]["items"].as_array().into_iter().flatten().filter_map(|i| i["label"].as_str().map(str::to_owned)).collect();
            if labels.iter().any(|l| l.starts_with("greet")) || id > 200 {
                break labels;
            }
            id += 1;
            thread::sleep(Duration::from_millis(50));
        };
        assert!(labels.iter().any(|l| l.starts_with("greet")), "{labels:?}");

        send(jsonrpc::request("999", "shutdown", Value::Null));
        send(jsonrpc::notification("exit", Value::Null));
        server.join().unwrap().unwrap();
    }

    #[test]
    fn scripts_are_loose_documents_and_gradle_scripts_are_not_kotlin() {
        // A `.kts` under a source root is not compiled by Kargo, so it is loose too.
        let model = Model {
            roots: vec![(PathBuf::from("/p/src"), SourceSet { package: 0, kind: SetKind::Main, target: None })],
            ..Model::default()
        };
        assert_eq!(model.origin(Path::new("/p/src/run.kts")), Origin::Loose);
        assert!(matches!(model.origin(Path::new("/p/src/Main.kt")), Origin::Source(_)));
        assert!(is_script_path(Path::new("/p/fetch.kts")));
        assert!(!is_script_path(Path::new("/p/build.gradle.kts")));
        assert!(!is_script_path(Path::new("/p/settings.gradle.kts")));
        // Outside a project a script is one; inside it, only one whose shebang runs `kargo script`.
        assert!(is_kotlin_document(Path::new("/p/fetch.kts"), "println(1)"));
        assert!(is_kotlin_document(Path::new("/p/Main.kt"), "fun main() {}"));
        assert!(!is_kotlin_document(Path::new("/p/build.gradle.kts"), "plugins {}"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("kargo.toml"),
            "[package]
name = \"app\"
version = \"0.1.0\"
",
        )
        .unwrap();
        assert!(!is_kotlin_document(&dir.path().join("build.kts"), "println(1)"));
        assert!(is_kotlin_document(
            &dir.path().join("run.kts"),
            "#!/usr/bin/env -S kargo script
println(1)"
        ));
    }

    #[test]
    fn upper_cases_drive_letters() {
        assert_eq!(file_path("file:///c%3A/p/src/Main.kt"), Some(PathBuf::from("C:/p/src/Main.kt")));
        assert_eq!(file_path("file:///home/me/Main.kt"), Some(PathBuf::from("/home/me/Main.kt")));
    }

    #[test]
    fn dependency_jars_are_on_the_platforms_and_scopes_of_their_classpaths() {
        let meta = json!({
            "packages": [{
                "name": "app",
                "targets": [
                    { "kind": "jvm", "dependencies": [{
                        "platform": "jvm",
                        "compile": [
                            { "module": "g:lib", "version": "1.0", "sources": "/c/lib-jvm-sources.jar" },
                            { "module": "org.jetbrains.kotlin:kotlin-stdlib", "version": "2.4.21", "sources": "/c/stdlib-sources.jar" },
                        ],
                        "test": [
                            { "module": "g:lib", "version": "1.0", "sources": "/c/lib-jvm-sources.jar" },
                            { "module": "g:check", "version": "2.0", "sources": "/c/check-sources.jar" },
                            { "module": "org.jetbrains.kotlin:kotlin-test", "version": "2.4.21", "sources": "/c/test-sources.jar" },
                            { "module": "g:unfetched", "version": "1.0", "sources": null },
                        ],
                    }]},
                    { "kind": "wasm", "dependencies": [{
                        "platform": "wasm-js",
                        "compile": [{ "module": "g:lib", "version": "1.0", "sources": "/c/lib-wasm-sources.jar" }],
                        "test": [{ "module": "g:lib", "version": "1.0", "sources": "/c/lib-wasm-sources.jar" }],
                    }]},
                    { "kind": "android", "dependencies": null },
                ],
            }],
        });
        let jars = dependency_jars(&meta);
        let found: Vec<(&str, Platforms, Platforms)> = jars.iter().map(|j| (j.jar.to_str().unwrap(), j.main, j.test)).collect();
        assert_eq!(
            found,
            [
                ("/c/check-sources.jar", Platforms::NONE, Platforms::JVM),
                ("/c/lib-jvm-sources.jar", Platforms::JVM, Platforms::NONE),
                ("/c/lib-wasm-sources.jar", Platforms::WASM, Platforms::NONE),
            ]
        );
    }

    /// An editor talking to the server with a formatter of the test's own.
    struct Editor {
        input: io::PipeWriter,
        output: BufReader<io::PipeReader>,
        server: Option<thread::JoinHandle<anyhow::Result<()>>>,
    }

    impl Editor {
        fn start(formatter: Formatter) -> Self {
            let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
            let (server_in, input) = io::pipe().unwrap();
            let (output, server_out) = io::pipe().unwrap();
            let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, formatter));
            let mut editor = Self { input, output: BufReader::new(output), server: Some(server) };
            editor.send(jsonrpc::request("0", "initialize", json!({ "capabilities": {} })));
            let init = editor.reply("0");
            assert_eq!(init["result"]["capabilities"]["documentFormattingProvider"], true, "{init}");
            editor
        }

        fn send(&mut self, message: Value) {
            jsonrpc::write(&mut self.input, &message).unwrap();
        }

        fn reply(&mut self, id: &str) -> Value {
            loop {
                let message = jsonrpc::read(&mut self.output).unwrap().expect("a message");
                if message["id"].as_str() == Some(id) && message.get("method").is_none() {
                    return message;
                }
            }
        }

        /// The next message of `method` the server sent.
        fn notification(&mut self, method: &str) -> Value {
            loop {
                let message = jsonrpc::read(&mut self.output).unwrap().expect("a message");
                if message["method"] == method {
                    return message;
                }
            }
        }

        fn open(&mut self, uri: &str, text: &str) {
            let document = json!({ "uri": uri, "languageId": "kotlin", "version": 1, "text": text });
            self.send(jsonrpc::notification("textDocument/didOpen", json!({ "textDocument": document })));
        }

        fn format(&mut self, id: &str, uri: &str) {
            self.send(jsonrpc::request(
                id,
                "textDocument/formatting",
                json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 4 } }),
            ));
        }

        fn finish(mut self) {
            self.send(jsonrpc::request("shutdown", "shutdown", Value::Null));
            self.send(jsonrpc::notification("exit", Value::Null));
            self.server.take().unwrap().join().unwrap().unwrap();
        }
    }

    #[test]
    fn finds_references_on_a_worker_thread_with_progress() {
        let mut editor = Editor::start(Arc::new(|_, _, _| Ok(None)));
        let (a, b) = ("file:///p/src/A.kt", "file:///p/src/B.kt");
        editor.open(a, "package p\n\nfun greet() = 1\n");
        editor.open(b, "package p\n\nfun main() { greet() }\n");
        let params = json!({
            "textDocument": { "uri": a },
            "position": { "line": 2, "character": 5 },
            "context": { "includeDeclaration": true },
            "workDoneToken": "refs",
        });
        editor.send(jsonrpc::request("1", "textDocument/references", params));
        let begin = editor.notification("$/progress");
        assert_eq!(begin["params"]["value"]["kind"], "begin", "{begin}");
        assert_eq!(begin["params"]["value"]["cancellable"], true);
        let reply = editor.reply("1");
        assert_eq!(reply["result"][0]["uri"], a, "{reply}");
        editor.finish();
    }

    #[test]
    fn searches_workspace_symbols_on_a_worker_and_resolves_them() {
        let mut editor = Editor::start(Arc::new(|_, _, _| Ok(None)));
        let a = "file:///p/src/A.kt";
        editor.open(a, "package p\n\nfun greet() = 1\n");
        editor.send(jsonrpc::request("1", "workspace/symbol", json!({ "query": "greet" })));
        let reply = editor.reply("1");
        assert_eq!(reply["result"][0]["name"], "greet", "{reply}");
        assert!(reply["result"][0]["location"]["range"].is_object(), "{reply}");
        // A cancelled search answers with the cancellation error.
        editor.send(jsonrpc::request("2", "workspace/symbol", json!({ "query": "greet" })));
        editor.send(jsonrpc::notification("$/cancelRequest", json!({ "id": "2" })));
        let reply = editor.reply("2");
        assert!(reply["result"].is_array() || reply["error"]["code"] == REQUEST_CANCELLED, "{reply}");
        editor.finish();
    }

    #[test]
    fn resolves_workspace_symbol_ranges_when_the_editor_asks_for_it() {
        let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, Arc::new(|_, _, _| Ok(None))));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        let capabilities = json!({ "workspace": { "symbol": { "resolveSupport": { "properties": ["location.range"] } } } });
        editor.send(jsonrpc::request("0", "initialize", json!({ "capabilities": capabilities })));
        let init = editor.reply("0");
        assert_eq!(init["result"]["capabilities"]["workspaceSymbolProvider"]["resolveProvider"], true);
        assert_eq!(init["result"]["capabilities"]["positionEncoding"], "utf-16");
        editor.open("file:///p/src/A.kt", "package p\n\nfun greet() = 1\n");
        editor.send(jsonrpc::request("1", "workspace/symbol", json!({ "query": "greet" })));
        let symbol = editor.reply("1")["result"][0].clone();
        assert!(symbol["location"]["range"].is_null(), "{symbol}");
        editor.send(jsonrpc::request("2", "workspaceSymbol/resolve", symbol));
        let resolved = editor.reply("2");
        assert_eq!(resolved["result"]["location"]["range"]["start"], json!({ "line": 2, "character": 4 }), "{resolved}");
        editor.finish();
    }

    #[test]
    fn formats_a_file_from_its_code_action() {
        let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let formatter: Formatter = Arc::new(|_, _, text| Ok(Some(text.replace("fun  main", "fun main"))));
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, formatter));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        editor.send(jsonrpc::request("0", "initialize", json!({ "capabilities": { "workspace": { "applyEdit": true } } })));
        let init = editor.reply("0");
        let capabilities = &init["result"]["capabilities"];
        assert_eq!(capabilities["executeCommandProvider"]["commands"], json!([FORMAT_COMMAND]), "{init}");
        assert_eq!(capabilities["documentOnTypeFormattingProvider"]["firstTriggerCharacter"], "\n");
        assert_eq!(capabilities["experimental"]["kargo"]["testTree"], true);
        let uri = "file:///p/src/A.kt";
        editor.open(uri, "package p\n\nfun  main() {}\n");
        let range = json!({ "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 0 } });
        editor.send(jsonrpc::request(
            "1",
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": [], "only": ["source"] } }),
        ));
        let actions = editor.reply("1");
        let action = actions["result"].as_array().unwrap().iter().find(|a| a["kind"] == actions::FORMAT).cloned();
        let command = action.expect("the format action")["command"].clone();
        editor.send(jsonrpc::request("2", "workspace/executeCommand", command));
        let apply = editor.notification("workspace/applyEdit");
        let edits = &apply["params"]["edit"]["changes"][uri];
        assert_eq!(edits[0]["newText"], "", "{apply}");
        assert_eq!(edits[0]["range"]["start"], json!({ "line": 2, "character": 4 }), "{apply}");
        assert!(editor.reply("2")["result"].is_null());
        editor.finish();
    }

    #[test]
    fn hears_of_renamed_and_deleted_files_at_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src/old")).unwrap();
        std::fs::write(dir.path().join("src/old/A.kt"), "package p\n\nfun zzalpha() = 1\n").unwrap();
        let config = Config::new(dir.path().join("home"), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, Arc::new(|_, _, _| Ok(None))));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        let params = json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {}, "initializationOptions": { "status": true } });
        editor.send(jsonrpc::request("0", "initialize", params));
        editor.reply("0");
        let status = editor.notification("kargo/status");
        assert_eq!(status["params"], json!({ "task": "indexing", "active": true }));
        let status = editor.notification("kargo/status");
        assert_eq!(status["params"], json!({ "task": "indexing", "active": false }));
        fn uris(editor: &mut Editor, id: &str) -> Vec<String> {
            editor.send(jsonrpc::request(id, "workspace/symbol", json!({ "query": "zzalpha" })));
            let reply = editor.reply(id);
            match reply["result"].as_array() {
                Some(symbols) => symbols.iter().map(|s| s["location"]["uri"].as_str().unwrap().to_owned()).collect(),
                None => uris(editor, &format!("{id}-again")),
            }
        }
        assert_eq!(uris(&mut editor, "1"), [text::path_to_uri(&dir.path().join("src/old/A.kt"))]);
        // A folder renamed: its files move with it.
        std::fs::rename(dir.path().join("src/old"), dir.path().join("src/new")).unwrap();
        let (old, new) = (text::path_to_uri(&dir.path().join("src/old")), text::path_to_uri(&dir.path().join("src/new")));
        editor.send(jsonrpc::notification("workspace/didRenameFiles", json!({ "files": [{ "oldUri": old, "newUri": new }] })));
        assert_eq!(uris(&mut editor, "2"), [text::path_to_uri(&dir.path().join("src/new/A.kt"))]);
        let file = dir.path().join("src/new/A.kt");
        std::fs::remove_file(&file).unwrap();
        editor.send(jsonrpc::notification("workspace/didDeleteFiles", json!({ "files": [{ "uri": text::path_to_uri(&file) }] })));
        assert!(uris(&mut editor, "3").is_empty());
        editor.finish();
    }

    #[test]
    fn rescans_the_source_roots_when_the_editor_does_not_watch_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/A.kt"), "package p\n\nfun zzalpha() = 1\n").unwrap();
        let config = Config::new(dir.path().join("home"), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, Arc::new(|_, _, _| Ok(None))));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        editor.send(jsonrpc::request("0", "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} })));
        editor.reply("0");
        fn names(editor: &mut Editor, id: &str, query: &str) -> Vec<String> {
            editor.send(jsonrpc::request(id, "workspace/symbol", json!({ "query": query })));
            let reply = editor.reply(id);
            // A search that overlaps a rescan fails with ContentModified; ask again.
            match reply["result"].as_array() {
                Some(symbols) => symbols.iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect(),
                None => names(editor, &format!("{id}-again"), query),
            }
        }
        assert_eq!(names(&mut editor, "1", "zzalpha"), ["zzalpha"]);
        // A new file and a deleted one are noticed on save, without any watcher.
        std::fs::write(dir.path().join("src/B.kt"), "package p\n\nfun zzbeta() = 1\n").unwrap();
        std::fs::remove_file(dir.path().join("src/A.kt")).unwrap();
        let saved = text::path_to_uri(&dir.path().join("src/B.kt"));
        editor.send(jsonrpc::notification("textDocument/didSave", json!({ "textDocument": { "uri": saved } })));
        let mut found = Vec::new();
        for n in 0..100 {
            found = names(&mut editor, &format!("q{n}"), "zzbeta");
            if !found.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(found, ["zzbeta"]);
        assert!(names(&mut editor, "last", "zzalpha").is_empty());
        editor.finish();
    }

    #[test]
    fn saved_typescript_modules_resolve_in_kotlin_without_a_build() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.wasm]\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("js")).unwrap();
        let sum = dir.path().join("js/sum.ts");
        std::fs::write(&sum, "export function zzsum(a: number, b: number): number { return a + b; }\n").unwrap();
        let main = "import js.sum.*\n\nfun main() {\n    zzsum(1, 2)\n    zzdiff(1, 2)\n}\n";
        std::fs::write(dir.path().join("src/Main.kt"), main).unwrap();
        let config = Config::new(dir.path().join("home"), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, Arc::new(|_, _, _| Ok(None))));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        editor.send(jsonrpc::request("0", "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} })));
        editor.reply("0");
        let uri = text::path_to_uri(&dir.path().join("src/Main.kt"));
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": main } }),
        ));
        // Where the call on `line` goes, once the libraries are in: the generated declarations, or nowhere.
        let definition = |editor: &mut Editor, id: &str, line: u32| -> Option<String> {
            editor.send(jsonrpc::request(
                id,
                "textDocument/definition",
                json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": 6 } }),
            ));
            let reply = editor.reply(id);
            let result = &reply["result"];
            let location = if result.is_array() { result.get(0).cloned().unwrap_or(Value::Null) } else { result.clone() };
            location["uri"].as_str().map(str::to_owned)
        };
        let wait = |editor: &mut Editor, name: &str, line: u32, found: bool| -> Option<String> {
            let mut last = None;
            for n in 0..100 {
                last = definition(editor, &format!("{name}{n}"), line);
                if last.is_some() == found {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            last
        };
        let target = wait(&mut editor, "a", 3, true).expect("`zzsum` resolves to its generated declaration");
        assert!(target.ends_with("generated/js-sources/js/sum/Exports.kt"), "{target}");
        let exports = dir.path().join("target/debug/generated/js-sources/js/sum/Exports.kt");
        let written = std::fs::metadata(&exports).unwrap().modified().unwrap();
        assert_eq!(definition(&mut editor, "b", 4), None, "`zzdiff` isn't declared yet");

        // Saving a new function makes it resolve: the declarations are generated again, without a build.
        std::fs::write(&sum, "export function zzsum(a: number, b: number): number { return a + b; }\nexport function zzdiff(a: number, b: number): number { return a - b; }\n").unwrap();
        let changes = json!({ "changes": [{ "uri": text::path_to_uri(&sum), "type": 2 }] });
        editor.send(jsonrpc::notification("workspace/didChangeWatchedFiles", changes.clone()));
        assert!(wait(&mut editor, "c", 4, true).is_some(), "`zzdiff` resolves after the save");
        assert!(std::fs::read_to_string(&exports).unwrap().contains("external fun zzdiff("));
        assert_ne!(std::fs::metadata(&exports).unwrap().modified().unwrap(), written);

        // And a removed one stops resolving.
        std::fs::write(&sum, "export function zzsum(a: number, b: number): number { return a + b; }\n").unwrap();
        editor.send(jsonrpc::notification("workspace/didChangeWatchedFiles", changes));
        assert_eq!(wait(&mut editor, "d", 4, false), None, "`zzdiff` is gone after the save");
        assert!(definition(&mut editor, "e", 3).is_some());
        editor.finish();
    }

    #[test]
    fn serves_every_workspace_folder_as_its_own_project() {
        let home = tempfile::tempdir().unwrap();
        let project = |name: &str, function: &str| {
            let dir = home.path().join(name);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("kargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")).unwrap();
            std::fs::write(dir.join("src/Main.kt"), format!("package p\n\nfun {function}() = 1\n")).unwrap();
            dir
        };
        let (one, two, three) = (project("one", "zzalpha"), project("two", "zzbeta"), project("three", "zzgamma"));
        let config = Config::new(home.path().join("home"), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (server_in, input) = io::pipe().unwrap();
        let (output, server_out) = io::pipe().unwrap();
        let server = thread::spawn(move || serve_with(config, BufReader::new(server_in), server_out, false, Arc::new(|_, _, _| Ok(None))));
        let mut editor = Editor { input, output: BufReader::new(output), server: Some(server) };
        let folder = |dir: &Path| json!({ "uri": text::path_to_uri(dir), "name": "x" });
        let folders = json!([folder(&one), folder(&two)]);
        editor.send(jsonrpc::request("0", "initialize", json!({ "workspaceFolders": folders, "capabilities": {} })));
        let init = editor.reply("0");
        assert_eq!(init["result"]["capabilities"]["workspace"]["workspaceFolders"]["supported"], true, "{init}");
        editor.send(jsonrpc::notification("initialized", json!({})));

        let symbols = |editor: &mut Editor, id: &str| -> Vec<String> {
            editor.send(jsonrpc::request(id, "workspace/symbol", json!({ "query": "zz" })));
            let reply = editor.reply(id);
            let mut names: Vec<String> =
                reply["result"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect();
            names.sort();
            names
        };
        // Both projects answer one search.
        assert_eq!(symbols(&mut editor, "1"), ["zzalpha", "zzbeta"]);

        // A file's requests go to its own project.
        let main_two = text::path_to_uri(&two.join("src/Main.kt"));
        editor.send(jsonrpc::request("2", "textDocument/documentSymbol", json!({ "textDocument": { "uri": main_two } })));
        assert_eq!(editor.reply("2")["result"][0]["name"], "zzbeta");

        // A folder the editor opens later is served too, and a closed one is dropped.
        let added = json!({ "event": { "added": [folder(&three)], "removed": [folder(&one)] } });
        editor.send(jsonrpc::notification("workspace/didChangeWorkspaceFolders", added));
        assert_eq!(symbols(&mut editor, "3"), ["zzbeta", "zzgamma"]);
        editor.finish();
    }

    #[test]
    fn a_cancelled_search_gives_up() {
        let unit = Arc::new(Unit::new(PathBuf::from("/p/src/A.kt"), "fun f() = 1\nfun g() = f()\n".to_owned(), Origin::Loose));
        let lib = Index::new(true, Vec::new());
        let proj = Index::new(false, vec![unit]);
        let packages = Packages { visible: Vec::new(), platforms: Vec::new() };
        let an = Analyzer::new(&lib, &proj, &packages);
        let group = features::rename_group(&an, UnitId { lib: false, unit: 0 }.sym(0));
        assert_eq!(features::search(&an, &group, true, &features::Control::default()).map(|r| r.len()), Some(2));
        let cancelled = || true;
        let control = features::Control { cancelled: Some(&cancelled), progress: None };
        assert!(features::search(&an, &group, true, &control).is_none());
    }

    #[test]
    fn formats_a_document_with_one_minimal_edit() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let mut editor = Editor::start(Arc::new(move |_, path, text| {
            log.lock().unwrap().push((path.to_path_buf(), text.to_owned()));
            Ok(Some(text.replace("fun   main", "fun main")))
        }));
        let uri = "file:///p/src/Main.kt";
        editor.open(uri, "fun   main() {}\n");
        editor.format("1", uri);
        let reply = editor.reply("1");
        assert_eq!(
            reply["result"],
            json!([{ "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 6 } }, "newText": "" }]),
            "{reply}"
        );
        assert_eq!(seen.lock().unwrap().as_slice(), [(PathBuf::from("/p/src/Main.kt"), "fun   main() {}\n".to_owned())]);
        editor.finish();
    }

    #[test]
    fn formatting_needs_no_edit_for_formatted_text_and_excluded_files() {
        let mut editor =
            Editor::start(Arc::new(|_, path, text| Ok(if path.ends_with("Generated.kt") { None } else { Some(text.to_owned()) })));
        for (id, name) in [("1", "Main.kt"), ("2", "Generated.kt")] {
            let uri = format!("file:///p/src/{name}");
            editor.open(&uri, "fun   main() {}\n");
            editor.format(id, &uri);
            assert_eq!(editor.reply(id)["result"], json!([]), "{name}");
        }
        // A document the editor did not open has nothing to format.
        editor.format("3", "file:///p/src/Closed.kt");
        assert_eq!(editor.reply("3")["result"], Value::Null);
        editor.finish();
    }

    #[test]
    fn formatting_failures_are_reported_to_the_editor() {
        let mut editor = Editor::start(Arc::new(|_, _, _| anyhow::bail!("ktlint could not format Main.kt: Not a valid Kotlin file")));
        let uri = "file:///p/src/Main.kt";
        editor.open(uri, "fun main( {\n");
        editor.format("1", uri);
        let reply = editor.reply("1");
        assert_eq!(reply["error"]["code"], REQUEST_FAILED, "{reply}");
        assert!(reply["error"]["message"].as_str().unwrap().contains("Not a valid Kotlin file"), "{reply}");
        editor.finish();
    }

    #[test]
    fn formatting_stale_text_is_rejected_and_the_server_stays_responsive() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Mutex::new(gate);
        let mut editor = Editor::start(Arc::new(move |_, _, text| {
            let _ = gate.lock().unwrap().recv();
            Ok(Some(text.trim_end().to_owned()))
        }));
        let uri = "file:///p/src/Main.kt";
        editor.open(uri, "fun main() {}\n\n");
        editor.format("1", uri);
        // The server answers other requests while ktlint runs; the document changes meanwhile.
        editor.send(jsonrpc::request("2", "textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } })));
        let symbols = editor.reply("2");
        assert!(symbols["result"].is_array(), "{symbols}");
        let change = json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": "fun other() {}\n\n" }] });
        editor.send(jsonrpc::notification("textDocument/didChange", change));
        editor.send(jsonrpc::request("3", "textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } })));
        let _ = editor.reply("3");
        release.send(()).unwrap();
        let reply = editor.reply("1");
        assert_eq!(reply["error"]["code"], CONTENT_MODIFIED, "{reply}");
        editor.finish();
    }

    #[test]
    fn checks_open_documents_again_when_the_libraries_load() {
        let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let (sender, events) = std::sync::mpsc::channel();
        let formatter: Formatter = Arc::new(|_, _, _| Ok(None));
        let mut state = State::new(config, Tx { index: 0, tx: sender }, 0, false, formatter);
        let uri = "file:///p/src/app/Main.kt";
        let main = "package app\n\nimport three.MaterialOptions\n\n/** Options (typed via [MaterialOptions]). */\nfun options(build: MaterialOptions.() -> Unit) {}\n";
        let document = json!({ "uri": uri, "languageId": "kotlin", "version": 1, "text": main });
        state.message(jsonrpc::notification("textDocument/didOpen", json!({ "textDocument": document }))).unwrap();
        // The messages the state sent for the document, the last one first.
        let last = || {
            let published: Vec<Value> = events
                .try_iter()
                .filter_map(|event| match event {
                    Event::Out(_, message) if message["method"] == "textDocument/publishDiagnostics" => Some(message),
                    _ => None,
                })
                .collect();
            published.last().expect("diagnostics").clone()
        };
        let before = last();
        assert_eq!(before["params"]["diagnostics"][0]["message"], "unresolved reference `MaterialOptions`", "{before}");

        let npm = "package three\n\nexternal interface MaterialOptions\n";
        let unit = Arc::new(Unit::new(
            PathBuf::from("/p/target/debug/generated/npm/three/Default.kt"),
            npm.to_owned(),
            Origin::Library { name: "npm".into(), main: Platforms::WASM, test: Platforms::NONE },
        ));
        state.loaded(Event::Libraries { generation: 0, units: vec![unit], jdk: None });
        let after = last();
        assert_eq!(after["params"]["uri"], uri);
        assert_eq!(after["params"]["diagnostics"], json!([]), "{after}");
    }

    #[test]
    fn reports_diagnostics_as_the_document_changes() {
        let mut editor = Editor::start(Arc::new(|_, _, _| Ok(None)));
        let uri = "file:///p/src/Main.kt";
        editor.open(
            uri,
            "fun main() {
    println(1
}
",
        );
        let published = editor.notification("textDocument/publishDiagnostics");
        assert_eq!(published["params"]["uri"], uri);
        let diagnostics = published["params"]["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{published}");
        assert_eq!(diagnostics[0]["message"], "unclosed `(`");
        assert_eq!(diagnostics[0]["range"]["start"], json!({ "line": 1, "character": 11 }));
        let change = json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": "fun main() {
    println(1)
}
" }] });
        editor.send(jsonrpc::notification("textDocument/didChange", change));
        assert_eq!(editor.notification("textDocument/publishDiagnostics")["params"]["diagnostics"], json!([]));
        editor.finish();
    }
}
