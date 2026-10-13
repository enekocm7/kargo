//! The `kargo lsp` message loop.
//!
//! Kargo sits between the editor and a Kotlin language server (the backend). Everything about
//! `kargo.toml` (diagnostics, completion, hover) and the `kargo/runnables` request is answered
//! here; every other message is passed through unchanged in both directions. The
//! `initialize` response is the backend's with Kargo's capabilities merged in.
//!
//! Before the backend starts, Kargo imports the project and writes the description the
//! backend reads (see [`crate::project`]). When `kargo.toml` or `kargo.lock` change and the
//! project's fingerprint with them, it imports the project again in the background, reports
//! it with `$/progress`, and tells the backend which generated files changed.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use kargo_toolchain::language_server::{KLS_MAIN_CLASS, KLS_MAX_JDK};
use kargo_toolchain::{LanguageServer, ServerKind};
use kargo_util::config::Config;

use crate::jsonrpc::{self, Kind};
use crate::{manifest, project, text};

/// How `kargo lsp` runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The Kotlin language server behind Kargo's; `None` serves only `kargo.toml` and runnables.
    pub server: Option<ServerKind>,
    /// Runs this executable as the server instead of the one Kargo manages.
    pub server_path: Option<PathBuf>,
    /// Kargo's built-in commands, for the warning about tasks they hide.
    pub builtin_commands: Vec<String>,
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
}

/// Runs the server until the editor sends `exit` or closes the stream. Returns whether the
/// editor shut it down properly (`shutdown` before `exit`).
pub fn serve(config: Config, opts: Options, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> Result<bool> {
    serve_with(config, opts, input, output, None)
}

pub(crate) fn serve_with(
    config: Config,
    opts: Options,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    backend: Option<BackendIo>,
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
    let mut server = Server::new(config, opts, Box::new(output), tx);
    if let Some((reader, writer)) = backend {
        server.attach(reader, writer, None);
    }
    for event in rx {
        if let Some(clean) = server.handle(event)? {
            server.stop_backend();
            return Ok(clean);
        }
    }
    server.stop_backend();
    Ok(false)
}

struct Backend {
    stdin: Box<dyn Write + Send>,
    child: Option<Child>,
}

/// An import's `$/progress`, once the editor accepted its token.
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
    shutdown: bool,
    next_id: u64,
    fingerprint: Option<String>,
    importing: bool,
    reimport: bool,
    progress: Option<Progress>,
    warned_foreign: bool,
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

/// The capabilities Kargo's own server has.
fn own_capabilities() -> Value {
    json!({
        "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
        "completionProvider": { "triggerCharacters": ["[", ".", "\"", "=", " ", ","] },
        "hoverProvider": true,
        "experimental": { "kargo": { "runnables": true } },
    })
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
            let mut triggers: Vec<Value> = completion.get("triggerCharacters").and_then(Value::as_array).cloned().unwrap_or_default();
            for t in ours["completionProvider"]["triggerCharacters"].as_array().into_iter().flatten() {
                if !triggers.contains(t) {
                    triggers.push(t.clone());
                }
            }
            completion.insert("triggerCharacters".into(), json!(triggers));
        }
        None => caps["completionProvider"] = ours["completionProvider"].clone(),
    }
    if caps["hoverProvider"].is_null() || caps["hoverProvider"] == false {
        caps["hoverProvider"] = json!(true);
    }
    if !caps["experimental"].is_object() {
        caps["experimental"] = json!({});
    }
    caps["experimental"]["kargo"] = ours["experimental"]["kargo"].clone();
    caps
}

