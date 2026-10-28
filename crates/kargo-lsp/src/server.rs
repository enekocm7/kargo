//! The `kargo lsp` message loop.
//!
//! Kargo sits between the editor and a Kotlin language server (the backend). Everything about
//! `kargo.toml` (diagnostics, completion, hover, code actions), Kargo's commands for
//! `workspace/executeCommand` and the `kargo/runnables` request are answered here; every other
//! message is passed through in both directions, except that completion items get their
//! signature as `detail` when the editor doesn't show `labelDetails` (see
//! [`label_details_as_detail`]). The `initialize` response is the backend's with Kargo's
//! capabilities merged in.
//!
//! Before the backend starts, Kargo imports the project and writes the description the
//! backend reads (see [`crate::project`]). When `kargo.toml` or `kargo.lock` change and the
//! project's fingerprint with them, it imports the project again in the background, reports
//! it with `$/progress`, and tells the backend which generated files changed.
//!
//! Work that may use the network (completing dependencies, hints at newer versions; see
//! [`crate::deps`]) runs on worker threads that send their results back as events, so neither
//! Kotlin traffic nor `kargo.toml` diagnostics wait for it.
//!
//! With `checkOnSave` (see [`crate::check`]), saving a Kotlin or Java file runs `kargo check` on
//! a worker thread, cancelling the one still running. Its diagnostics are merged into the
//! backend's: into the `textDocument/publishDiagnostics` it pushes and the
//! `textDocument/diagnostic` reports the editor pulls, leaving out what the backend already reports.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use kargo_toolchain::language_server::{KLS_MAIN_CLASS, KLS_MAX_JDK};
use kargo_toolchain::{LanguageServer, ServerKind};
use kargo_util::config::Config;

use crate::jsonrpc::{self, Kind};
use crate::{actions, check, deps, manifest, project, script, text};

/// The Kotlin language server behind `kargo lsp`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KotlinServer {
    /// None: only `kargo.toml` support and runnables.
    #[default]
    None,
    /// Kargo's own server, written in Rust and run in-process (see [`crate::kotlin`]).
    Kargo,
    /// A Kotlin language server Kargo downloads and runs.
    External(ServerKind),
}

impl KotlinServer {
    /// The server Kargo downloads and runs, if it is one.
    pub fn external(self) -> Option<ServerKind> {
        match self {
            KotlinServer::External(kind) => Some(kind),
            _ => None,
        }
    }
}

/// How `kargo lsp` runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The Kotlin language server behind Kargo's.
    pub server: KotlinServer,
    /// Runs this executable as the server instead of the one Kargo manages.
    pub server_path: Option<PathBuf>,
    /// Which targets `workspace.json` describes to the Kotlin LSP.
    pub targets: project::Targets,
    /// Kargo's built-in commands, for the warning about tasks they hide.
    pub builtin_commands: Vec<String>,
    /// The Maven Central search API; default: `KARGO_MAVEN_SEARCH_URL` or [`deps::DEFAULT_SEARCH_URL`].
    pub search_url: Option<String>,
    /// Appends a line for each message from the editor to this file, with how long each request
    /// took to answer (`--log-file`).
    pub log_file: Option<PathBuf>,
}

/// A backend talking over in-memory streams, for tests.
pub(crate) type BackendIo = (Box<dyn BufRead + Send>, Box<dyn Write + Send>);

enum Event {
    /// A message from the editor; `None` when it closed the stream.
    Client(Option<Value>),
    /// A message from the backend; `None` when it exited.
    Backend(Option<Value>),
    /// A background import finished.
    Imported(project::Outcome),
    /// A response computed on a worker thread, ready for the editor.
    Reply(Value),
    /// The newer-version hints for an open `kargo.toml`.
    Hints { uri: String, hints: Vec<deps::Hint> },
    /// A command's new contents for the `kargo.toml` at `uri`, worked out on a worker thread from
    /// `original`, with the reply to request `id`.
    Edited { id: Value, uri: String, original: String, outcome: Result<(String, Value)> },
    /// The check of the `kargo.toml` at `manifest` started as number `generation` finished.
    Checked { generation: u64, manifest: PathBuf, outcome: Result<check::Output> },
}

