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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde_json::{Value, json};

use kargo_maven::Repository;
use kargo_util::config::Config;

use super::features::{self, CompletionOptions, Target};
use super::index::{Index, Origin, Platforms, SetKind, SourceSet, Unit, UnitId};
use super::jdk::Jdk;
use super::library::{self, SourcesJar, UnitCache};
use super::resolve::{Analyzer, Packages};
use crate::jsonrpc::{self, Kind};
use crate::text;

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

enum Event {
    Client(Option<Value>),
    Project { generation: u64, model: Model, units: Vec<Arc<Unit>> },
    Libraries { generation: u64, units: Vec<Arc<Unit>>, jdk: Option<Arc<Jdk>> },
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
    pub repositories: Vec<Repository>,
}

impl Model {
    /// The origin of a file at `path`.
    pub fn origin(&self, path: &Path) -> Origin {
        if let Some((_, set)) = self.files.iter().find(|(p, _)| p == path) {
            return Origin::Source(set.clone());
        }
        self.roots.iter().find(|(root, _)| path.starts_with(root)).map_or(Origin::Loose, |(_, set)| Origin::Source(set.clone()))
    }
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
        let mut platforms = Platforms::NONE;
        for target in package["targets"].as_array().into_iter().flatten() {
            let kind = target["kind"].as_str().unwrap_or_default().to_owned();
            platforms = platforms.union(Platforms::of_target(&kind));
            if kind == "wasm" {
                // `browser` is the `js` platform too.
                model.wasm_platform = Some(if target["environment"] == "wasi" { "wasi" } else { "js" }.to_owned());
            }
            let mut add = |key: &str, set: SetKind| {
                for (i, root) in target[key].as_array().into_iter().flatten().filter_map(Value::as_str).enumerate() {
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
/// for, else the one on `JAVA_HOME` or the path. A missing JDK isn't downloaded here; `kargo build` does that.
fn project_jdk(config: &Config, root: &Path) -> Option<Arc<Jdk>> {
    let installed = kargo_manifest::Manifest::load(&root.join(kargo_manifest::MANIFEST_NAME))
        .ok()
        .and_then(|manifest| kargo_toolchain::jdks::find_installed(config, &manifest.toolchain.jdk).ok().flatten());
    let (home, version) = match installed {
        Some(installed) => (installed.jdk().home, installed.major),
        None => {
            let jdk = kargo_toolchain::Jdk::detect().ok()?;
            (jdk.home, jdk.version)
        }
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
}

/// Loads the project at `root` and its libraries, sending each as it is ready.
fn load(config: Config, root: PathBuf, generation: u64, tx: Sender<Event>, cache: UnitCache, with_dependencies: bool) {
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
    if tx.send(Event::Project { generation, model: model.clone(), units }).is_err() {
        return;
    }
    let platforms = model.packages.platforms.iter().fold(Platforms::NONE, |all, p| all.union(*p));
    let mut jars = library::stdlib_jars(&config, &model.kotlin, model.repositories.clone(), platforms, model.wasm_platform.as_deref());
    // Without the standard library's sources, the built-in outline stands in for it.
    let outline = !jars.iter().any(|j| j.name.starts_with("kotlin-stdlib"));
    let mut units = library::units(&config, &cache, &jars);
    if outline {
        units.extend(library::stub_units(&config));
    }
    let jdk = project_jdk(&config, &root);
    if tx.send(Event::Libraries { generation, units: units.clone(), jdk: jdk.clone() }).is_err() || !with_dependencies {
        return;
    }
    let Ok(meta) = crate::project::metadata(&config, &root, false) else { return };
    let deps = dependency_jars(&meta);
    if deps.is_empty() {
        return;
    }
    // Parsed together with the standard library, so a file both carry is one unit.
    jars.extend(deps);
    let mut units = library::units(&config, &cache, &jars);
    if outline {
        units.extend(library::stub_units(&config));
    }
    let _ = tx.send(Event::Libraries { generation, units, jdk });
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
    serve_with(config, input, output, true)
}

pub(crate) fn serve_with(
    config: Config,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    dependencies: bool,
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
    let mut state = State::new(config, Box::new(output), tx, dependencies);
    loop {
        let event = match state.queued.pop_front() {
            Some(message) => Event::Client(message),
            None => match rx.recv() {
                Ok(event) => event,
                Err(_) => break,
            },
        };
        if !state.handle(event, &rx)? {
            break;
        }
    }
    Ok(())
}

/// How long the first requests wait for the project's files and for the standard library.
const PROJECT_WAIT: Duration = Duration::from_secs(30);
const LIBRARY_WAIT: Duration = Duration::from_secs(5);

struct State {
    config: Config,
    out: Box<dyn Write + Send>,
    tx: Sender<Event>,
    root: Option<PathBuf>,
    model: Model,
    /// Every project file, by path; open documents hold the editor's text.
    files: HashMap<PathBuf, Arc<Unit>>,
    /// Open documents: URI to path.
    open: HashMap<String, PathBuf>,
    lib: Arc<Index>,
    proj: Option<(Arc<Index>, HashMap<PathBuf, u32>)>,
    /// Which load of the project is current.
    load_generation: u64,
    /// Bumped on every change, so stale completion items are not resolved.
    generation: u64,
    snippets: bool,
    watch: bool,
    dependencies: bool,
    cache: UnitCache,
    next_id: u64,
    /// Whether the current load delivered the project's files, and the libraries.
    project_loaded: bool,
    libraries_loaded: bool,
    /// Editor messages that arrived while waiting for a load.
    queued: VecDeque<Option<Value>>,
}

impl State {
    fn new(config: Config, out: Box<dyn Write + Send>, tx: Sender<Event>, dependencies: bool) -> Self {
        Self {
            config,
            out,
            tx,
            root: None,
            model: Model::default(),
            files: HashMap::new(),
            open: HashMap::new(),
            lib: Arc::new(Index::new(true, Vec::new())),
            proj: None,
            load_generation: 0,
            generation: 0,
            snippets: false,
            watch: false,
            dependencies,
            cache: UnitCache::default(),
            next_id: 0,
            project_loaded: false,
            libraries_loaded: false,
            queued: VecDeque::new(),
        }
    }

    fn send(&mut self, message: &Value) -> anyhow::Result<()> {
        jsonrpc::write(&mut self.out, message).map_err(Into::into)
    }

    /// Handles one event; `false` when the server should stop.
    fn handle(&mut self, event: Event, rx: &Receiver<Event>) -> anyhow::Result<bool> {
        match event {
            Event::Client(None) => return Ok(false),
            Event::Client(Some(message)) => {
                if jsonrpc::kind(&message) == Kind::Request && !matches!(jsonrpc::method(&message), "initialize" | "shutdown") {
                    self.wait_for_load(rx);
                }
                return self.message(message);
            }
            other => self.loaded(other),
        }
        Ok(true)
    }

    /// Waits a while for the project and the standard library, the first time they load, so
    /// the first requests see them. Editor messages that arrive meanwhile are queued.
    fn wait_for_load(&mut self, rx: &Receiver<Event>) {
        let start = Instant::now();
        while self.root.is_some() && !(self.project_loaded && self.libraries_loaded) {
            let limit = if self.project_loaded { LIBRARY_WAIT } else { PROJECT_WAIT };
            let Some(remaining) = limit.checked_sub(start.elapsed()) else { break };
            match rx.recv_timeout(remaining) {
                Ok(Event::Client(message)) => self.queued.push_back(message),
                Ok(other) => self.loaded(other),
                Err(_) => break,
            }
        }
    }

    fn loaded(&mut self, event: Event) {
        match event {
            Event::Project { generation, model, units } if generation == self.load_generation => {
                self.project_loaded = true;
                self.model = model;
                let open: HashMap<PathBuf, Arc<Unit>> =
                    self.open.values().filter_map(|p| self.files.get(p).map(|u| (p.clone(), Arc::clone(u)))).collect();
                self.files = units.into_iter().map(|u| (u.path.clone(), u)).collect();
                for (path, unit) in open {
                    let origin = self.model.origin(&path);
                    self.files.insert(path.clone(), Arc::new(Unit::new(path, unit.text.clone(), origin)));
                }
                self.changed();
            }
            Event::Libraries { generation, units, jdk } if generation == self.load_generation => {
                self.libraries_loaded = true;
                self.lib = Arc::new(Index::new(true, units).with_jdk(jdk));
                self.changed();
            }
            _ => {}
        }
    }

    fn changed(&mut self) {
        self.proj = None;
        self.generation += 1;
    }

    fn reload(&mut self) {
        let Some(root) = self.root.clone() else { return };
        self.load_generation += 1;
        let (config, tx, cache, generation, deps) =
            (self.config.clone(), self.tx.clone(), Arc::clone(&self.cache), self.load_generation, self.dependencies);
        thread::spawn(move || load(config, root, generation, tx, cache, deps));
    }

    fn message(&mut self, message: Value) -> anyhow::Result<bool> {
        let method = jsonrpc::method(&message).to_owned();
        let params = &message["params"];
        match jsonrpc::kind(&message) {
            Kind::Response => {}
            Kind::Notification => match method.as_str() {
                "exit" => return Ok(false),
                "initialized" => self.register_watchers()?,
                "textDocument/didOpen" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    if let Some(path) = file_path(uri).filter(|_| uri.ends_with(".kt")) {
                        let text = params["textDocument"]["text"].as_str().unwrap_or_default().to_owned();
                        self.open.insert(uri.to_owned(), path.clone());
                        self.set_file(path, text);
                    }
                }
                "textDocument/didChange" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    if let Some(path) = self.open.get(uri).cloned() {
                        let mut text = self.files.get(&path).map(|u| u.text.clone()).unwrap_or_default();
                        text::apply_changes(&mut text, params["contentChanges"].as_array().map(Vec::as_slice).unwrap_or_default());
                        self.set_file(path, text);
                    }
                }
                "textDocument/didClose" => {
                    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                    if let Some(path) = self.open.remove(uri) {
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
                    for change in params["changes"].as_array().into_iter().flatten() {
                        let uri = change["uri"].as_str().unwrap_or_default();
                        let Some(path) = file_path(uri) else { continue };
                        let name = text::file_name(uri);
                        if name == kargo_manifest::MANIFEST_NAME || name == "kargo.lock" {
                            reload = true;
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
                    }
                }
                _ => {}
            },
            Kind::Request => {
                let id = message["id"].clone();
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

    fn set_file(&mut self, path: PathBuf, text: String) {
        let origin = self.model.origin(&path);
        self.files.insert(path.clone(), Arc::new(Unit::new(path, text, origin)));
        self.changed();
    }

    fn register_watchers(&mut self) -> anyhow::Result<()> {
        if !self.watch {
            return Ok(());
        }
        self.next_id += 1;
        let id = format!("kotlin/{}", self.next_id);
        let watchers = json!([
            { "globPattern": "**/*.kt" },
            { "globPattern": "**/*.java" },
            { "globPattern": "**/kargo.toml" },
            { "globPattern": "**/kargo.lock" }
        ]);
        let params = json!({ "registrations": [{ "id": "kargo-kotlin-watch", "method": "workspace/didChangeWatchedFiles", "registerOptions": { "watchers": watchers } }] });
        self.send(&jsonrpc::request(&id, "client/registerCapability", params))
    }

    /// The project index, rebuilt after changes.
    fn snapshot(&mut self) -> (Arc<Index>, HashMap<PathBuf, u32>) {
        if let Some((index, ids)) = &self.proj {
            return (Arc::clone(index), ids.clone());
        }
        let mut units: Vec<Arc<Unit>> = self.files.values().cloned().collect();
        units.sort_by(|a, b| a.path.cmp(&b.path));
        let ids: HashMap<PathBuf, u32> = units.iter().enumerate().map(|(i, u)| (u.path.clone(), i as u32)).collect();
        let index = Arc::new(Index::new(false, units));
        self.proj = Some((Arc::clone(&index), ids.clone()));
        (index, ids)
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, Failure> {
        match method {
            "initialize" => return Ok(self.initialize(params)),
            "shutdown" => return Ok(Value::Null),
            _ => {}
        }
        let (proj, ids) = self.snapshot();
        let lib = Arc::clone(&self.lib);
        let packages = self.model.packages.clone();
        let an = Analyzer::new(&lib, &proj, &packages);
        let generation = self.generation;
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        // A Java file is read for its declarations; this server answers requests for Kotlin files only.
        let unit = file_path(uri)
            .and_then(|p| ids.get(&p).copied())
            .filter(|&u| !proj.units[u as usize].is_java())
            .map(|u| UnitId { lib: false, unit: u });
        let at = |unit: UnitId| -> (usize, Option<usize>) {
            let u = an.unit(unit);
            let offset = text::offset(&u.text, &params["position"]);
            (offset, u.file.lexed.ident_at(offset))
        };
        match method {
            "textDocument/completion" => {
                let Some(unit) = unit else { return Ok(json!({ "isIncomplete": false, "items": [] })) };
                let trigger = params["context"]["triggerCharacter"].as_str();
                if trigger.is_some_and(|t| t != "." && t != ":") {
                    return Ok(json!({ "isIncomplete": true, "items": [] }));
                }
                let (offset, _) = at(unit);
                Ok(features::completion(&an, unit, offset, CompletionOptions { snippets: self.snippets }, generation))
            }
            "completionItem/resolve" => {
                if params["data"]["generation"].as_u64() != Some(generation) {
                    return Ok(params.clone());
                }
                Ok(features::resolve_item(&an, params.clone()))
            }
            "textDocument/hover" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                Ok(tok.and_then(|t| features::hover(&an, unit, t)).unwrap_or(Value::Null))
            }
            "textDocument/definition" | "textDocument/declaration" | "textDocument/typeDefinition" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(tok) = tok else { return Ok(Value::Null) };
                let mut targets = features::targets_at(&an, unit, tok);
                if method == "textDocument/typeDefinition" {
                    targets = type_targets(&an, &targets);
                }
                let locations: Vec<Value> = targets.iter().take(1).filter_map(|t| features::location(&an, t)).collect();
                Ok(json!(locations))
            }
            "textDocument/references" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(group) = tok.and_then(|t| group_at(&an, unit, t)) else { return Ok(json!([])) };
                let include = params["context"]["includeDeclaration"] != false;
                let refs = features::references(&an, &group, include);
                Ok(json!(refs.iter().map(|(u, r)| location(&an, *u, r.clone())).collect::<Vec<_>>()))
            }
            "textDocument/documentHighlight" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(group) = tok.and_then(|t| group_at(&an, unit, t)) else { return Ok(json!([])) };
                let refs = features::references(&an, &group, true);
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
                let (_, tok) = at(unit);
                let Some(tok) = tok else { return Err(Failure::Invalid("there is nothing to rename here".into())) };
                rename_target(&an, unit, tok)?;
                let u = an.unit(unit);
                let span = u.file.lexed.tokens[tok].span();
                Ok(json!({ "range": text::range(&u.text, span), "placeholder": u.file.lexed.text(&u.text, tok) }))
            }
            "textDocument/rename" => {
                let Some(unit) = unit else { return Ok(Value::Null) };
                let (_, tok) = at(unit);
                let Some(tok) = tok else { return Err(Failure::Invalid("there is nothing to rename here".into())) };
                let new_name = params["newName"].as_str().unwrap_or_default();
                let Some(new_text) = features::new_name_text(new_name) else {
                    return Err(Failure::Invalid(format!("`{new_name}` is not a valid Kotlin name")));
                };
                let group = rename_target(&an, unit, tok)?;
                let mut changes: HashMap<String, Vec<Value>> = HashMap::new();
                for (u, range) in features::references(&an, &group, true) {
                    let unit = an.unit(u);
                    changes
                        .entry(text::path_to_uri(&unit.path))
                        .or_default()
                        .push(json!({ "range": text::range(&unit.text, range), "newText": new_text }));
                }
                Ok(json!({ "changes": changes }))
            }
            "textDocument/documentSymbol" => {
                let Some(unit) = unit else { return Ok(json!([])) };
                Ok(features::document_symbols(&an, unit))
            }
            "workspace/symbol" => Ok(features::workspace_symbols(&an, params["query"].as_str().unwrap_or_default())),
            _ => Err(Failure::NotFound),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let completion = &params["capabilities"]["textDocument"]["completion"];
        self.snippets = completion["completionItem"]["snippetSupport"] == true;
        self.watch = params["capabilities"]["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"] == true;
        self.root = params["rootUri"]
            .as_str()
            .and_then(file_path)
            .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
            .or_else(|| params["workspaceFolders"][0]["uri"].as_str().and_then(file_path));
        self.reload();
        json!({
            "capabilities": {
                "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
                "completionProvider": { "triggerCharacters": ["."], "resolveProvider": true },
                "hoverProvider": true,
                "definitionProvider": true,
                "declarationProvider": true,
                "typeDefinitionProvider": true,
                "referencesProvider": true,
                "documentHighlightProvider": true,
                "renameProvider": { "prepareProvider": true },
                "documentSymbolProvider": true,
                "workspaceSymbolProvider": true,
            },
            "serverInfo": { "name": "kargo-kotlin", "version": env!("CARGO_PKG_VERSION") },
        })
    }
}

enum Failure {
    NotFound,
    Invalid(String),
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

/// What renaming the identifier at `tok` changes: a declaration of the project and its counterparts.
fn rename_target(an: &Analyzer, unit: UnitId, tok: usize) -> Result<Vec<super::index::SymId>, Failure> {
    let targets = features::targets_at(an, unit, tok);
    match targets.first() {
        Some(Target::Sym(m)) if !m.sym.lib && !an.unit(m.sym.unit_id()).is_java() => Ok(features::rename_group(an, m.sym)),
        Some(Target::Sym(m)) if !m.sym.lib => {
            let name = &an.decl(m.sym).name;
            Err(Failure::Invalid(format!("`{name}` is declared in a Java file, which this server doesn't edit, so it can't be renamed")))
        }
        Some(Target::Sym(m)) => {
            let name = &an.decl(m.sym).name;
            let origin = match &an.unit(m.sym.unit_id()).origin {
                Origin::Library { name, .. } => name.clone(),
                _ => "a library".into(),
            };
            Err(Failure::Invalid(format!("`{name}` is declared in {origin}, not in this project, so it can't be renamed")))
        }
        Some(Target::It { .. }) => Err(Failure::Invalid("`it` is an implicit parameter; name the lambda's parameter to rename it".into())),
        Some(Target::Package(_)) => Err(Failure::Invalid("renaming packages isn't supported".into())),
        None => Err(Failure::Invalid("there is no declaration to rename here".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layout_from_metadata() {
        let meta = json!({
            "toolchain": { "kotlin": "2.4.21" },
            "packages": [
                {
                    "name": "app",
                    "path-dependencies": [{ "package": "lib" }],
                    "targets": [
                        { "kind": "jvm", "source-roots": ["/p/src", "/p/src-jvm"], "test-roots": ["/p/tests", "/p/tests-jvm", "/p/test-fixtures", "/p/test-fixtures-jvm"] },
                        { "kind": "wasm", "source-roots": ["/p/src", "/p/src-wasm"], "test-roots": ["/p/tests", "/p/tests-wasm"] },
                    ],
                    "examples": [{ "name": "demo", "path": "/p/examples/demo.kt" }],
                },
                { "name": "lib", "path-dependencies": [{ "package": "core" }], "targets": [{ "kind": "native", "source-roots": ["/p/lib/src", "/p/lib/src-native"], "test-roots": [] }] },
                { "name": "core", "targets": [] },
            ],
        });
        let model = model_from_metadata(&meta);
        assert_eq!(model.kotlin, "2.4.21");
        assert_eq!(model.wasm_platform.as_deref(), Some("js"));
        assert_eq!(model.packages.platforms, [Platforms::JVM.union(Platforms::WASM), Platforms::NATIVE, Platforms::NONE]);
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
        assert_eq!(set("/p/lib/src-native/N.kt"), (1, SetKind::Main, Some("native".into())));
        assert_eq!(model.origin(Path::new("/elsewhere/X.kt")), Origin::Loose);
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
}