impl Server {
    fn new(config: Config, opts: Options, out: Box<dyn Write + Send>, tx: Sender<Event>) -> Self {
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
            shutdown: false,
            next_id: 0,
            fingerprint: None,
            importing: false,
            reimport: false,
            progress: None,
            warned_foreign: false,
        }
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        jsonrpc::write(&mut self.out, message).context("failed to write to the editor")
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
        }
    }

    fn on_client(&mut self, message: Value) -> Result<Option<bool>> {
        let method = jsonrpc::method(&message).to_owned();
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
                    "kargo/runnables" => {
                        let reply = match self.runnables(&message) {
                            Ok(list) => jsonrpc::response(&id, json!(list)),
                            Err(e) => jsonrpc::error(&id, -32603, format!("{e:#}")),
                        };
                        self.send(&reply)?;
                    }
                    _ if document_uri(&message).is_some_and(is_kargo_document) => {
                        let result = self.manifest_request(&method, &message);
                        self.send(&jsonrpc::response(&id, result))?;
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

    fn on_backend(&mut self, message: Value) -> Result<()> {
        if jsonrpc::kind(&message) == Kind::Response {
            self.pending.remove(&id_key(&message["id"]));
            if self.initialize_id.as_ref() == Some(&message["id"]) {
                self.initialize_id = None;
                let mut message = message;
                if message["result"].is_object() {
                    message["result"]["capabilities"] = merge_capabilities(&message["result"]["capabilities"]);
                }
                return self.send(&message);
            }
        }
        self.send(&message)
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
        let dir = params["rootUri"]
            .as_str()
            .and_then(text::uri_to_path)
            .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
            .or_else(|| params["workspaceFolders"][0]["uri"].as_str().and_then(text::uri_to_path))
            .or_else(|| std::env::current_dir().ok());
        self.root = dir.as_deref().and_then(project::project_root);

        // Import before the backend starts, so it finds the project description right away.
        let kind = self.opts.server;
        let mut metadata: Option<Value> = None;
        let mut imported = false;
        if let Some(root) = self.root.clone() {
            self.fingerprint = project::fingerprint(&root).ok();
            let fresh = kind == Some(ServerKind::KotlinLsp) && project::is_fresh(&root, ServerKind::KotlinLsp);
            if kind.is_some() && !fresh {
                imported = true;
                let token = params.get("workDoneToken").filter(|t| !t.is_null()).cloned();
                if let Some(token) = &token {
                    self.notify("$/progress", json!({ "token": token, "value": { "kind": "begin", "title": "Importing Kargo project" } }))?;
                }
                let outcome = project::import(&self.config, &root, kind);
                if let Some(token) = &token {
                    self.notify("$/progress", json!({ "token": token, "value": { "kind": "end" } }))?;
                }
                self.report_import(&outcome, true)?;
                metadata = outcome.metadata;
            }
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
                if let Some(root) = &self.root
                    && dir.as_deref() != Some(root.as_path())
                {
                    let uri = text::path_to_uri(root);
                    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    params.insert("rootUri".into(), json!(uri));
                    params.insert("rootPath".into(), json!(root.display().to_string()));
                    params.insert("workspaceFolders".into(), json!([{ "uri": uri, "name": name }]));
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
        if self.client_progress {
            let create_id = self.next_id();
            let token = format!("{create_id}/import");
            self.send(&jsonrpc::request(&create_id, "window/workDoneProgress/create", json!({ "token": token })))?;
            self.progress = Some(Progress { token, create_id, begun: false });
        }
        let (config, tx, kind) = (self.config.clone(), self.tx.clone(), self.opts.server);
        thread::spawn(move || {
            let outcome = project::import(&config, &root, kind);
            let _ = tx.send(Event::Imported(outcome));
        });
        Ok(())
    }

    fn imported(&mut self, outcome: project::Outcome) -> Result<()> {
        self.importing = false;
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

    fn manifest_request(&mut self, method: &str, message: &Value) -> Value {
        let uri = document_uri(message).unwrap_or_default();
        let Some(text) = self.docs.get(uri).filter(|_| is_manifest(uri)) else { return Value::Null };
        let offset = text::offset(text, &message["params"]["position"]);
        match method {
            "textDocument/completion" => json!({ "isIncomplete": false, "items": manifest::completion(text, offset) }),
            "textDocument/hover" => manifest::hover(text, offset).unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn manifest_notification(&mut self, method: &str, message: &Value) -> Result<()> {
        let uri = document_uri(message).unwrap_or_default().to_owned();
        let params = &message["params"];
        match method {
            "textDocument/didOpen" if is_manifest(&uri) => {
                self.docs.insert(uri.clone(), params["textDocument"]["text"].as_str().unwrap_or_default().to_owned());
                self.publish(&uri)?;
            }
            "textDocument/didChange" if is_manifest(&uri) => {
                if let Some(text) = self.docs.get_mut(&uri) {
                    text::apply_changes(text, params["contentChanges"].as_array().map(Vec::as_slice).unwrap_or_default());
                    self.publish(&uri)?;
                }
            }
            "textDocument/didClose" if is_manifest(&uri) => {
                self.docs.remove(&uri);
                self.notify("textDocument/publishDiagnostics", json!({ "uri": uri, "diagnostics": [] }))?;
            }
            "textDocument/didSave" => self.maybe_reimport()?,
            _ => {}
        }
        Ok(())
    }

    fn publish(&mut self, uri: &str) -> Result<()> {
        let Some(text) = self.docs.get(uri) else { return Ok(()) };
        let path = text::uri_to_path(uri).unwrap_or_else(|| PathBuf::from(kargo_manifest::MANIFEST_NAME));
        let builtins = &self.opts.builtin_commands;
        let problems = manifest::problems(text, Path::new(&path), &|name| builtins.iter().any(|b| b == name));
        let diagnostics: Vec<Value> = problems
            .into_iter()
            .map(|p| json!({ "range": text::range(text, p.span), "severity": if p.warning { 2 } else { 1 }, "source": "kargo", "message": p.message }))
            .collect();
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

    /// An editor talking to `serve_with` over pipes.
    struct Editor {
        input: PipeWriter,
        output: BufReader<PipeReader>,
        server: Option<thread::JoinHandle<Result<bool>>>,
    }

    impl Editor {
        fn start(opts: Options, backend: Option<BackendIo>) -> (Self, tempfile::TempDir) {
            let home = tempfile::tempdir().unwrap();
            let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
            let (server_in, input) = io::pipe().unwrap();
            let (output, server_out) = io::pipe().unwrap();
            let server = thread::spawn(move || serve_with(config, opts, BufReader::new(server_in), server_out, backend));
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

    /// A fake Kotlin server: answers `initialize`, `textDocument/hover` and `shutdown`, and
    /// reports every method it receives.
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
    fn passes_kotlin_traffic_through_and_keeps_kargo_toml() {
        let dir = project(MANIFEST);
        let root = text::path_to_uri(dir.path());
        let manifest_uri = text::path_to_uri(&dir.path().join("kargo.toml"));
        let kotlin_uri = text::path_to_uri(&dir.path().join("src/Main.kt"));
        let (backend, seen) = fake_backend();
        let opts = Options { server: Some(ServerKind::KotlinLsp), ..Options::default() };
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
        let methods: Vec<&str> = received.iter().map(jsonrpc::method).collect();
        assert_eq!(
            methods,
            ["initialize", "textDocument/didOpen", "textDocument/hover", "shutdown", "exit"],
            "kargo.toml stays with Kargo"
        );
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
        let opts = Options { server: Some(ServerKind::KotlinLsp), ..Options::default() };
        let (mut editor, _home) = Editor::start(opts, Some(backend));
        let (init, _) = editor.request(1, "initialize", json!({ "rootUri": text::path_to_uri(dir.path()), "capabilities": {} }));
        assert_eq!(init["result"]["capabilities"], own_capabilities());
        let warning = editor.recv();
        assert_eq!(warning["method"], "window/showMessage");
        assert!(editor.finish());
    }
}