/// Runs the server until the editor sends `exit` or closes the stream. Returns whether the
/// editor shut it down properly (`shutdown` before `exit`).
pub fn serve(config: Config, opts: Options, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> Result<bool> {
    serve_with(config, opts, input, output, None, check::kargo_runner())
}

pub(crate) fn serve_with(
    config: Config,
    opts: Options,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    backend: Option<BackendIo>,
    runner: check::Runner,
) -> Result<bool> {
    let (tx, rx) = mpsc::channel();
    let client_tx = tx.clone();
    thread::spawn(move || {
        let mut input = input;
        loop {
            let message = match jsonrpc::read(&mut input) {
                Ok(message) => message,
                Err(e) => {
                    eprintln!("kargo lsp: {e}");
                    None
                }
            };
            let end = message.is_none();
            if client_tx.send(Event::Client(message)).is_err() || end {
                break;
            }
        }
    });
    let mut server = Server::new(config, opts, Box::new(output), tx, runner);
    if let Some((reader, writer)) = backend {
        server.attach(reader, writer, None);
    }
    for event in rx {
        if let Some(clean) = server.handle(event)? {
            server.cancel_check();
            server.stop_backend();
            return Ok(clean);
        }
    }
    server.cancel_check();
    server.stop_backend();
    Ok(false)
}

struct Backend {
    stdin: Box<dyn Write + Send>,
    child: Option<Child>,
}

/// The `$/progress` of an import or a check, once the editor accepted its token.
struct Progress {
    token: String,
    create_id: String,
    begun: bool,
}

struct Server {
    config: Config,
    opts: Options,
    out: Box<dyn Write + Send>,
    tx: Sender<Event>,
    backend: Option<Backend>,
    /// The project's directory, once known.
    root: Option<PathBuf>,
    /// Open `kargo.toml` documents by URI.
    docs: HashMap<String, String>,
    /// Requests sent to the backend that it hasn't answered yet.
    pending: HashSet<String>,
    /// The id of the `initialize` request, while the backend answers it.
    initialize_id: Option<Value>,
    client_progress: bool,
    client_watches: bool,
    /// Whether the editor applies `workspace/applyEdit`.
    client_apply_edit: bool,
    shutdown: bool,
    next_id: u64,
    fingerprint: Option<String>,
    importing: bool,
    reimport: bool,
    progress: Option<Progress>,
    warned_foreign: bool,
    searcher: Arc<deps::Searcher>,
    /// Newer-version hints by `kargo.toml` URI.
    hints: HashMap<String, Vec<deps::Hint>>,
    /// Check on save: what the editor asked for and how a check runs.
    check: check::Settings,
    runner: check::Runner,
    /// The number of the latest check; results of older ones are dropped.
    check_generation: u64,
    /// Stops the running check.
    check_cancel: Option<Arc<AtomicBool>>,
    check_progress: Option<Progress>,
    /// The diagnostics of the last check of each `kargo.toml`, by [`check::uri_key`]: the URI
    /// to publish under and the diagnostics.
    checked: HashMap<PathBuf, HashMap<String, (String, Vec<Value>)>>,
    /// The backend's last pushed diagnostics, by [`check::uri_key`].
    backend_pushed: HashMap<String, Vec<Value>>,
    /// The items of the backend's last full `textDocument/diagnostic` report, by [`check::uri_key`].
    backend_pulled: HashMap<String, Vec<Value>>,
    /// `textDocument/diagnostic` requests the backend hasn't answered, with their URI.
    pulls: HashMap<String, String>,
    /// Whether the editor shows a completion item's `labelDetails` (its
    /// `completionItem.labelDetailsSupport`).
    client_label_details: bool,
    /// `textDocument/completion` requests the backend hasn't answered, whose items get their
    /// `labelDetails` as `detail` (see [`label_details_as_detail`]). Only recorded for an editor
    /// that doesn't show `labelDetails`.
    completions: HashSet<String>,
    /// Whether the backend's diagnostics are pulled (`diagnosticProvider`) rather than pushed.
    backend_pull: bool,
    /// Whether the editor re-pulls on `workspace/diagnostic/refresh`.
    client_refresh: bool,
    /// Kotlin and Java documents open in the editor, by [`check::uri_key`].
    open_sources: HashSet<String>,
    /// Documents Kargo pushed check diagnostics for while their diagnostics are pulled (they
    /// were closed), by [`check::uri_key`].
    pushed: HashSet<String>,
    /// `initializationOptions.status`: send `kargo/status` notifications.
    status: bool,
    /// The `--log-file`, and when the server started, which its lines count from.
    log: Option<std::fs::File>,
    started: Instant,
    /// The editor's requests not answered yet, by [`id_key`]: their method and when they came,
    /// for the log.
    timings: HashMap<String, (String, Instant)>,
}

/// The editor's `textDocument.uri` of a message, if any.
fn document_uri(message: &Value) -> Option<&str> {
    message["params"]["textDocument"]["uri"].as_str()
}

fn is_manifest(uri: &str) -> bool {
    text::file_name(uri) == kargo_manifest::MANIFEST_NAME
}

fn is_lockfile(uri: &str) -> bool {
    text::file_name(uri) == "kargo.lock"
}

/// Documents Kargo answers for instead of the Kotlin server: `kargo.lock` and every TOML file
/// (editors send all of them to a server registered for TOML; only `kargo.toml` gets features).
fn is_kargo_document(uri: &str) -> bool {
    is_lockfile(uri) || uri.ends_with(".toml")
}

/// The key of a request id in [`Server::pending`].
fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Asks the Kotlin LSP to import each workspace folder from `workspace.json`. Left alone, it
/// picks the importer by what it finds in the folder, and an IDE's `.idea/` (IntelliJ, which
/// describes none of Kargo's libraries) wins over `workspace.json`. An editor's own
/// `buildTools` entry for a folder is kept.
fn prefer_workspace_json(params: &mut serde_json::Map<String, Value>) {
    let mut folders: Vec<String> = params
        .get("workspaceFolders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f["uri"].as_str())
        .map(str::to_owned)
        .collect();
    folders.extend(params.get("rootUri").and_then(Value::as_str).map(str::to_owned));
    let options = params.entry("initializationOptions").or_insert_with(|| json!({}));
    if options.is_null() {
        *options = json!({});
    }
    let Some(options) = options.as_object_mut() else { return };
    let Some(tools) = options.entry("buildTools").or_insert_with(|| json!({})).as_object_mut() else { return };
    for folder in folders {
        tools.entry(folder).or_insert_with(|| json!("json"));
    }
}

/// The capabilities Kargo's own server has.
fn own_capabilities() -> Value {
    json!({
        "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
        "completionProvider": { "triggerCharacters": ["[", ".", "\"", "=", " ", ","] },
        "hoverProvider": true,
        "codeActionProvider": { "codeActionKinds": CODE_ACTION_KINDS },
        "executeCommandProvider": { "commands": actions::COMMANDS },
        "experimental": { "kargo": { "runnables": true } },
    })
}

/// The kinds of code actions Kargo offers in `kargo.toml`.
const CODE_ACTION_KINDS: [&str; 3] = ["quickfix", "refactor", "source"];

/// Adds the strings of `ours` that `list` lacks.
fn union(list: &Value, ours: &Value) -> Value {
    let mut out: Vec<Value> = list.as_array().cloned().unwrap_or_default();
    for item in ours.as_array().into_iter().flatten() {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    json!(out)
}

/// The backend's capabilities with Kargo's merged in: Kargo needs open, change and save
/// notifications, completion and hover.
pub(crate) fn merge_capabilities(backend: &Value) -> Value {
    let ours = own_capabilities();
    let Value::Object(_) = backend else { return ours };
    let mut caps = backend.clone();
    caps["textDocumentSync"] = match &backend["textDocumentSync"] {
        Value::Number(n) => {
            let change = if n.as_u64() == Some(0) { json!(2) } else { json!(n) };
            json!({ "openClose": true, "change": change, "save": { "includeText": false } })
        }
        Value::Object(sync) => {
            let mut sync = Value::Object(sync.clone());
            sync["openClose"] = json!(true);
            if !sync["change"].as_u64().is_some_and(|c| c > 0) {
                sync["change"] = json!(2);
            }
            if sync["save"].is_null() || sync["save"] == false {
                sync["save"] = json!({ "includeText": false });
            }
            sync
        }
        _ => ours["textDocumentSync"].clone(),
    };
    match caps["completionProvider"].as_object_mut() {
        Some(completion) => {
            let triggers =
                union(completion.get("triggerCharacters").unwrap_or(&Value::Null), &ours["completionProvider"]["triggerCharacters"]);
            completion.insert("triggerCharacters".into(), triggers);
        }
        None => caps["completionProvider"] = ours["completionProvider"].clone(),
    }
    if caps["hoverProvider"].is_null() || caps["hoverProvider"] == false {
        caps["hoverProvider"] = json!(true);
    }
    // `true` already offers every kind; a list of kinds gains Kargo's.
    match &caps["codeActionProvider"] {
        Value::Object(provider) if provider.contains_key("codeActionKinds") => {
            caps["codeActionProvider"]["codeActionKinds"] =
                union(&backend["codeActionProvider"]["codeActionKinds"], &ours["codeActionProvider"]["codeActionKinds"]);
        }
        Value::Object(_) | Value::Bool(true) => {}
        _ => caps["codeActionProvider"] = ours["codeActionProvider"].clone(),
    }
    let commands = union(&backend["executeCommandProvider"]["commands"], &ours["executeCommandProvider"]["commands"]);
    if !caps["executeCommandProvider"].is_object() {
        caps["executeCommandProvider"] = json!({});
    }
    caps["executeCommandProvider"]["commands"] = commands;
    if !caps["experimental"].is_object() {
        caps["experimental"] = json!({});
    }
    // The backend's own (Kargo's server answers `kargo/testTree`) with Kargo's.
    let mut kargo = backend["experimental"]["kargo"].as_object().cloned().unwrap_or_default();
    kargo.extend(ours["experimental"]["kargo"].as_object().cloned().unwrap_or_default());
    caps["experimental"]["kargo"] = Value::Object(kargo);
    caps
}

/// Gives the items of a completion result without a `detail` the `detail` (or else the
/// `description`) of their `labelDetails`, for an editor that doesn't show `labelDetails`. The
/// Kotlin LSP puts each overload's signature only there, so without this every overload of
/// `print` shows as the same bare `print`.
fn label_details_as_detail(result: &mut Value) {
    let items = match result {
        Value::Array(items) => items,
        Value::Object(list) => match list.get_mut("items") {
            Some(Value::Array(items)) => items,
            _ => return,
        },
        _ => return,
    };
    for item in items {
        let Some(item) = item.as_object_mut() else { continue };
        if item.get("detail").is_none_or(Value::is_null) {
            let signature = item.get("labelDetails").and_then(|d| d["detail"].as_str().or(d["description"].as_str()));
            if let Some(signature) = signature.map(str::to_owned) {
                item.insert("detail".into(), json!(signature));
            }
        }
    }
}

impl Server {
    fn new(config: Config, opts: Options, out: Box<dyn Write + Send>, tx: Sender<Event>, runner: check::Runner) -> Self {
        let searcher = Arc::new(deps::Searcher::new(&config, opts.search_url.clone()));
        let log = opts.log_file.as_ref().and_then(|path| match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => Some(file),
            Err(e) => {
                eprintln!("kargo lsp: could not open the log file {}: {e}", path.display());
                None
            }
        });
        Self {
            config,
            opts,
            out,
            tx,
            backend: None,
            root: None,
            docs: HashMap::new(),
            pending: HashSet::new(),
            initialize_id: None,
            client_progress: false,
            client_watches: false,
            client_apply_edit: false,
            shutdown: false,
            next_id: 0,
            fingerprint: None,
            importing: false,
            reimport: false,
            progress: None,
            warned_foreign: false,
            searcher,
            hints: HashMap::new(),
            check: check::Settings::default(),
            runner,
            check_generation: 0,
            check_cancel: None,
            check_progress: None,
            checked: HashMap::new(),
            backend_pushed: HashMap::new(),
            backend_pulled: HashMap::new(),
            pulls: HashMap::new(),
            client_label_details: false,
            completions: HashSet::new(),
            backend_pull: false,
            client_refresh: false,
            open_sources: HashSet::new(),
            pushed: HashSet::new(),
            status: false,
            log,
            started: Instant::now(),
            timings: HashMap::new(),
        }
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        if self.log.is_some()
            && jsonrpc::kind(message) == Kind::Response
            && let Some((method, since)) = self.timings.remove(&id_key(&message["id"]))
        {
            let failed = if message["error"].is_object() { " (error)" } else { "" };
            let ms = since.elapsed().as_secs_f64() * 1000.0;
            self.log_line(&format!("{method} {} answered in {ms:.1} ms{failed}", message["id"]));
        }
        jsonrpc::write(&mut self.out, message).context("failed to write to the editor")
    }

    /// Writes a line to the `--log-file`, after the seconds since the server started.
    fn log_line(&mut self, line: &str) {
        let elapsed = self.started.elapsed().as_secs_f64();
        if let Some(log) = &mut self.log {
            let _ = writeln!(log, "[{elapsed:>9.3}s] {line}");
        }
    }

    /// Sends `kargo/status` for `task` (`resolving`, `checking`), when the editor asked for it.
    fn status(&mut self, task: &str, active: bool, message: Option<&str>) -> Result<()> {
        if !self.status {
            return Ok(());
        }
        let mut params = json!({ "task": task, "active": active });
        if let Some(message) = message {
            params["message"] = json!(message);
        }
        self.notify("kargo/status", params)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&jsonrpc::notification(method, params))
    }

    /// `window/showMessage` (`level` 1 = error, 2 = warning, 3 = info) or, with `log`,
    /// `window/logMessage`.
    fn message(&mut self, level: u8, text: impl Into<String>, log: bool) -> Result<()> {
        let method = if log { "window/logMessage" } else { "window/showMessage" };
        self.notify(method, json!({ "type": level, "message": text.into() }))
    }

    fn send_backend(&mut self, message: &Value) {
        let Some(backend) = &mut self.backend else { return };
        if jsonrpc::write(&mut backend.stdin, message).is_err() {
            // Its exit arrives as an event.
            self.backend.as_mut().map(|b| b.child.as_mut().map(Child::kill));
        }
    }

    fn next_id(&mut self) -> String {
        self.next_id += 1;
        format!("kargo/{}", self.next_id)
    }

    /// Returns `Some(clean)` when the server should exit.
    fn handle(&mut self, event: Event) -> Result<Option<bool>> {
        match event {
            Event::Client(None) => Ok(Some(false)),
            Event::Client(Some(message)) => self.on_client(message),
            Event::Backend(Some(message)) => {
                self.on_backend(message)?;
                Ok(None)
            }
            Event::Backend(None) => {
                self.backend_exited()?;
                Ok(None)
            }
            Event::Imported(outcome) => {
                self.imported(outcome)?;
                Ok(None)
            }
            Event::Reply(message) => {
                self.send(&message)?;
                Ok(None)
            }
            Event::Edited { id, uri, original, outcome } => {
                let reply = match outcome.and_then(|(new, result)| self.write_manifest(&uri, &original, &new).map(|()| result)) {
                    Ok(result) => jsonrpc::response(&id, result),
                    Err(e) => jsonrpc::error(&id, -32603, format!("{e:#}")),
                };
                self.send(&reply)?;
                Ok(None)
            }
            Event::Checked { generation, manifest, outcome } => {
                self.checked_event(generation, manifest, outcome)?;
                Ok(None)
            }
            Event::Hints { uri, hints } => {
                // Republish only when they changed, so a project without updates sends nothing more.
                if self.docs.contains_key(&uri) && self.hints.insert(uri.clone(), hints.clone()).unwrap_or_default() != hints {
                    self.publish(&uri)?;
                }
                Ok(None)
            }
        }
    }

    fn on_client(&mut self, message: Value) -> Result<Option<bool>> {
        let method = jsonrpc::method(&message).to_owned();
        if self.log.is_some() {
            match jsonrpc::kind(&message) {
                Kind::Request => {
                    self.timings.insert(id_key(&message["id"]), (method.clone(), Instant::now()));
                }
                Kind::Notification => self.log_line(&method),
                Kind::Response => {}
            }
        }
        match jsonrpc::kind(&message) {
            Kind::Response => {
                let id = message["id"].as_str().unwrap_or_default();
                if id.starts_with("kargo/") {
                    self.own_response(id.to_owned(), &message)?;
                } else {
                    self.send_backend(&message);
                }
                Ok(None)
            }
            Kind::Request => {
                let id = message["id"].clone();
                match method.as_str() {
                    "initialize" => self.initialize(message)?,
                    "shutdown" => {
                        self.shutdown = true;
                        self.forward_request(message, json!(null))?;
                    }
                    "workspace/executeCommand" if message["params"]["command"].as_str().is_some_and(|c| actions::COMMANDS.contains(&c)) => {
                        match self.execute_command(&message) {
                            Ok(Some(result)) => self.send(&jsonrpc::response(&id, result))?,
                            // Answered when the worker thread is done.
                            Ok(None) => {}
                            Err(e) => self.send(&jsonrpc::error(&id, -32603, format!("{e:#}")))?,
                        }
                    }
                    "kargo/runnables" => {
                        let reply = match self.runnables(&message) {
                            Ok(list) => jsonrpc::response(&id, json!(list)),
                            Err(e) => jsonrpc::error(&id, -32603, format!("{e:#}")),
                        };
                        self.send(&reply)?;
                    }
                    _ if matches!(method.as_str(), "textDocument/completion" | "textDocument/hover")
                        && document_uri(&message).is_some_and(|uri| script::is_script_uri(uri) && self.docs.contains_key(uri)) =>
                    {
                        match self.script_request(&method, &message) {
                            script::Answer::Now(result) => self.send(&jsonrpc::response(&id, result))?,
                            // Answered by the worker thread when it is done.
                            script::Answer::Later => {}
                            script::Answer::Backend => self.forward_request(message, Value::Null)?,
                        }
                    }
                    _ if document_uri(&message).is_some_and(is_kargo_document) => {
                        if !self.dependency_completion(&method, &message) {
                            let result = self.manifest_request(&method, &message);
                            self.send(&jsonrpc::response(&id, result))?;
                        }
                    }
                    _ => self.forward_request(message, Value::Null)?,
                }
                Ok(None)
            }
            Kind::Notification => {
                match method.as_str() {
                    "exit" => {
                        self.send_backend(&message);
                        return Ok(Some(self.shutdown));
                    }
                    "initialized" => {
                        self.send_backend(&message);
                        self.watch_manifests()?;
                    }
                    "workspace/didChangeWatchedFiles" => {
                        self.send_backend(&message);
                        let changes = message["params"]["changes"].as_array().cloned().unwrap_or_default();
                        if changes.iter().filter_map(|c| c["uri"].as_str()).any(|u| is_manifest(u) || is_lockfile(u)) {
                            self.maybe_reimport()?;
                        }
                    }
                    _ if document_uri(&message).is_some_and(is_kargo_document) => {
                        self.manifest_notification(&method, &message)?;
                    }
                    "textDocument/didOpen" | "textDocument/didClose" | "textDocument/didSave" => {
                        self.send_backend(&message);
                        self.source_notification(&method, &message)?;
                        self.script_notification(&method, &message)?;
                    }
                    "textDocument/didChange" => {
                        self.send_backend(&message);
                        self.script_notification(&method, &message)?;
                    }
                    _ => self.send_backend(&message),
                }
                Ok(None)
            }
        }
    }

    /// Sends a request to the backend, or answers it with `fallback` (`shutdown`) or "method
    /// not found" when there is none.
    fn forward_request(&mut self, message: Value, fallback: Value) -> Result<()> {
        let id = message["id"].clone();
        if self.backend.is_some() {
            self.pending.insert(id_key(&id));
            if jsonrpc::method(&message) == "textDocument/diagnostic"
                && let Some(uri) = document_uri(&message)
            {
                self.pulls.insert(id_key(&id), uri.to_owned());
            }
            if jsonrpc::method(&message) == "textDocument/completion" && !self.client_label_details {
                self.completions.insert(id_key(&id));
            }
            self.send_backend(&message);
            return Ok(());
        }
        let reply = if jsonrpc::method(&message) == "shutdown" {
            jsonrpc::response(&id, fallback)
        } else {
            jsonrpc::error(&id, jsonrpc::METHOD_NOT_FOUND, format!("`{}` needs a Kotlin language server", jsonrpc::method(&message)))
        };
        self.send(&reply)
    }

    fn on_backend(&mut self, mut message: Value) -> Result<()> {
        match jsonrpc::kind(&message) {
            Kind::Response => {
                self.pending.remove(&id_key(&message["id"]));
                if self.initialize_id.as_ref() == Some(&message["id"]) {
                    self.initialize_id = None;
                    if message["result"].is_object() {
                        let capabilities = &message["result"]["capabilities"];
                        self.backend_pull = !capabilities["diagnosticProvider"].is_null();
                        // The editor's positions are in the encoding the backend picked.
                        text::set_encoding(text::Encoding::of_capability(&capabilities["positionEncoding"]));
                        message["result"]["capabilities"] = merge_capabilities(capabilities);
                    }
                } else if let Some(uri) = self.pulls.remove(&id_key(&message["id"])) {
                    self.merge_pulled(&uri, &mut message["result"]);
                } else if self.completions.remove(&id_key(&message["id"])) {
                    label_details_as_detail(&mut message["result"]);
                }
            }
            Kind::Notification if jsonrpc::method(&message) == "textDocument/publishDiagnostics" => {
                let uri = message["params"]["uri"].as_str().unwrap_or_default().to_owned();
                let theirs = message["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
                let key = check::uri_key(&uri);
                let mut ours = self.check_diagnostics(&key);
                if let Some(text) = self.docs.get(&uri).filter(|_| script::is_script_uri(&uri)) {
                    ours.extend(script::diagnostics(text));
                }
                message["params"]["diagnostics"] = json!(check::merge(&theirs, &ours));
                self.backend_pushed.insert(key, theirs);
            }
            _ => {}
        }
        self.send(&message)
    }

    /// Adds the check's diagnostics to the backend's report for `uri` (a
    /// `DocumentDiagnosticReport`). An `unchanged` report becomes a full one again, since the
    /// check's part may have changed since.
    fn merge_pulled(&mut self, uri: &str, report: &mut Value) {
        let key = check::uri_key(uri);
        match report["kind"].as_str() {
            Some("full") => {
                let theirs = report["items"].as_array().cloned().unwrap_or_default();
                report["items"] = json!(check::merge(&theirs, &self.check_diagnostics(&key)));
                self.backend_pulled.insert(key, theirs);
            }
            Some("unchanged") => {
                if let Some(theirs) = self.backend_pulled.get(&key) {
                    let items = check::merge(theirs, &self.check_diagnostics(&key));
                    *report = json!({ "kind": "full", "resultId": report["resultId"], "items": items });
                }
            }
            _ => {}
        }
    }

    fn backend_exited(&mut self) -> Result<()> {
        let Some(mut backend) = self.backend.take() else { return Ok(()) };
        if let Some(child) = &mut backend.child {
            let _ = child.wait();
        }
        // Answer what it left unanswered, so the editor doesn't wait forever.
        if let Some(id) = self.initialize_id.take() {
            self.pending.remove(&id_key(&id));
            let result =
                json!({ "capabilities": own_capabilities(), "serverInfo": { "name": "kargo", "version": env!("CARGO_PKG_VERSION") } });
            self.send(&jsonrpc::response(&id, result))?;
        }
        for id in std::mem::take(&mut self.pending) {
            let id: Value = serde_json::from_str(&id).unwrap_or(Value::Null);
            self.send(&jsonrpc::error(&id, -32603, "the Kotlin language server exited"))?;
        }
        if !self.shutdown {
            self.message(1, "kargo lsp: the Kotlin language server exited; kargo.toml support keeps working", false)?;
        }
        Ok(())
    }

    fn initialize(&mut self, message: Value) -> Result<()> {
        let params = &message["params"];
        self.client_progress = params["capabilities"]["window"]["workDoneProgress"] == true;
        self.client_watches = params["capabilities"]["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"] == true;
        self.client_apply_edit = params["capabilities"]["workspace"]["applyEdit"] == true;
        self.client_refresh = params["capabilities"]["workspace"]["diagnostics"]["refreshSupport"] == true;
        self.client_label_details = params["capabilities"]["textDocument"]["completion"]["completionItem"]["labelDetailsSupport"] == true;
        self.status = params["initializationOptions"]["status"] == true;
        // Kargo's own server reports no compiler errors, so `kargo check` does on save.
        self.check = check::Settings::from_options(&params["initializationOptions"], self.opts.server == KotlinServer::Kargo);
        let dir = params["rootUri"]
            .as_str()
            .and_then(text::uri_to_path)
            .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
            .or_else(|| params["workspaceFolders"][0]["uri"].as_str().and_then(text::uri_to_path))
            .or_else(|| std::env::current_dir().ok());
        self.root = dir.as_deref().and_then(project::project_root);

        // Import before the backend starts, so it finds the project description right away.
        let kind = self.opts.server.external();
        let mut metadata: Option<Value> = None;
        let mut imported = false;
        if let Some(root) = self.root.clone() {
            self.fingerprint = project::fingerprint(&root).ok();
            let fresh = kind == Some(ServerKind::KotlinLsp) && project::is_fresh(&root, ServerKind::KotlinLsp, self.opts.targets);
            if kind.is_some() && !fresh {
                imported = true;
                let token = params.get("workDoneToken").filter(|t| !t.is_null()).cloned();
                if let Some(token) = &token {
                    self.notify("$/progress", json!({ "token": token, "value": { "kind": "begin", "title": "Importing Kargo project" } }))?;
                }
                let outcome = project::import(&self.config, &root, kind, self.opts.targets);
                if let Some(token) = &token {
                    self.notify("$/progress", json!({ "token": token, "value": { "kind": "end" } }))?;
                }
                self.report_import(&outcome, true)?;
                metadata = outcome.metadata;
            }
        }
        if self.backend.is_none() && self.opts.server == KotlinServer::Kargo {
            let (reader, writer) = crate::kotlin::spawn(self.config.clone());
            self.attach(reader, writer, None);
        }
        if self.backend.is_none()
            && let Some(kind) = kind
            && let Err(e) = self.start_backend(kind, metadata.as_ref())
        {
            self.message(1, format!("kargo lsp: could not start the Kotlin language server: {e:#}"), false)?;
        }
        if self.backend.is_some() {
            self.initialize_id = Some(message["id"].clone());
            self.pending.insert(id_key(&message["id"]));
            let mut message = message;
            if let Some(params) = message["params"].as_object_mut() {
                // Kargo reported its import with the editor's token and ended it.
                if imported {
                    params.remove("workDoneToken");
                }
                // The editor may have opened a workspace member; the server reads the
                // description at the project root.
                // (With several workspace folders, the backend finds each one's root itself.)
                if let Some(root) = &self.root
                    && dir.as_deref() != Some(root.as_path())
                    && params.get("workspaceFolders").and_then(Value::as_array).is_none_or(|folders| folders.len() <= 1)
                {
                    let uri = text::path_to_uri(root);
                    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    params.insert("rootUri".into(), json!(uri));
                    params.insert("rootPath".into(), json!(root.display().to_string()));
                    params.insert("workspaceFolders".into(), json!([{ "uri": uri, "name": name }]));
                }
                if kind == Some(ServerKind::KotlinLsp) && self.root.as_ref().is_some_and(|r| r.join("workspace.json").is_file()) {
                    prefer_workspace_json(params);
                }
            }
            self.send_backend(&message);
            return Ok(());
        }
        let result = json!({ "capabilities": own_capabilities(), "serverInfo": { "name": "kargo", "version": env!("CARGO_PKG_VERSION") } });
        self.send(&jsonrpc::response(&message["id"], result))
    }

    fn start_backend(&mut self, kind: ServerKind, metadata: Option<&Value>) -> Result<()> {
        let jdk = &metadata.unwrap_or(&Value::Null)["toolchain"]["jdk"];
        let mut java_home = jdk["home"].as_str().map(PathBuf::from);
        // kotlin-language-server's compiler can't run on newer JDKs: use a managed one then.
        if kind == ServerKind::Kls
            && self.opts.server_path.is_none()
            && !jdk["version"].as_u64().is_some_and(|v| v <= u64::from(KLS_MAX_JDK))
        {
            let request = kargo_manifest::JdkRequest::Major(KLS_MAX_JDK);
            java_home = Some(kargo_toolchain::jdks::resolve(&self.config, &request)?.home);
        }
        let (exe, mut cmd) = match &self.opts.server_path {
            Some(path) => (path.clone(), Command::new(path)),
            None => {
                let server = LanguageServer::ensure(&self.config, kind, kind.default_version())?;
                match kind {
                    // kotlin-language-server's Windows launcher lists every jar on one command
                    // line, which is too long for cmd.exe; start its main class on `java` instead.
                    ServerKind::Kls => {
                        let java = match &java_home {
                            Some(home) => home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }),
                            None => PathBuf::from("java"),
                        };
                        let mut cmd = Command::new(&java);
                        cmd.arg(format!("-DkotlinLanguageServer.version={}", server.version));
                        cmd.arg("-cp").arg(server.home.join("lib").join("*")).arg(KLS_MAIN_CLASS);
                        (java, cmd)
                    }
                    ServerKind::KotlinLsp => {
                        let exe = server.executable().context("the language server has no launcher")?;
                        (exe.clone(), Command::new(exe))
                    }
                    ServerKind::Kda => anyhow::bail!("kotlin-debug-adapter is not a language server"),
                }
            }
        };
        cmd.args(kind.stdio_args()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        if let Some(root) = &self.root {
            cmd.current_dir(root);
        }
        if kind.needs_jdk()
            && let Some(home) = &java_home
        {
            cmd.env("JAVA_HOME", home);
        }
        self.config.shell().verbose("Running", kargo_util::process::describe(&cmd));
        let mut child = cmd.spawn().with_context(|| format!("failed to run {}", exe.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        self.attach(Box::new(BufReader::new(stdout)), Box::new(stdin), Some(child));
        Ok(())
    }

    fn attach(&mut self, reader: Box<dyn BufRead + Send>, writer: Box<dyn Write + Send>, child: Option<Child>) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut reader = reader;
            while let Ok(Some(message)) = jsonrpc::read(&mut reader) {
                if tx.send(Event::Backend(Some(message))).is_err() {
                    return;
                }
            }
            let _ = tx.send(Event::Backend(None));
        });
        self.backend = Some(Backend { stdin: writer, child });
    }

    fn stop_backend(&mut self) {
        let Some(mut backend) = self.backend.take() else { return };
        drop(backend.stdin);
        if let Some(child) = &mut backend.child {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Asks the editor to tell Kargo when any `kargo.toml` or `kargo.lock` changes.
    fn watch_manifests(&mut self) -> Result<()> {
        if !self.client_watches {
            return Ok(());
        }
        let id = self.next_id();
        let watchers = json!([{ "globPattern": "**/kargo.toml" }, { "globPattern": "**/kargo.lock" }]);
        let params = json!({ "registrations": [{ "id": "kargo-watch", "method": "workspace/didChangeWatchedFiles", "registerOptions": { "watchers": watchers } }] });
        self.send(&jsonrpc::request(&id, "client/registerCapability", params))
    }

    fn own_response(&mut self, id: String, _message: &Value) -> Result<()> {
        // The editor accepted the token of a check that is still running.
        if let Some(progress) = &mut self.check_progress
            && progress.create_id == id
            && !progress.begun
        {
            progress.begun = true;
            let token = progress.token.clone();
            self.notify(
                "$/progress",
                json!({ "token": token, "value": { "kind": "begin", "title": "kargo check", "cancellable": false } }),
            )?;
        }
        // The editor accepted a progress token: report the import if it is still running.
        if let Some(progress) = &mut self.progress
            && progress.create_id == id
            && !progress.begun
            && self.importing
        {
            progress.begun = true;
            let token = progress.token.clone();
            self.notify(
                "$/progress",
                json!({ "token": token, "value": { "kind": "begin", "title": "Importing Kargo project", "cancellable": false } }),
            )?;
        }
        Ok(())
    }

    fn maybe_reimport(&mut self) -> Result<()> {
        let Some(root) = self.root.clone() else { return Ok(()) };
        let Ok(fingerprint) = project::fingerprint(&root) else { return Ok(()) };
        if self.fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return Ok(());
        }
        if self.importing {
            self.reimport = true;
            return Ok(());
        }
        self.fingerprint = Some(fingerprint);
        self.importing = true;
        self.status("resolving", true, None)?;
        if self.client_progress {
            let create_id = self.next_id();
            let token = format!("{create_id}/import");
            self.send(&jsonrpc::request(&create_id, "window/workDoneProgress/create", json!({ "token": token })))?;
            self.progress = Some(Progress { token, create_id, begun: false });
        }
        let (config, tx, kind, targets) = (self.config.clone(), self.tx.clone(), self.opts.server.external(), self.opts.targets);
        thread::spawn(move || {
            let outcome = project::import(&config, &root, kind, targets);
            // Keep the Zed tasks `kargo ide --zed` wrote current too.
            let _ = crate::zed::refresh(&config, &root);
            let _ = tx.send(Event::Imported(outcome));
        });
        Ok(())
    }

    fn imported(&mut self, outcome: project::Outcome) -> Result<()> {
        self.importing = false;
        self.status("resolving", false, Some(if outcome.error.is_some() { "failed" } else { "done" }))?;
        if let Some(progress) = self.progress.take()
            && progress.begun
        {
            let message = if outcome.error.is_some() { "failed" } else { "done" };
            self.notify("$/progress", json!({ "token": progress.token, "value": { "kind": "end", "message": message } }))?;
        }
        self.report_import(&outcome, false)?;
        let changes: Vec<Value> = outcome.written.changed.iter().map(|path| json!({ "uri": text::path_to_uri(path), "type": 2 })).collect();
        if !changes.is_empty() {
            self.send_backend(&jsonrpc::notification("workspace/didChangeWatchedFiles", json!({ "changes": changes })));
        }
        let open: Vec<String> = self.docs.keys().cloned().collect();
        for uri in open {
            self.compute_hints(&uri);
        }
        if std::mem::take(&mut self.reimport) {
            self.maybe_reimport()?;
        }
        Ok(())
    }

    fn report_import(&mut self, outcome: &project::Outcome, initial: bool) -> Result<()> {
        if let Some(error) = &outcome.error {
            // kargo.toml's diagnostics show the details; a popup on every save would be noise.
            self.message(if initial { 2 } else { 3 }, format!("kargo lsp: importing the project failed: {error}"), !initial)?;
        }
        if !outcome.written.foreign.is_empty() && !std::mem::replace(&mut self.warned_foreign, true) {
            let names: Vec<String> = outcome.written.foreign.iter().map(|p| p.display().to_string()).collect();
            let text = format!(
                "kargo lsp: {} was not written by Kargo, so it is left alone; delete it to let `kargo lsp` describe the project",
                names.join(", ")
            );
            self.message(2, text, false)?;
        }
        Ok(())
    }

    fn runnables(&mut self, message: &Value) -> Result<Vec<Value>> {
        let file = document_uri(message).and_then(text::uri_to_path);
        let root = match (&self.root, &file) {
            (Some(root), _) => root.clone(),
            (None, Some(file)) => file.parent().and_then(project::project_root).context("the file is not in a Kargo project")?,
            (None, None) => anyhow::bail!("no Kargo project is open"),
        };
        let metadata = project::metadata(&self.config, &root, true)?;
        let kargo = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "kargo".to_owned());
        Ok(project::runnables(&metadata, &kargo, file.as_deref()))
    }

    /// Answers a completion on a dependency's coordinate or version from a worker thread, since
    /// it may search Maven Central or read the repositories. Returns whether it did.
    fn dependency_completion(&mut self, method: &str, message: &Value) -> bool {
        if method != "textDocument/completion" {
            return false;
        }
        let uri = document_uri(message).unwrap_or_default();
        let Some(text) = self.docs.get(uri).filter(|_| is_manifest(uri)) else { return false };
        let offset = text::offset(text, &message["params"]["position"]);
        let Some(request) = deps::request(text, offset) else { return false };
        let (text, id, tx) = (text.clone(), message["id"].clone(), self.tx.clone());
        let path = text::uri_to_path(uri).unwrap_or_else(|| PathBuf::from(kargo_manifest::MANIFEST_NAME));
        let (config, searcher) = (self.config.clone(), Arc::clone(&self.searcher));
        thread::spawn(move || {
            let result = match &request {
                deps::Request::Coordinate { partial, .. } => {
                    let hits = searcher.search(partial);
                    // Results depend on what is typed, so the editor asks again on every keystroke.
                    json!({ "isIncomplete": true, "items": deps::coordinate_items(&text, &request, &hits.unwrap_or_default()) })
                }
                deps::Request::Version { module, .. } => {
                    let versions = deps::versions(&config, deps::repositories(&text, &path), module);
                    json!({ "isIncomplete": false, "items": deps::version_items(&text, &request, &versions) })
                }
                deps::Request::NpmName { partial, .. } => {
                    let registry = kargo_ops::npm_registry::Registry::new(&config, path.parent().unwrap_or(Path::new(".")));
                    let names = searcher.search_npm(&registry, partial);
                    json!({ "isIncomplete": true, "items": deps::npm_name_items(&text, &request, &names.unwrap_or_default()) })
                }
                deps::Request::NpmVersion { package, .. } => {
                    let versions = deps::npm_versions(&config, path.parent().unwrap_or(Path::new(".")), package);
                    json!({ "isIncomplete": false, "items": deps::npm_version_items(&text, &request, &versions) })
                }
            };
            let _ = tx.send(Event::Reply(jsonrpc::response(&id, result)));
        });
        true
    }

    /// Works out the newer-version hints of the open `kargo.toml` at `uri` on a worker thread,
    /// from the manifest on disk.
    fn compute_hints(&self, uri: &str) {
        let Some(path) = text::uri_to_path(uri) else { return };
        let (config, tx, uri) = (self.config.clone(), self.tx.clone(), uri.to_owned());
        thread::spawn(move || {
            let hints = deps::hints(&config, &path);
            let _ = tx.send(Event::Hints { uri, hints });
        });
    }

    fn manifest_request(&mut self, method: &str, message: &Value) -> Value {
        let uri = document_uri(message).unwrap_or_default();
        let Some(text) = self.docs.get(uri).filter(|_| is_manifest(uri)) else { return Value::Null };
        let offset = text::offset(text, &message["params"]["position"]);
        if method == "textDocument/codeAction" {
            let params = &message["params"];
            let path = text::uri_to_path(uri).unwrap_or_else(|| PathBuf::from(kargo_manifest::MANIFEST_NAME));
            let start = text::offset(text, &params["range"]["start"]);
            let end = text::offset(text, &params["range"]["end"]).max(start);
            let only = params["context"]["only"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            let builtins = &self.opts.builtin_commands;
            let request = actions::Request {
                uri,
                text,
                path: &path,
                range: start..end,
                only,
                hints: self.hints.get(uri).map(Vec::as_slice).unwrap_or_default(),
                is_builtin_command: &|name| builtins.iter().any(|b| b == name),
            };
            return json!(actions::code_actions(&request));
        }
        match method {
            "textDocument/completion" => json!({ "isIncomplete": false, "items": manifest::completion(text, offset) }),
            "textDocument/hover" => manifest::hover(text, offset).unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    /// Runs one of [`actions::COMMANDS`]. Returns the reply, or `None` when a worker thread
    /// answers later.
    fn execute_command(&mut self, message: &Value) -> Result<Option<Value>> {
        let params = &message["params"];
        let uri = actions::command_uri(params)?;
        let path = actions::command_path(&uri)?;
        let original = match self.docs.get(&uri) {
            Some(text) => text.clone(),
            None => std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?,
        };
        if self.docs.contains_key(&uri) && !self.client_apply_edit {
            anyhow::bail!("the editor can't apply edits from the server; save kargo.toml and run the kargo command instead");
        }
        match params["command"].as_str().unwrap_or_default() {
            actions::ADD_TARGET => {
                let (new, scaffold) = actions::add_target(&path, &original, params)?;
                self.write_manifest(&uri, &original, &new)?;
                Ok(Some(scaffold()?))
            }
            _ => {
                // Picking versions may use the network.
                let (config, tx, id, params) = (self.config.clone(), self.tx.clone(), message["id"].clone(), params.clone());
                thread::spawn(move || {
                    let outcome = actions::add_dependency(&config, &path, &original, &params);
                    let _ = tx.send(Event::Edited { id, uri, original, outcome });
                });
                Ok(None)
            }
        }
    }

    /// Changes the `kargo.toml` at `uri` from `original` to `new`: through the editor when it has
    /// the document open, else on disk. Fails if it changed since `original` was read.
    fn write_manifest(&mut self, uri: &str, original: &str, new: &str) -> Result<()> {
        let path = actions::command_path(uri)?;
        let current = match self.docs.get(uri) {
            Some(text) => text.clone(),
            None => std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?,
        };
        if current != original {
            anyhow::bail!("kargo.toml changed in the meantime; nothing was changed, try again");
        }
        let Some(edit) = text::edit(original, new) else { return Ok(()) };
        if !self.docs.contains_key(uri) {
            return kargo_util::fs::write_string_atomic(&path, new);
        }
        let id = self.next_id();
        let params = json!({ "label": "Edit kargo.toml", "edit": { "changes": { uri: [edit] } } });
        self.send(&jsonrpc::request(&id, "workspace/applyEdit", params))
    }

    fn manifest_notification(&mut self, method: &str, message: &Value) -> Result<()> {
        let uri = document_uri(message).unwrap_or_default().to_owned();
        let params = &message["params"];
        match method {
            "textDocument/didOpen" if is_manifest(&uri) => {
                self.docs.insert(uri.clone(), params["textDocument"]["text"].as_str().unwrap_or_default().to_owned());
                self.publish(&uri)?;
                self.compute_hints(&uri);
            }
            "textDocument/didChange" if is_manifest(&uri) => {
                if let Some(text) = self.docs.get_mut(&uri) {
                    text::apply_changes(text, params["contentChanges"].as_array().map(Vec::as_slice).unwrap_or_default());
                    self.publish(&uri)?;
                }
            }
            "textDocument/didClose" if is_manifest(&uri) => {
                self.docs.remove(&uri);
                self.hints.remove(&uri);
                self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] }))?;
            }
            "textDocument/didSave" => {
                // Without watchers the editor doesn't say the manifest changed, and the backend
                // reloads the layout from that.
                if is_manifest(&uri) && !self.client_watches {
                    let changes = json!([{ "uri": uri, "type": 2 }]);
                    self.send_backend(&jsonrpc::notification("workspace/didChangeWatchedFiles", json!({ "changes": changes })));
                }
                self.maybe_reimport()?
            }
            _ => {}
        }
        Ok(())
    }

    /// Tracks the Kotlin and Java documents the editor opens and closes, and checks on save.
    fn source_notification(&mut self, method: &str, message: &Value) -> Result<()> {
        let Some(uri) = document_uri(message).map(str::to_owned) else { return Ok(()) };
        let key = check::uri_key(&uri);
        match method {
            "textDocument/didOpen" => {
                self.open_sources.insert(key.clone());
                // Pulled reports carry the check's diagnostics from now on.
                if self.backend_pull && self.pushed.remove(&key) {
                    self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] }))?;
                }
            }
            "textDocument/didClose" => {
                self.open_sources.remove(&key);
                self.backend_pulled.remove(&key);
                // The editor forgets pulled diagnostics of a closed document; push the check's.
                if self.backend_pull && !self.check_diagnostics(&key).is_empty() {
                    self.publish_checked(&key, &uri)?;
                }
            }
            "textDocument/didSave" if check::is_checked_source(&uri) => self.start_check(&uri)?,
            _ => {}
        }
        Ok(())
    }

    /// Checks the package of the file at `uri` on a worker thread, cancelling the running check.
    fn start_check(&mut self, uri: &str) -> Result<()> {
        if !self.check.enabled {
            return Ok(());
        }
        let Some(manifest) = text::uri_to_path(uri).as_deref().and_then(check::manifest_for) else { return Ok(()) };
        self.cancel_check();
        self.end_check_progress("cancelled")?;
        self.check_generation += 1;
        let generation = self.check_generation;
        self.status("checking", true, None)?;
        if self.client_progress {
            let create_id = self.next_id();
            let token = format!("{create_id}/check");
            self.send(&jsonrpc::request(&create_id, "window/workDoneProgress/create", json!({ "token": token })))?;
            self.check_progress = Some(Progress { token, create_id, begun: false });
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.check_cancel = Some(Arc::clone(&cancel));
        let run = check::Run { manifest: manifest.clone(), targets: self.check.targets.clone(), offline: self.config.offline() };
        let (runner, tx) = (Arc::clone(&self.runner), self.tx.clone());
        thread::spawn(move || {
            let outcome = runner(&run, &cancel);
            let _ = tx.send(Event::Checked { generation, manifest, outcome });
        });
        Ok(())
    }

    /// Stops the running check, if any.
    fn cancel_check(&mut self) {
        if let Some(cancel) = self.check_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn end_check_progress(&mut self, message: &str) -> Result<()> {
        match self.check_progress.take() {
            Some(progress) if progress.begun => {
                self.notify("$/progress", json!({ "token": progress.token, "value": { "kind": "end", "message": message } }))
            }
            _ => Ok(()),
        }
    }

    /// Ends the progress and the `kargo/status` of a check that finished, saying how.
    fn finish_check(&mut self, message: &str) -> Result<()> {
        self.end_check_progress(message)?;
        self.status("checking", false, Some(message))
    }

    fn checked_event(&mut self, generation: u64, manifest: PathBuf, outcome: Result<check::Output>) -> Result<()> {
        if generation != self.check_generation {
            return Ok(());
        }
        self.check_cancel = None;
        let output = match outcome {
            Ok(output) if !output.failed_elsewhere() => output,
            Ok(output) => {
                // Not about the sources (an unresolvable dependency, a broken kargo.toml): keep
                // the diagnostics there are and say why in the log.
                self.finish_check("failed")?;
                let lines: Vec<&str> = output.stderr.lines().collect();
                let tail = lines[lines.len().saturating_sub(20)..].join("\n");
                return self.message(2, format!("kargo lsp: `kargo check` failed:\n{tail}"), true);
            }
            Err(e) => {
                self.finish_check("failed")?;
                return self.message(2, format!("kargo lsp: could not run `kargo check`: {e:#}"), true);
            }
        };
        let found = check::diagnostics(&output.messages, &manifest);
        let errors = found.values().flat_map(|(_, list)| list).filter(|d| d["severity"] == 1).count();
        self.finish_check(&match errors {
            0 => "done".to_owned(),
            1 => "1 error".to_owned(),
            n => format!("{n} errors"),
        })?;
        let previous = self.checked.insert(manifest, found.clone()).unwrap_or_default();
        // Publish where diagnostics appeared, changed or went away.
        let mut affected: Vec<(String, String)> = found.into_iter().map(|(key, (uri, _))| (key, uri)).collect();
        for (key, (uri, _)) in previous {
            if !affected.iter().any(|(k, _)| *k == key) {
                affected.push((key, uri));
            }
        }
        let mut refresh = false;
        for (key, uri) in affected {
            if self.backend_pull && self.open_sources.contains(&key) {
                refresh = true;
            } else {
                self.publish_checked(&key, &uri)?;
            }
        }
        if refresh && self.client_refresh {
            let id = self.next_id();
            self.send(&jsonrpc::request(&id, "workspace/diagnostic/refresh", Value::Null))?;
        }
        Ok(())
    }

    /// The check diagnostics for the document with [`check::uri_key`] `key`, from every package.
    fn check_diagnostics(&self, key: &str) -> Vec<Value> {
        self.checked.values().filter_map(|files| files.get(key)).flat_map(|(_, list)| list.iter().cloned()).collect()
    }

    /// Pushes the diagnostics of the document at `uri` again, with the check's merged in.
    fn publish_checked(&mut self, key: &str, uri: &str) -> Result<()> {
        // An open kargo.toml has Kargo's own diagnostics, which `publish` merges with the check's.
        if let Some(open) = self.docs.keys().find(|d| check::uri_key(d) == key).cloned() {
            return self.publish(&open);
        }
        let ours = self.check_diagnostics(key);
        let theirs = self.backend_pushed.get(key).cloned().unwrap_or_default();
        if self.backend_pull {
            if ours.is_empty() && !self.pushed.remove(key) {
                return Ok(());
            }
            if !ours.is_empty() {
                self.pushed.insert(key.to_owned());
            }
        }
        self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": check::merge(&theirs, &ours) }))
    }

    /// Answers a completion or hover on a Kargo script's `@file:` annotation. The network-bound
    /// values (`DependsOn` and `Kotlin` versions) go to a worker thread.
    fn script_request(&self, method: &str, message: &Value) -> script::Answer {
        let uri = document_uri(message).unwrap_or_default().to_owned();
        let Some(text) = self.docs.get(&uri) else { return script::Answer::Backend };
        let offset = text::offset(text, &message["params"]["position"]);
        if method == "textDocument/hover" {
            return script::hover(text, offset).map_or(script::Answer::Backend, script::Answer::Now);
        }
        let Some(request) = script::request(text, offset) else { return script::Answer::Backend };
        if matches!(&request, script::Request::Value { name, .. } if name == "DependsOn" || name == "Kotlin") {
            self.script_versions(message["id"].clone(), text.clone(), request);
            return script::Answer::Later;
        }
        script::Answer::Now(json!({ "isIncomplete": false, "items": script::completion(text, &request) }))
    }

    /// Completes the coordinate or version of `@file:DependsOn`, and the version of `@file:Kotlin`,
    /// on a worker thread: coordinates from the search, versions from the script's repositories.
    fn script_versions(&self, id: Value, text: String, request: script::Request) {
        let (config, searcher, tx) = (self.config.clone(), Arc::clone(&self.searcher), self.tx.clone());
        let repositories = kargo_ops::script::declared_repositories(&text);
        thread::spawn(move || {
            let script::Request::Value { name, partial, range } = &request else {
                let _ = tx.send(Event::Reply(jsonrpc::response(&id, json!({ "isIncomplete": false, "items": [] }))));
                return;
            };
            let result = if name == "Kotlin" {
                let module = kargo_maven::ModuleId::new("org.jetbrains.kotlin", "kotlin-compiler-embeddable");
                let versions = deps::versions(&config, repositories, &module);
                let request = deps::Request::Version { module, range: range.clone(), quoted: true };
                json!({ "isIncomplete": false, "items": deps::version_items(&text, &request, &versions) })
            } else if let Some(colon) = partial.rfind(':').filter(|_| partial.matches(':').count() >= 2) {
                // `group:artifact:` typed: complete the version after the last colon.
                let Ok(module) = partial[..colon].parse::<kargo_maven::ModuleId>() else {
                    let _ = tx.send(Event::Reply(jsonrpc::response(&id, json!({ "isIncomplete": false, "items": [] }))));
                    return;
                };
                let versions = deps::versions(&config, repositories, &module);
                let version = deps::Request::Version { module, range: range.start + colon + 1..range.end, quoted: true };
                json!({ "isIncomplete": false, "items": deps::version_items(&text, &version, &versions) })
            } else if partial.len() >= 2 {
                let hits = searcher.search(partial).unwrap_or_default();
                let coordinate = deps::Request::Coordinate {
                    partial: partial.clone(),
                    range: range.clone(),
                    key: false,
                    quoted: true,
                    has_value: false,
                };
                json!({ "isIncomplete": true, "items": deps::coordinate_items(&text, &coordinate, &hits) })
            } else {
                json!({ "isIncomplete": false, "items": [] })
            };
            let _ = tx.send(Event::Reply(jsonrpc::response(&id, result)));
        });
    }

    /// Tracks the Kargo scripts the editor opens, changes and closes, and publishes the problems of
    /// their headers. Other `.kts` files are left to the Kotlin server.
    fn script_notification(&mut self, method: &str, message: &Value) -> Result<()> {
        let Some(uri) = document_uri(message).map(str::to_owned) else { return Ok(()) };
        if !script::is_script_uri(&uri) {
            return Ok(());
        }
        let params = &message["params"];
        match method {
            "textDocument/didOpen" => {
                let text = params["textDocument"]["text"].as_str().unwrap_or_default();
                if text::uri_to_path(&uri).is_some_and(|path| script::is_kargo_script(&path, text)) {
                    self.docs.insert(uri.clone(), text.to_owned());
                    self.publish_script(&uri)?;
                }
            }
            "textDocument/didChange" => {
                if let Some(text) = self.docs.get_mut(&uri) {
                    text::apply_changes(text, params["contentChanges"].as_array().map(Vec::as_slice).unwrap_or_default());
                    self.publish_script(&uri)?;
                }
            }
            "textDocument/didClose" if self.docs.remove(&uri).is_some() => {
                self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] }))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Publishes the problems of a tracked Kargo script's header; the backend's own diagnostics are
    /// added to them when it reports.
    fn publish_script(&mut self, uri: &str) -> Result<()> {
        let Some(text) = self.docs.get(uri) else { return Ok(()) };
        let diagnostics = script::diagnostics(text);
        self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": diagnostics }))
    }

    fn publish(&mut self, uri: &str) -> Result<()> {
        let Some(text) = self.docs.get(uri) else { return Ok(()) };
        let path = text::uri_to_path(uri).unwrap_or_else(|| PathBuf::from(kargo_manifest::MANIFEST_NAME));
        let builtins = &self.opts.builtin_commands;
        let problems = manifest::problems(text, Path::new(&path), &|name| builtins.iter().any(|b| b == name));
        let mut diagnostics: Vec<Value> = problems.iter().map(|p| manifest::diagnostic(text, p)).collect();
        // Newer versions, as information; a hint whose dependency was edited away is dropped.
        diagnostics.extend(self.hints.get(uri).into_iter().flatten().filter_map(|hint| deps::diagnostic(text, hint)));
        // Messages of `kargo check` without a file, such as the `-Werror` summary.
        diagnostics.extend(self.check_diagnostics(&check::uri_key(uri)));
        self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": diagnostics }))
    }
}

/// Runs the server over stdin and stdout.
pub fn run(config: Config, opts: Options) -> Result<bool> {
    serve(config, opts, BufReader::new(io::stdin()), io::stdout())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};
    use std::io::{PipeReader, PipeWriter};

    #[test]
    fn merges_capabilities() {
        let merged = merge_capabilities(&json!({
            "textDocumentSync": 1,
            "completionProvider": { "triggerCharacters": ["."] },
            "definitionProvider": true,
        }));
        assert_eq!(merged["textDocumentSync"], json!({ "openClose": true, "change": 1, "save": { "includeText": false } }));
        assert_eq!(merged["completionProvider"]["triggerCharacters"], json!([".", "[", "\"", "=", " ", ","]));
        assert_eq!(merged["hoverProvider"], true);
        assert_eq!(merged["definitionProvider"], true);
        assert_eq!(merged["experimental"]["kargo"]["runnables"], true);
        let merged = merge_capabilities(&json!({ "textDocumentSync": { "openClose": true, "change": 2, "save": true } }));
        assert_eq!(merged["textDocumentSync"]["save"], true);
        assert_eq!(merge_capabilities(&Value::Null), own_capabilities());
    }

    #[test]
    fn gives_completion_items_without_a_detail_their_label_details() {
        let mut list = json!({
            "isIncomplete": false,
            "items": [
                { "label": "print", "labelDetails": { "description": "Unit", "detail": "(message: Int) (kotlin.io)" } },
                { "label": "print", "detail": "kept", "labelDetails": { "detail": "(message: Any?)" } },
                { "label": "println", "labelDetails": { "description": "kotlin.io" } },
                { "label": "plain" },
            ],
        });
        label_details_as_detail(&mut list);
        let details: Vec<Option<&str>> = list["items"].as_array().unwrap().iter().map(|i| i["detail"].as_str()).collect();
        assert_eq!(details, [Some("(message: Int) (kotlin.io)"), Some("kept"), Some("kotlin.io"), None]);
        assert_eq!(list["items"][0]["labelDetails"]["description"], "Unit", "the item keeps what the editor may show");

        let mut array = json!([{ "label": "print", "labelDetails": { "detail": "(message: Int)" } }]);
        label_details_as_detail(&mut array);
        assert_eq!(array[0]["detail"], "(message: Int)");
        let mut none = Value::Null;
        label_details_as_detail(&mut none);
        assert!(none.is_null());
    }

    /// An editor talking to `serve_with` over pipes.
    struct Editor {
        input: PipeWriter,
        output: BufReader<PipeReader>,
        server: Option<thread::JoinHandle<Result<bool>>>,
    }

    impl Editor {
        fn start(opts: Options, backend: Option<BackendIo>) -> (Self, tempfile::TempDir) {
            Self::start_with(opts, backend, Arc::new(|_: &check::Run, _: &AtomicBool| anyhow::bail!("no checks in this test")))
        }

        fn start_with(opts: Options, backend: Option<BackendIo>, runner: check::Runner) -> (Self, tempfile::TempDir) {
            let home = tempfile::tempdir().unwrap();
            let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
            let (server_in, input) = io::pipe().unwrap();
            let (output, server_out) = io::pipe().unwrap();
            let server = thread::spawn(move || serve_with(config, opts, BufReader::new(server_in), server_out, backend, runner));
            (Self { input, output: BufReader::new(output), server: Some(server) }, home)
        }

        fn send(&mut self, message: Value) {
            jsonrpc::write(&mut self.input, &message).unwrap();
        }

        fn recv(&mut self) -> Value {
            jsonrpc::read(&mut self.output).unwrap().expect("a message")
        }

        /// Reads until the response to `id`, returning it and the messages before it.
        fn response(&mut self, id: i64) -> (Value, Vec<Value>) {
            let mut before = Vec::new();
            loop {
                let message = self.recv();
                if message["id"] == id && jsonrpc::kind(&message) == Kind::Response {
                    return (message, before);
                }
                before.push(message);
            }
        }

        fn request(&mut self, id: i64, method: &str, params: Value) -> (Value, Vec<Value>) {
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
            self.response(id)
        }

        fn finish(mut self) -> bool {
            self.request(999, "shutdown", Value::Null);
            self.send(jsonrpc::notification("exit", Value::Null));
            self.server.take().unwrap().join().unwrap().unwrap()
        }
    }

    fn project(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), manifest).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/Main.kt"), "fun main() {}\n").unwrap();
        dir
    }

    const MANIFEST: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[tasks]\nhello = \"echo hi\"\n";

    #[test]
    fn serves_kargo_scripts_annotations_and_their_problems() {
        // A script outside any project is a Kargo script.
        let dir = tempfile::tempdir().unwrap();
        let uri = text::path_to_uri(&dir.path().join("fetch.kts"));
        let (mut editor, _home) = Editor::start(Options::default(), None);
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} }));
        assert_eq!(init["result"]["capabilities"]["hoverProvider"], true);
        editor.send(jsonrpc::notification("initialized", json!({})));

        let text = "@file:DependsOn(\"a:b\")\n@file:Ke\nprintln(1)\n";
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": text } }),
        ));
        // The header's problems are published as the script opens: `a:b` has no version.
        let published = editor.recv();
        assert_eq!(published["method"], "textDocument/publishDiagnostics", "{published}");
        assert_eq!(published["params"]["diagnostics"][0]["range"]["start"], json!({ "line": 0, "character": 0 }));
        assert!(published["params"]["diagnostics"][0]["message"].as_str().unwrap().contains("version pinned"), "{published}");

        let (completion, _) = editor.request(
            2,
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 1, "character": 9 } }),
        );
        let labels: Vec<&str> = completion["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert!(labels.contains(&"Kotlin") && labels.contains(&"DependsOn"), "{labels:?}");

        let (hover, _) =
            editor.request(3, "textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 9 } }));
        assert!(hover["result"]["contents"]["value"].as_str().unwrap().contains("@file:DependsOn"), "{hover}");

        assert!(editor.finish());
    }

    #[test]
    fn serves_kargo_toml_without_a_kotlin_server() {
        let dir = project(MANIFEST);
        let root = text::path_to_uri(dir.path());
        let uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let (mut editor, _home) = Editor::start(Options::default(), None);
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": root, "capabilities": {} }));
        assert_eq!(init["result"]["capabilities"]["hoverProvider"], true);
        editor.send(jsonrpc::notification("initialized", json!({})));

        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nnmae = \"x\"\n";
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": text } }),
        ));
        let diagnostics = editor.recv();
        assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
        assert_eq!(diagnostics["params"]["diagnostics"][0]["range"]["start"], json!({ "line": 3, "character": 0 }));

        let change = json!({ "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 4, "character": 0 } }, "text": "" });
        editor.send(jsonrpc::notification(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [change] }),
        ));
        assert_eq!(editor.recv()["params"]["diagnostics"], json!([]));

        let (completion, _) = editor.request(
            2,
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 3, "character": 0 } }),
        );
        let labels: Vec<&str> = completion["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert!(labels.contains(&"group") && !labels.contains(&"name"), "{labels:?}");

        let (hover, _) =
            editor.request(3, "textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": { "line": 1, "character": 1 } }));
        assert!(hover["result"]["contents"]["value"].as_str().unwrap().contains("package name"));

        let (runnables, _) = editor.request(4, "kargo/runnables", json!({}));
        let labels: Vec<&str> = runnables["result"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Run app (jvm)", "Test app (jvm)", "Task hello"]);

        let (missing, _) = editor.request(5, "textDocument/definition", json!({ "textDocument": { "uri": "file:///x/Main.kt" } }));
        assert_eq!(missing["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
        assert!(editor.finish(), "shutdown, then exit");
    }

    #[test]
    fn serves_kotlin_with_kargos_own_server() {
        let dir = project(MANIFEST);
        std::fs::create_dir_all(dir.path().join("src/model")).unwrap();
        std::fs::write(dir.path().join("src/model/User.kt"), "package model\n\n/** A user. */\nclass User(val name: String)\n").unwrap();
        let root = text::path_to_uri(dir.path());
        let main = dir.path().join("src/Main.kt");
        let uri = text::path_to_uri(&main);
        let opts = Options { server: KotlinServer::Kargo, ..Options::default() };
        let (mut editor, _home) = Editor::start(opts, None);
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": root, "capabilities": {} }));
        let caps = &init["result"]["capabilities"];
        assert_eq!(caps["definitionProvider"], true);
        assert_eq!(caps["renameProvider"]["prepareProvider"], true);
        assert_eq!(caps["experimental"]["kargo"]["runnables"], true, "Kargo's own capabilities are merged in");
        editor.send(jsonrpc::notification("initialized", json!({})));
        let text = "import model.User\n\nfun main() {\n    val user = User(\"a\")\n    printl\n    user.\n}\n";
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": text } }),
        ));
        let labels = |editor: &mut Editor, id: i64, line: u32, character: u32| -> Vec<String> {
            let (reply, _) = editor.request(
                id,
                "textDocument/completion",
                json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
            );
            reply["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect()
        };
        let members = labels(&mut editor, 2, 5, 9);
        assert!(members.contains(&"name".to_owned()) && members.contains(&"toString".to_owned()), "{members:?}");
        let names = labels(&mut editor, 3, 4, 10);
        assert!(names.contains(&"println".to_owned()), "{names:?}");

        let at =
            |line: u32, character: u32| json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } });
        let (hover, _) = editor.request(4, "textDocument/hover", at(3, 16));
        let hover = hover["result"]["contents"]["value"].as_str().unwrap().to_owned();
        assert!(hover.contains("class User(val name: String)") && hover.contains("A user."), "{hover}");
        let (definition, _) = editor.request(5, "textDocument/definition", at(3, 16));
        assert!(definition["result"][0]["uri"].as_str().unwrap().ends_with("src/model/User.kt"), "{definition}");
        assert_eq!(definition["result"][0]["range"]["start"], json!({ "line": 3, "character": 6 }));

        let (rename, _) = editor.request(
            6,
            "textDocument/rename",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 3, "character": 16 }, "newName": "Person" }),
        );
        let changes = rename["result"]["changes"].as_object().unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert_eq!(changes[&uri].as_array().unwrap().len(), 2, "the import and the call");
        let (library, _) = editor.request(7, "textDocument/prepareRename", at(3, 9));
        assert_eq!(library["result"]["placeholder"], "user");

        assert!(caps["codeActionProvider"]["codeActionKinds"].as_array().unwrap().contains(&json!("source.organizeImports")));
        let range = json!({ "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 4 } });
        let params = json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": [] } });
        let (actions, _) = editor.request(9, "textDocument/codeAction", params);
        assert_eq!(actions["result"], json!([]), "the Kotlin server answers code actions: none here");

        let (runnables, _) = editor.request(8, "kargo/runnables", json!({}));
        assert!(runnables["result"].as_array().is_some_and(|r| !r.is_empty()));
        assert!(editor.finish(), "shutdown, then exit");
    }

    #[test]
    fn completes_versions_and_hints_at_newer_ones() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("repo/com/acme/widget");
        std::fs::create_dir_all(&meta).unwrap();
        let versions: String = ["1.0.0", "1.1.0", "2.0.0-RC1"].iter().map(|v| format!("<version>{v}</version>")).collect();
        std::fs::write(
            meta.join("maven-metadata.xml"),
            format!("<metadata><versioning><versions>{versions}</versions></versioning></metadata>"),
        )
        .unwrap();
        let url = format!("file://{}", dir.path().join("repo").display()).replace('\\', "/");
        let text = format!(
            "[package]
name = \"app\"
version = \"0.1.0\"

[repositories]
local = \"{url}\"

[dependencies]
\"com.acme:widget\" = \"1.0.0\"
"
        );
        std::fs::write(dir.path().join("kargo.toml"), &text).unwrap();
        let uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let (mut editor, _home) = Editor::start(Options::default(), None);
        editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} }));
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": text } }),
        ));
        assert_eq!(editor.recv()["params"]["diagnostics"], json!([]));
        let hint = editor.recv();
        let diagnostic = &hint["params"]["diagnostics"][0];
        assert_eq!(diagnostic["severity"], 3);
        assert_eq!(diagnostic["message"], "`com.acme:widget` 1.1.0 is available");
        assert_eq!(diagnostic["range"]["start"], json!({ "line": 8, "character": 0 }));

        let (completion, _) = editor.request(
            2,
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 8, "character": 21 } }),
        );
        let labels: Vec<&str> = completion["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["1.1.0", "1.0.0", "2.0.0-RC1"]);
        // Offline, coordinates aren't searched.
        let (completion, _) = editor.request(
            3,
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 8, "character": 9 } }),
        );
        assert_eq!(completion["result"], json!({ "isIncomplete": true, "items": [] }));
        assert!(editor.finish());
    }

    #[test]
    fn answers_code_actions_and_runs_commands() {
        let dir = project(MANIFEST);
        let uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let (mut editor, _home) = Editor::start(Options::default(), None);
        let capabilities = json!({ "workspace": { "applyEdit": true } });
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": capabilities }));
        assert_eq!(init["result"]["capabilities"]["executeCommandProvider"]["commands"], json!(actions::COMMANDS));
        let text = MANIFEST.replace("version = ", "verison = ");
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "toml", "version": 1, "text": text } }),
        ));
        editor.recv();

        let range = json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 2, "character": 1 } });
        let params = json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": [], "only": ["quickfix"] } });
        let (found, _) = editor.request(2, "textDocument/codeAction", params);
        let titles: Vec<&str> = found["result"].as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Rename to `version`", "Add `version = \"0.1.0\"`"]);

        let change = json!({ "text": MANIFEST });
        editor.send(jsonrpc::notification(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [change] }),
        ));
        assert_eq!(editor.recv()["params"]["diagnostics"], json!([]));
        // The document is open, so the edit goes through the editor and the file on disk stays.
        let args = json!([{ "uri": uri, "target": "native" }]);
        editor.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "workspace/executeCommand", "params": { "command": actions::ADD_TARGET, "arguments": args } }));
        let apply = editor.recv();
        assert_eq!(apply["method"], "workspace/applyEdit");
        let edit = &apply["params"]["edit"]["changes"][&uri][0];
        assert!(edit["newText"].as_str().unwrap().contains("[target.native]"), "{edit}");
        editor.send(jsonrpc::response(&apply["id"], json!({ "applied": true })));
        let (reply, _) = editor.response(3);
        assert_eq!(reply["result"]["targets"], json!(["native"]));
        assert!(dir.path().join("src-native").is_dir());
        assert_eq!(std::fs::read_to_string(dir.path().join("kargo.toml")).unwrap(), MANIFEST);
        assert!(editor.finish());
    }

    /// A fake Kotlin server: answers `initialize`, `textDocument/hover`, `textDocument/completion`
    /// (one overload, its signature only in `labelDetails`) and `shutdown`, and reports every
    /// method it receives.
    fn fake_backend() -> (BackendIo, mpsc::Receiver<Value>) {
        let (from_kargo, to_backend) = io::pipe().unwrap();
        let (from_backend, to_kargo) = io::pipe().unwrap();
        let (seen_tx, seen) = mpsc::channel();
        thread::spawn(move || {
            let mut input = BufReader::new(from_kargo);
            let mut output = to_kargo;
            while let Ok(Some(message)) = jsonrpc::read(&mut input) {
                let reply = match jsonrpc::method(&message) {
                    "initialize" => Some(json!({ "capabilities": { "textDocumentSync": 2, "definitionProvider": true } })),
                    "textDocument/hover" => Some(json!({ "contents": "kotlin" })),
                    "textDocument/completion" => Some(json!({
                        "isIncomplete": false,
                        "items": [{ "label": "print", "labelDetails": { "description": "Unit", "detail": "(message: Int) (kotlin.io)" } }],
                    })),
                    "shutdown" => Some(Value::Null),
                    _ => None,
                };
                if let Some(result) = reply {
                    jsonrpc::write(&mut output, &jsonrpc::response(&message["id"], result)).unwrap();
                }
                let exit = jsonrpc::method(&message) == "exit";
                let _ = seen_tx.send(message);
                if exit {
                    break;
                }
            }
        });
        ((Box::new(BufReader::new(from_backend)), Box::new(to_backend)), seen)
    }

    #[test]
    fn prefers_workspace_json_but_keeps_the_editors_choice() {
        let mut params = json!({
            "rootUri": "file:///p",
            "workspaceFolders": [{ "uri": "file:///p", "name": "p" }, { "uri": "file:///q", "name": "q" }],
            "initializationOptions": { "checkOnSave": true, "buildTools": { "file:///q": "gradle" } },
        });
        prefer_workspace_json(params.as_object_mut().unwrap());
        assert_eq!(params["initializationOptions"]["buildTools"], json!({ "file:///p": "json", "file:///q": "gradle" }));
        assert_eq!(params["initializationOptions"]["checkOnSave"], true);

        let mut bare = json!({ "rootUri": "file:///p", "initializationOptions": null });
        prefer_workspace_json(bare.as_object_mut().unwrap());
        assert_eq!(bare["initializationOptions"], json!({ "buildTools": { "file:///p": "json" } }));
    }

    #[test]
    fn shows_completion_signatures_to_editors_that_do_not_show_label_details() {
        let dir = project(MANIFEST);
        let root = text::path_to_uri(dir.path());
        let uri = text::path_to_uri(&dir.path().join("src/Main.kt"));
        let completion = json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 0 } });
        let shows_label_details = json!({ "textDocument": { "completion": { "completionItem": { "labelDetailsSupport": true } } } });
        for (capabilities, expected) in [(json!({}), Some("(message: Int) (kotlin.io)")), (shows_label_details, None)] {
            let (backend, _seen) = fake_backend();
            let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
            let (mut editor, _home) = Editor::start(opts, Some(backend));
            editor.request(1, "initialize", json!({ "rootUri": root, "capabilities": capabilities }));
            let (reply, _) = editor.request(2, "textDocument/completion", completion.clone());
            let item = &reply["result"]["items"][0];
            assert_eq!(item["detail"].as_str(), expected, "{capabilities}");
            assert_eq!(item["labelDetails"]["detail"], "(message: Int) (kotlin.io)", "the item is passed through");
            assert!(editor.finish());
        }
    }

    #[test]
    fn passes_kotlin_traffic_through_and_keeps_kargo_toml() {
        let dir = project(MANIFEST);
        let root = text::path_to_uri(dir.path());
        let manifest_uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let kotlin_uri = text::path_to_uri(&dir.path().join("src/Main.kt"));
        let (backend, seen) = fake_backend();
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start(opts, Some(backend));

        // Opened below the project root, as editors do for a workspace member.
        let src = text::path_to_uri(&dir.path().join("src"));
        let (init, before) = editor.request(1, "initialize", json!({ "rootUri": src, "capabilities": {}, "workDoneToken": "t" }));
        let caps = &init["result"]["capabilities"];
        assert_eq!(caps["definitionProvider"], true, "the backend's capabilities");
        assert_eq!(caps["hoverProvider"], true, "and Kargo's");
        assert_eq!(caps["textDocumentSync"]["save"], json!({ "includeText": false }));
        assert!(before.iter().any(|m| m["method"] == "$/progress"), "the import is reported: {before:?}");
        // Offline, the dependencies can't be resolved: the project is described without them.
        let workspace: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("workspace.json")).unwrap()).unwrap();
        assert_eq!(workspace["modules"][1]["name"], "app.main");

        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": manifest_uri, "languageId": "toml", "version": 1, "text": MANIFEST } }),
        ));
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": kotlin_uri, "languageId": "kotlin", "version": 1, "text": "" } }),
        ));
        assert_eq!(editor.recv()["params"]["diagnostics"], json!([]));
        let (hover, _) = editor.request(
            2,
            "textDocument/hover",
            json!({ "textDocument": { "uri": kotlin_uri }, "position": { "line": 0, "character": 0 } }),
        );
        assert_eq!(hover["result"]["contents"], "kotlin");

        assert!(editor.finish());
        let mut received: Vec<Value> = Vec::new();
        while received.last().is_none_or(|m| jsonrpc::method(m) != "exit") {
            received.push(seen.recv_timeout(Duration::from_secs(10)).unwrap());
        }
        assert!(received[0]["params"].get("workDoneToken").is_none(), "Kargo already ended the editor's progress");
        assert_eq!(received[0]["params"]["rootUri"], root, "the server opens the project root");
        assert_eq!(received[0]["params"]["initializationOptions"]["buildTools"][&root], "json", "workspace.json beats an IDE's .idea");
        let methods: Vec<&str> = received.iter().map(jsonrpc::method).collect();
        assert_eq!(
            methods,
            ["initialize", "textDocument/didOpen", "textDocument/hover", "shutdown", "exit"],
            "kargo.toml stays with Kargo"
        );
    }

    #[test]
    fn tells_the_backend_about_a_saved_kargo_toml_when_the_editor_does_not_watch_files() {
        let dir = project(MANIFEST);
        let manifest_uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let (backend, seen) = fake_backend();
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start(opts, Some(backend));
        editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} }));
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": manifest_uri, "languageId": "toml", "version": 1, "text": MANIFEST } }),
        ));
        editor.send(jsonrpc::notification("textDocument/didSave", json!({ "textDocument": { "uri": manifest_uri } })));
        assert!(editor.finish());
        let mut received: Vec<Value> = Vec::new();
        while received.last().is_none_or(|m| jsonrpc::method(m) != "exit") {
            received.push(seen.recv_timeout(Duration::from_secs(10)).unwrap());
        }
        let changed = received.iter().find(|m| jsonrpc::method(m) == "workspace/didChangeWatchedFiles").expect("a change notice");
        assert_eq!(changed["params"]["changes"], json!([{ "uri": manifest_uri, "type": 2 }]));
    }

    #[test]
    fn answers_for_a_backend_that_exits() {
        let dir = project(MANIFEST);
        let (from_kargo, to_backend) = io::pipe().unwrap();
        let (from_backend, to_kargo) = io::pipe().unwrap();
        // The backend exits on `initialize` without answering.
        thread::spawn(move || {
            let mut input = BufReader::new(from_kargo);
            let _ = jsonrpc::read(&mut input);
            drop(to_kargo);
        });
        let backend: BackendIo = (Box::new(BufReader::new(from_backend)), Box::new(to_backend));
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start(opts, Some(backend));
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} }));
        assert_eq!(init["result"]["capabilities"], own_capabilities());
        let warning = editor.recv();
        assert_eq!(warning["method"], "window/showMessage");
        assert!(editor.finish());
    }

    /// A fake Kotlin server that reports `Unresolved reference 'x'.` on the second line of every
    /// document it opens: pushed, or with `pull`, in `textDocument/diagnostic` reports (full the
    /// first time, unchanged after).
    fn diagnosing_backend(pull: bool) -> BackendIo {
        let (from_kargo, to_backend) = io::pipe().unwrap();
        let (from_backend, to_kargo) = io::pipe().unwrap();
        thread::spawn(move || {
            let mut input = BufReader::new(from_kargo);
            let mut output = to_kargo;
            let reported = json!([{
                "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 5 } },
                "severity": 1, "source": "kotlin", "message": "Unresolved reference 'x'.",
            }]);
            while let Ok(Some(message)) = jsonrpc::read(&mut input) {
                let params = &message["params"];
                let reply = match jsonrpc::method(&message) {
                    "initialize" if pull => Some(
                        json!({ "capabilities": { "textDocumentSync": 2, "diagnosticProvider": { "interFileDependencies": true, "workspaceDiagnostics": false } } }),
                    ),
                    "initialize" => Some(json!({ "capabilities": { "textDocumentSync": 2 } })),
                    "textDocument/diagnostic" if params["previousResultId"].is_string() => {
                        Some(json!({ "kind": "unchanged", "resultId": "1" }))
                    }
                    "textDocument/diagnostic" => Some(json!({ "kind": "full", "resultId": "1", "items": reported })),
                    "shutdown" => Some(Value::Null),
                    "textDocument/didOpen" if !pull => {
                        let params = json!({ "uri": params["textDocument"]["uri"], "diagnostics": reported });
                        jsonrpc::write(&mut output, &jsonrpc::notification("textDocument/publishDiagnostics", params)).unwrap();
                        None
                    }
                    "exit" => break,
                    _ => None,
                };
                if let Some(result) = reply {
                    jsonrpc::write(&mut output, &jsonrpc::response(&message["id"], result)).unwrap();
                }
            }
        });
        (Box::new(BufReader::new(from_backend)), Box::new(to_backend))
    }

    /// A check runner that answers with the next of `outputs` once the test sends on the returned
    /// channel; a cancelled check answers at once with `stale` diagnostics the server must drop.
    fn gated_runner(outputs: Vec<Vec<Value>>, stale: Value) -> (check::Runner, mpsc::Sender<()>, mpsc::Receiver<check::Run>) {
        let (gate_tx, gate) = mpsc::channel::<()>();
        let (runs_tx, runs) = mpsc::channel();
        let state = std::sync::Mutex::new((gate, outputs.into_iter(), runs_tx));
        let runner: check::Runner = Arc::new(move |run: &check::Run, cancel: &AtomicBool| {
            let state = state.lock().unwrap();
            state.2.send(run.clone()).unwrap();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    let finished = json!({ "reason": "build-finished", "success": false });
                    return Ok(check::Output { messages: vec![stale.clone(), finished], stderr: String::new() });
                }
                if state.0.recv_timeout(Duration::from_millis(20)).is_ok() {
                    break;
                }
            }
            let mut state = state;
            Ok(check::Output { messages: state.1.next().unwrap_or_default(), stderr: String::new() })
        });
        (runner, gate_tx, runs)
    }

    fn compiler_message(file: &Path, line: u32, target: &str, message: &str) -> Value {
        json!({
            "reason": "compiler-message", "severity": "error", "message": message,
            "file": file.display().to_string(), "line": line, "column": 5, "target": target,
        })
    }

    /// Reads until `publishDiagnostics` for `count` documents, answering the editor's side of
    /// requests. Returns the last diagnostics of each document by URI.
    fn publishes(editor: &mut Editor, count: usize) -> HashMap<String, Vec<Value>> {
        let mut found = HashMap::new();
        while found.len() < count {
            let message = editor.recv();
            assert_ne!(message["params"]["diagnostics"][0]["message"], "stale", "a cancelled check is dropped");
            if jsonrpc::kind(&message) == Kind::Request {
                editor.send(jsonrpc::response(&message["id"], Value::Null));
            }
            if message["method"] == "textDocument/publishDiagnostics" {
                let uri = message["params"]["uri"].as_str().unwrap().to_owned();
                found.insert(uri, message["params"]["diagnostics"].as_array().cloned().unwrap());
            }
        }
        found
    }

    /// Reads until a `publishDiagnostics` for `uri`.
    fn diagnostics_for(editor: &mut Editor, uri: &str) -> Vec<Value> {
        let found = publishes(editor, 1);
        assert!(found.contains_key(uri), "{found:?}");
        found[uri].clone()
    }

    fn open(editor: &mut Editor, uri: &str, text: &str) {
        editor.send(jsonrpc::notification(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "kotlin", "version": 1, "text": text } }),
        ));
    }

    fn save(editor: &mut Editor, uri: &str) {
        editor.send(jsonrpc::notification("textDocument/didSave", json!({ "textDocument": { "uri": uri } })));
    }

    const SOURCE: &str = "fun main() {\n    x()\n    val s: String = 1\n}\n";

    fn multiplatform_project() -> tempfile::TempDir {
        let dir = project("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.jvm]\n[target.wasm]\n");
        std::fs::write(dir.path().join("src/Main.kt"), SOURCE).unwrap();
        std::fs::create_dir_all(dir.path().join("src-wasm")).unwrap();
        std::fs::write(dir.path().join("src-wasm/Web.kt"), SOURCE).unwrap();
        dir
    }

    #[test]
    fn merges_check_diagnostics_into_pushed_ones() {
        let dir = multiplatform_project();
        let (common, web) = (dir.path().join("src/Main.kt"), dir.path().join("src-wasm/Web.kt"));
        let (common_uri, web_uri) = (text::path_to_uri(&common), text::path_to_uri(&web));
        let failing = vec![
            // The Kotlin server reports this one already.
            compiler_message(&common, 2, "jvm", "Unresolved reference 'x'."),
            compiler_message(&common, 3, "wasm", "initializer type mismatch"),
            compiler_message(&web, 2, "wasm", "unresolved reference 'x'."),
            json!({ "reason": "build-finished", "success": false }),
        ];
        let passing = vec![json!({ "reason": "build-finished", "success": true })];
        let stale = compiler_message(&common, 1, "wasm", "stale");
        let (runner, gate, runs) = gated_runner(vec![failing, passing], stale);
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start_with(opts, Some(diagnosing_backend(false)), runner);
        let params = json!({
            "rootUri": text::path_to_uri(dir.path()),
            "capabilities": { "window": { "workDoneProgress": true } },
            "initializationOptions": { "checkOnSave": true, "checkTargets": ["wasm"] },
        });
        editor.request(1, "initialize", params);
        open(&mut editor, &common_uri, SOURCE);
        assert_eq!(diagnostics_for(&mut editor, &common_uri).len(), 1, "the Kotlin server's own");

        // A second save cancels the first check; its results never show.
        save(&mut editor, &common_uri);
        let first = runs.recv_timeout(Duration::from_secs(10)).unwrap();
        save(&mut editor, &common_uri);
        let second = runs.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(first, second);
        assert_eq!(second.manifest, dir.path().join("kargo.toml"));
        assert_eq!(second.targets, ["wasm"]);
        gate.send(()).unwrap();

        let found = publishes(&mut editor, 2);
        let web_diagnostics = &found[&web_uri];
        assert_eq!(web_diagnostics.len(), 1);
        assert_eq!(web_diagnostics[0]["source"], "kargo");
        assert_eq!(web_diagnostics[0]["code"], "wasm");
        assert_eq!(web_diagnostics[0]["range"]["start"], json!({ "line": 1, "character": 4 }));
        let messages: Vec<&str> = found[&common_uri].iter().map(|d| d["message"].as_str().unwrap()).collect();
        assert_eq!(messages, ["Unresolved reference 'x'.", "initializer type mismatch"], "not duplicated");

        // The next check passes: Kargo's diagnostics go, the Kotlin server's stay.
        save(&mut editor, &common_uri);
        runs.recv_timeout(Duration::from_secs(10)).unwrap();
        gate.send(()).unwrap();
        let cleared = publishes(&mut editor, 2);
        assert!(cleared[&web_uri].is_empty());
        assert_eq!(cleared[&common_uri].len(), 1);
        assert!(editor.finish());
    }

    #[test]
    fn reports_status_and_logs_request_timings() {
        let dir = multiplatform_project();
        let common = dir.path().join("src/Main.kt");
        let common_uri = text::path_to_uri(&common);
        let failing = vec![compiler_message(&common, 3, "jvm", "type mismatch"), json!({ "reason": "build-finished", "success": false })];
        let (runner, gate, runs) = gated_runner(vec![failing], Value::Null);
        let log = dir.path().join("lsp.log");
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), log_file: Some(log.clone()), ..Options::default() };
        let (mut editor, _home) = Editor::start_with(opts, Some(diagnosing_backend(false)), runner);
        let params = json!({
            "rootUri": text::path_to_uri(dir.path()),
            "capabilities": {},
            "initializationOptions": { "checkOnSave": true, "status": true },
        });
        editor.request(1, "initialize", params);
        open(&mut editor, &common_uri, SOURCE);
        save(&mut editor, &common_uri);
        runs.recv_timeout(Duration::from_secs(10)).unwrap();
        let status = |editor: &mut Editor| loop {
            let message = editor.recv();
            if message["method"] == "kargo/status" {
                return message["params"].clone();
            }
        };
        assert_eq!(status(&mut editor), json!({ "task": "checking", "active": true }));
        gate.send(()).unwrap();
        assert_eq!(status(&mut editor), json!({ "task": "checking", "active": false, "message": "1 error" }));
        assert!(editor.finish());
        let log = std::fs::read_to_string(&log).unwrap();
        assert!(log.lines().any(|l| l.contains("] initialize 1 answered in ") && l.ends_with(" ms")), "{log}");
        assert!(log.lines().any(|l| l.ends_with("] textDocument/didSave")), "{log}");
    }

    #[test]
    fn merges_check_diagnostics_into_pulled_reports() {
        let dir = multiplatform_project();
        let (common, web) = (dir.path().join("src/Main.kt"), dir.path().join("src-wasm/Web.kt"));
        let (common_uri, web_uri) = (text::path_to_uri(&common), text::path_to_uri(&web));
        let failing = vec![
            compiler_message(&common, 2, "jvm", "Unresolved reference 'x'."),
            compiler_message(&common, 3, "wasm", "initializer type mismatch"),
            compiler_message(&web, 3, "wasm", "initializer type mismatch"),
            json!({ "reason": "build-finished", "success": false }),
        ];
        let (runner, gate, runs) = gated_runner(vec![failing], Value::Null);
        let opts = Options { server: KotlinServer::External(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start_with(opts, Some(diagnosing_backend(true)), runner);
        let capabilities = json!({ "workspace": { "diagnostics": { "refreshSupport": true } } });
        let params = json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": capabilities, "initializationOptions": { "checkOnSave": true } });
        editor.request(1, "initialize", params);
        open(&mut editor, &common_uri, SOURCE);
        let pull = |editor: &mut Editor, id: i64, previous: Option<&str>| {
            let mut params = json!({ "textDocument": { "uri": common_uri } });
            if let Some(previous) = previous {
                params["previousResultId"] = json!(previous);
            }
            editor.request(id, "textDocument/diagnostic", params).0["result"].clone()
        };
        assert_eq!(pull(&mut editor, 2, None)["items"].as_array().unwrap().len(), 1);

        save(&mut editor, &common_uri);
        assert!(runs.recv_timeout(Duration::from_secs(10)).unwrap().targets.is_empty(), "every enabled target");
        gate.send(()).unwrap();
        // The closed file's diagnostics are pushed; the open one's are pulled again.
        let web_diagnostics = diagnostics_for(&mut editor, &web_uri);
        assert_eq!(web_diagnostics[0]["message"], "initializer type mismatch");
        let refresh = editor.recv();
        assert_eq!(refresh["method"], "workspace/diagnostic/refresh");
        editor.send(jsonrpc::response(&refresh["id"], Value::Null));
        let report = pull(&mut editor, 3, Some("1"));
        assert_eq!(report["kind"], "full", "an unchanged report gains the check's diagnostics");
        let messages: Vec<&str> = report["items"].as_array().unwrap().iter().map(|d| d["message"].as_str().unwrap()).collect();
        assert_eq!(messages, ["Unresolved reference 'x'.", "initializer type mismatch"]);

        // Once open, the pushed diagnostics give way to the pulled ones.
        open(&mut editor, &web_uri, SOURCE);
        assert_eq!(diagnostics_for(&mut editor, &web_uri), Vec::<Value>::new());
        assert!(editor.finish());
    }
}
