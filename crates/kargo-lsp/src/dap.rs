//! `kargo dap`: a debug adapter for Kargo projects, over stdio.
//!
//! Kargo runs [fwcd's kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter) (KDA)
//! on the project's JDK and passes Debug Adapter Protocol traffic through, with three changes:
//!
//! - **Launching**: a `launch` request carries a Kargo command line in `args` (such as
//!   `["run", "--bin", "server"]` or `["test", "com.example.MathTest"]`). Kargo runs it with
//!   `--debug=0`, relays what the program prints as `output` events, and once the JVM listens
//!   for a debugger, asks KDA to attach to it. The editor sees an ordinary `launch`.
//! - **Attaching**: an `attach` request gets the arguments KDA requires (`projectRoot`,
//!   `hostName`, `timeout`), so `{ "port": 5005 }` is enough.
//! - **Source paths**: KDA expects Gradle's layout. It turns a breakpoint's file into class
//!   names from its path below `kotlin/` or `java/` (`.../kotlin/com/example/Main.kt` becomes
//!   `com.example.Main` and `com.example.MainKt`), and finds the file of a stack frame below
//!   `src/<set>/<language>` of the project root. Kargo's sources live anywhere in `src/`, so
//!   Kargo mirrors them: every source file gets an empty placeholder at
//!   `target/dap/src/main/kotlin/<package>/<file>`, the project root KDA is given is
//!   `target/dap`, and paths are rewritten between the two in every message. A breakpoint
//!   request is sent once for each class the file declares at the top level (and its
//!   `@file:JvmName`), and the answers are merged, so breakpoints in any of them resolve.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use kargo_ops::ops::context::BuildContext;
use kargo_ops::ops::metadata::{self, MetadataOptions};
use kargo_ops::workspace::{ProfileKind, Project};
use kargo_toolchain::language_server::KDA_MAIN_CLASS;
use kargo_toolchain::{LanguageServer, ServerKind};
use kargo_util::config::Config;

use crate::jsonrpc;

/// How `kargo dap` runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Runs this executable as the debug adapter instead of the kotlin-debug-adapter Kargo manages.
    pub adapter_path: Option<PathBuf>,
}

/// How long KDA tries to connect to the JVM, in milliseconds.
const ATTACH_TIMEOUT_MS: u64 = 10_000;

/// What the JVM prints once it waits for a debugger.
const LISTENING: &str = "Listening for transport dt_socket at address:";

/// The first sequence number of the requests Kargo sends KDA itself. KDA reads sequence
/// numbers as 32-bit integers, and editors count up from 1.
const FIRST_OWN_SEQ: i64 = 1_000_000_000;

/// Runs the debug adapter for the project in the current directory over stdin and stdout.
/// Returns whether the session ended cleanly.
pub fn run(config: Config, opts: Options) -> Result<bool> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let project = Project::discover(&cwd, None)?;
    // The context holds the lock on `target/`, which the `kargo run` of a launch needs: let it
    // go before the session starts.
    let (metadata, java_home) = {
        let ctx = BuildContext::new(&config, &project, project.members()[0], ProfileKind::Dev)?;
        let metadata = metadata::metadata(&ctx, &MetadataOptions { no_deps: true })?;
        let java_home = match &opts.adapter_path {
            Some(_) => None,
            None => Some(ctx.jdk().context("`kargo dap` runs the debugger on the project's JDK")?.home.clone()),
        };
        (metadata, java_home)
    };
    let mirror = Mirror::build(&metadata, &project.target_dir().join("dap"))?;

    let mut cmd = match (&opts.adapter_path, &java_home) {
        (Some(path), _) => Command::new(path),
        (None, java_home) => {
            let java_home = java_home.as_deref().context("no JDK")?;
            let adapter = LanguageServer::ensure(&config, ServerKind::Kda, ServerKind::Kda.default_version())?;
            // The launcher lists every jar on one command line, which is too long for cmd.exe.
            let launcher = adapter.executable().context("kotlin-debug-adapter has no launcher")?;
            let lib = launcher.parent().and_then(Path::parent).unwrap_or(&adapter.home).join("lib");
            let mut cmd = Command::new(java_home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }));
            cmd.arg("-cp").arg(lib.join("*")).arg(KDA_MAIN_CLASS);
            cmd.env("JAVA_HOME", java_home);
            cmd
        }
    };
    cmd.current_dir(project.root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    config.shell().verbose("Running", kargo_util::process::describe(&cmd));
    let mut adapter = cmd.spawn().context("failed to start the debug adapter")?;
    let to_adapter = adapter.stdin.take().context("no stdin")?;
    let from_adapter = BufReader::new(adapter.stdout.take().context("no stdout")?);

    let kargo = std::env::current_exe().context("failed to find the kargo executable")?;
    let proxy = Proxy::new(mirror, project.root().to_path_buf());
    let clean = serve(proxy, &kargo, BufReader::new(io::stdin()), io::stdout(), from_adapter, to_adapter);
    let _ = adapter.kill();
    let _ = adapter.wait();
    clean
}

enum Event {
    /// A message from the editor; `None` when it closed the stream.
    Client(Option<Value>),
    /// A message from the debug adapter; `None` when it exited.
    Adapter(Option<Value>),
    /// What the launched program printed, by DAP output category.
    Output(&'static str, String),
    /// The launched JVM listens for a debugger on this port.
    Listening(u16),
    /// The launched `kargo` exited, with its exit code.
    Exited(Option<i32>),
}

fn serve(
    mut proxy: Proxy,
    kargo: &Path,
    client_in: impl BufRead + Send + 'static,
    mut client_out: impl Write,
    adapter_in: impl BufRead + Send + 'static,
    mut adapter_out: impl Write,
) -> Result<bool> {
    let (tx, rx) = mpsc::channel();
    read_messages(client_in, tx.clone(), Event::Client);
    read_messages(adapter_in, tx.clone(), Event::Adapter);
    let mut program: Option<Arc<Mutex<Child>>> = None;
    let result = session(&mut proxy, kargo, &rx, &tx, &mut program, &mut client_out, &mut adapter_out);
    if let Some(child) = program.take() {
        stop(&child);
    }
    result
}

fn session(
    proxy: &mut Proxy,
    kargo: &Path,
    rx: &mpsc::Receiver<Event>,
    tx: &Sender<Event>,
    program: &mut Option<Arc<Mutex<Child>>>,
    mut client_out: impl Write,
    mut adapter_out: impl Write,
) -> Result<bool> {
    let mut clean = true;
    while let Ok(event) = rx.recv() {
        let actions = match event {
            Event::Client(None) => break,
            Event::Adapter(None) => {
                clean = proxy.terminated;
                break;
            }
            Event::Client(Some(message)) => proxy.on_client(message),
            Event::Adapter(Some(message)) => proxy.on_adapter(message),
            Event::Output(category, text) => vec![Action::Client(event_message("output", json!({ "category": category, "output": text })))],
            Event::Listening(port) => proxy.listening(port),
            Event::Exited(code) => {
                *program = None;
                proxy.exited(code)
            }
        };
        for action in actions {
            match action {
                Action::Client(message) => jsonrpc::write(&mut client_out, &message)?,
                Action::Adapter(message) => jsonrpc::write(&mut adapter_out, &message)?,
                Action::Attach(mut message) => {
                    let arguments = &mut message["arguments"];
                    let host = arguments["hostName"].as_str().unwrap_or("127.0.0.1").to_owned();
                    if let Some(port) = arguments["port"].as_u64().and_then(|p| u16::try_from(p).ok())
                        && let Ok(local) = gate(&host, port)
                    {
                        arguments["hostName"] = json!("127.0.0.1");
                        arguments["port"] = json!(local);
                    }
                    jsonrpc::write(&mut adapter_out, &message)?
                }
                Action::Launch(launch) => match spawn_program(kargo, &launch, tx.clone()) {
                    Ok(child) => *program = Some(child),
                    Err(e) => {
                        for action in proxy.launch_failed(&format!("{e:#}")) {
                            if let Action::Client(message) = action {
                                jsonrpc::write(&mut client_out, &message)?;
                            }
                        }
                    }
                },
                Action::Stop => {
                    if let Some(child) = program.take() {
                        stop(&child);
                    }
                }
            }
        }
    }
    Ok(clean)
}

/// Reads DAP messages on a thread of their own.
fn read_messages(mut input: impl BufRead + Send + 'static, tx: Sender<Event>, wrap: fn(Option<Value>) -> Event) {
    thread::spawn(move || {
        loop {
            match jsonrpc::read(&mut input) {
                Ok(Some(message)) => {
                    if tx.send(wrap(Some(message))).is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = tx.send(wrap(None));
                    return;
                }
            }
        }
    });
}

/// Starts the `kargo` command of a launch request, relaying its output and watching for the
/// JVM's port.
fn spawn_program(kargo: &Path, launch: &Launch, tx: Sender<Event>) -> Result<Arc<Mutex<Child>>> {
    let mut cmd = Command::new(kargo);
    cmd.args(&launch.args).current_dir(&launch.cwd).envs(launch.env.iter().cloned());
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Its own process group, so stopping it stops the JVM it starts too.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to run {}", kargo.display()))?;
    relay(child.stdout.take().context("no stdout")?, "stdout", tx.clone());
    relay(child.stderr.take().context("no stderr")?, "stderr", tx.clone());
    let child = Arc::new(Mutex::new(child));
    let watched = Arc::clone(&child);
    thread::spawn(move || {
        loop {
            let status = match watched.lock() {
                Ok(mut child) => child.try_wait(),
                Err(_) => return,
            };
            match status {
                Ok(Some(status)) => {
                    let _ = tx.send(Event::Exited(status.code()));
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(_) => {
                    let _ = tx.send(Event::Exited(None));
                    return;
                }
            }
        }
    });
    Ok(child)
}

/// Relays a stream of the launched program as `output` events, and reports the port in the
/// JVM's "Listening for transport" line.
fn relay(mut stream: impl Read + Send + 'static, category: &'static str, tx: Sender<Event>) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut seen = String::new();
        let mut found = false;
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            if !found {
                seen.push_str(&text);
                if let Some(port) = listening_port(&seen) {
                    found = true;
                    seen.clear();
                    let _ = tx.send(Event::Listening(port));
                }
            }
            if tx.send(Event::Output(category, text)).is_err() {
                return;
            }
        }
    });
}

/// Stops the launched `kargo` and the JVM it started.
fn stop(child: &Mutex<Child>) {
    let Ok(mut child) = child.lock() else { return };
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let pid = child.id().to_string();
    #[cfg(unix)]
    let stopped = Command::new("kill").args(["-TERM", &format!("-{pid}")]).stderr(Stdio::null()).status();
    #[cfg(windows)]
    let stopped = Command::new("taskkill").args(["/T", "/F", "/PID", &pid]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    #[cfg(not(any(unix, windows)))]
    let stopped: io::Result<std::process::ExitStatus> = Err(io::ErrorKind::Unsupported.into());
    if !stopped.is_ok_and(|s| s.success()) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// The port in the JVM's "Listening for transport dt_socket at address: ..." line.
fn listening_port(output: &str) -> Option<u16> {
    let rest = &output[output.find(LISTENING)? + LISTENING.len()..];
    let line = rest.lines().next().filter(|_| rest.contains('\n'))?;
    let address = line.trim();
    address.rsplit(':').next()?.parse().ok()
}

/// What the message loop does for the proxy.
#[derive(Debug, PartialEq)]
enum Action {
    Client(Value),
    Adapter(Value),
    /// Send KDA an `attach` request, through a [`gate`] to the JVM it names.
    Attach(Value),
    /// Run `kargo` for a launch request.
    Launch(Launch),
    /// Stop the launched program.
    Stop,
}

/// A `kargo` command line to debug.
#[derive(Debug, Clone, PartialEq)]
struct Launch {
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

/// The launch request waiting for its JVM.
#[derive(Debug)]
struct PendingLaunch {
    seq: Value,
    /// What the editor passed, for KDA's own options (`logLevel`, ...).
    arguments: Value,
    /// The program, e.g. "`kargo run`", for messages.
    what: String,
}

/// A breakpoint request Kargo sent KDA as several, one per class.
#[derive(Debug)]
struct Fanout {
    request: Value,
    responses: Vec<Option<Value>>,
}

/// The state of a debug session between the editor and KDA.
#[derive(Debug)]
struct Proxy {
    mirror: Mirror,
    root: PathBuf,
    next_seq: i64,
    launch: Option<PendingLaunch>,
    /// The sequence number of the launch request KDA attaches for.
    attaching: Option<Value>,
    /// Breakpoint requests by the editor's sequence number.
    fanouts: HashMap<i64, Fanout>,
    /// The editor's sequence number of each request a fan-out sent, and its position.
    fanout_parts: HashMap<i64, (i64, usize)>,
    /// The mirrored files each source's breakpoints were last sent for.
    breakpoint_files: HashMap<String, Vec<String>>,
    /// Whether KDA reported the end of the session.
    terminated: bool,
    /// The last `stopped` event since the program last ran: KDA reports a breakpoint twice.
    stopped: Option<Value>,
}

impl Proxy {
    fn new(mirror: Mirror, root: PathBuf) -> Self {
        Self {
            mirror,
            root,
            next_seq: FIRST_OWN_SEQ,
            launch: None,
            attaching: None,
            fanouts: HashMap::new(),
            fanout_parts: HashMap::new(),
            breakpoint_files: HashMap::new(),
            terminated: false,
            stopped: None,
        }
    }

    fn on_client(&mut self, mut message: Value) -> Vec<Action> {
        if message["type"] != "request" {
            self.mirror.to_adapter(&mut message);
            return vec![Action::Adapter(message)];
        }
        let seq = message["seq"].clone();
        let command = message["command"].as_str().unwrap_or_default();
        if matches!(
            command,
            "continue" | "next" | "stepIn" | "stepOut" | "stepBack" | "reverseContinue" | "pause" | "goto" | "restartFrame"
        ) {
            self.stopped = None;
        }
        match command {
            "launch" => {
                let arguments = message["arguments"].clone();
                match launch_command(&arguments, &self.root) {
                    Ok(launch) => {
                        let what = format!("`kargo {}`", launch.args.first().map(String::as_str).unwrap_or_default());
                        self.launch = Some(PendingLaunch { seq, arguments, what });
                        vec![Action::Launch(launch)]
                    }
                    Err(e) => vec![Action::Client(error_response(&message, &format!("{e:#}")))],
                }
            }
            "attach" => {
                let mut arguments = message["arguments"].clone();
                if !arguments.is_object() {
                    arguments = json!({});
                }
                defaults(&mut arguments, &self.mirror, None);
                message["arguments"] = arguments;
                vec![Action::Attach(message)]
            }
            "setBreakpoints" => self.set_breakpoints(message),
            "disconnect" | "terminate" => {
                self.mirror.to_adapter(&mut message);
                vec![Action::Adapter(message), Action::Stop]
            }
            _ => {
                self.mirror.to_adapter(&mut message);
                vec![Action::Adapter(message)]
            }
        }
    }

    /// Sends a breakpoint request once for each class of the file, so KDA looks for the
    /// breakpoints in all of them.
    fn set_breakpoints(&mut self, mut message: Value) -> Vec<Action> {
        let path = message["arguments"]["source"]["path"].as_str().unwrap_or_default().to_owned();
        let files = self.mirror.breakpoint_files(Path::new(&path));
        let Some(seq) = message["seq"].as_i64().filter(|_| !files.is_empty()) else {
            self.mirror.to_adapter(&mut message);
            return vec![Action::Adapter(message)];
        };
        // Clear the breakpoints of classes the file no longer declares.
        let mut targets: Vec<(String, bool)> = files.iter().map(|f| (f.clone(), true)).collect();
        for old in self.breakpoint_files.insert(path, files.clone()).unwrap_or_default() {
            if !files.contains(&old) {
                targets.push((old, false));
            }
        }
        let mut actions = Vec::new();
        for (index, (file, keep)) in targets.iter().enumerate() {
            let mut part = message.clone();
            let part_seq = if index == 0 { seq } else { self.own_seq() };
            part["seq"] = json!(part_seq);
            part["arguments"]["source"]["path"] = json!(file);
            if let Some(name) = Path::new(file).file_name() {
                part["arguments"]["source"]["name"] = json!(name.to_string_lossy());
            }
            if !keep {
                part["arguments"]["breakpoints"] = json!([]);
                part["arguments"]["lines"] = json!([]);
            }
            self.fanout_parts.insert(part_seq, (seq, index));
            actions.push(Action::Adapter(part));
        }
        self.fanouts.insert(seq, Fanout { request: message, responses: vec![None; targets.len()] });
        actions
    }

    fn on_adapter(&mut self, mut message: Value) -> Vec<Action> {
        if message["type"] == "response" {
            let request_seq = message["request_seq"].as_i64();
            if let Some((seq, index)) = request_seq.and_then(|s| self.fanout_parts.remove(&s)) {
                return self.fanout_response(seq, index, message);
            }
            if self.attaching.is_some() && self.attaching.as_ref() == message.get("request_seq") {
                self.attaching = None;
                message["command"] = json!("launch");
            }
        }
        if message["type"] == "event" && message["event"] == "terminated" {
            self.terminated = true;
        }
        if message["type"] == "event" && message["event"] == "stopped" {
            if self.stopped.as_ref() == Some(&message["body"]) {
                return Vec::new();
            }
            self.stopped = Some(message["body"].clone());
        }
        self.mirror.to_client(&mut message);
        vec![Action::Client(message)]
    }

    fn fanout_response(&mut self, seq: i64, index: usize, response: Value) -> Vec<Action> {
        let Some(fanout) = self.fanouts.get_mut(&seq) else { return Vec::new() };
        fanout.responses[index] = Some(response);
        if fanout.responses.iter().any(Option::is_none) {
            return Vec::new();
        }
        let fanout = self.fanouts.remove(&seq).expect("present");
        let responses: Vec<Value> = fanout.responses.into_iter().flatten().collect();
        let mut merged = responses[0].clone();
        merged["request_seq"] = json!(seq);
        if merged["success"] == true {
            let source = fanout.request["arguments"]["source"].clone();
            let count = fanout.request["arguments"]["breakpoints"].as_array().map_or(0, Vec::len);
            let breakpoints: Vec<Value> = (0..count)
                .map(|i| {
                    let candidates = responses.iter().filter_map(|r| r["body"]["breakpoints"].get(i));
                    let mut chosen = candidates.clone().find(|b| b["verified"] == true).or(candidates.clone().next()).cloned();
                    let mut breakpoint = chosen.take().unwrap_or_else(|| json!({ "verified": false }));
                    breakpoint["source"] = source.clone();
                    breakpoint
                })
                .collect();
            merged["body"]["breakpoints"] = json!(breakpoints);
        }
        vec![Action::Client(merged)]
    }

    /// The launched JVM listens: KDA attaches to it.
    fn listening(&mut self, port: u16) -> Vec<Action> {
        let Some(launch) = self.launch.take() else { return Vec::new() };
        let mut arguments = launch.arguments;
        if !arguments.is_object() {
            arguments = json!({});
        }
        defaults(&mut arguments, &self.mirror, Some(port));
        self.attaching = Some(launch.seq.clone());
        vec![Action::Attach(json!({ "seq": launch.seq, "type": "request", "command": "attach", "arguments": arguments }))]
    }

    /// The launched program exited.
    fn exited(&mut self, code: Option<i32>) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(launch) = self.launch.take() {
            let status = code.map_or("was stopped".to_owned(), |c| format!("exited with code {c}"));
            let text = format!("{} {status} before the program waited for a debugger; see its output above", launch.what);
            actions.push(Action::Client(error_response(&json!({ "seq": launch.seq, "command": "launch" }), &text)));
        }
        if !self.terminated {
            self.terminated = true;
            actions.push(Action::Client(event_message("terminated", json!({}))));
        }
        actions
    }

    /// The launched program couldn't be started.
    fn launch_failed(&mut self, error: &str) -> Vec<Action> {
        let Some(launch) = self.launch.take() else { return Vec::new() };
        vec![Action::Client(error_response(&json!({ "seq": launch.seq, "command": "launch" }), error))]
    }

    fn own_seq(&mut self) -> i64 {
        self.next_seq += 1;
        self.next_seq
    }
}

/// How long KDA's traffic must pause before the JVM's first event reaches it.
const QUIET: Duration = Duration::from_millis(750);

/// Relays a JDWP connection to the JVM at `host:port`, and returns the local port KDA should
/// attach to instead.
///
/// The JVM waits suspended, and its first event (`VMStart`) tells the debugger it may resume
/// it. KDA starts handling events before it installs the breakpoints it already knows, so it
/// would resume the program before they are in place, and a breakpoint early in `main` is
/// missed. The gate holds that first event back until KDA has been quiet for [`QUIET`], which
/// is when it has sent its breakpoint requests; replies to its commands pass meanwhile.
fn gate(host: &str, port: u16) -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let local = listener.local_addr()?.port();
    let target = (host.to_owned(), port);
    thread::spawn(move || {
        let Ok((debugger, _)) = listener.accept() else { return };
        let Ok(vm) = TcpStream::connect((target.0.as_str(), target.1)) else { return };
        let _ = relay_jdwp(debugger, vm);
    });
    Ok(local)
}

fn relay_jdwp(debugger: TcpStream, vm: TcpStream) -> io::Result<()> {
    let last_command = Arc::new(Mutex::new(Instant::now()));
    let to_debugger = Arc::new(Mutex::new(debugger.try_clone()?));

    // Debugger to JVM: copied as is, noting when it last sent something.
    {
        let (mut from, mut to, last) = (debugger, vm.try_clone()?, Arc::clone(&last_command));
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = from.read(&mut buf) {
                if n == 0 || to.write_all(&buf[..n]).is_err() {
                    break;
                }
                if let Ok(mut last) = last.lock() {
                    *last = Instant::now();
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Write);
        });
    }

    // JVM to debugger: the handshake, then packets, holding back the first event.
    let mut from = vm;
    let mut handshake = [0u8; 14];
    from.read_exact(&mut handshake)?;
    to_debugger.lock().map_err(|_| io::ErrorKind::Other)?.write_all(&handshake)?;
    let held: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let mut released = false;
    loop {
        let mut header = [0u8; 11];
        if from.read_exact(&mut header).is_err() {
            break;
        }
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let mut packet = header.to_vec();
        packet.resize(length.max(11), 0);
        from.read_exact(&mut packet[11..])?;
        // A command (not a reply) of the Event command set: Composite.
        let event = header[8] & 0x80 == 0 && header[9] == 64 && header[10] == 100;
        if event && !released {
            released = true;
            *held.lock().map_err(|_| io::ErrorKind::Other)? = Some(packet);
            let (held, out, last) = (Arc::clone(&held), Arc::clone(&to_debugger), Arc::clone(&last_command));
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_millis(50));
                    let quiet = last.lock().map(|l| l.elapsed() >= QUIET).unwrap_or(true);
                    if quiet {
                        break;
                    }
                }
                if let (Ok(mut out), Ok(mut held)) = (out.lock(), held.lock())
                    && let Some(packet) = held.take()
                {
                    let _ = out.write_all(&packet);
                }
            });
            continue;
        }
        let mut out = to_debugger.lock().map_err(|_| io::ErrorKind::Other)?;
        // Later events wait for the first one.
        if event
            && let Ok(mut held) = held.lock()
            && let Some(first) = held.take()
        {
            out.write_all(&first)?;
        }
        out.write_all(&packet)?;
    }
    if let Ok(out) = to_debugger.lock() {
        let _ = out.shutdown(std::net::Shutdown::Write);
    }
    Ok(())
}

/// Fills in what KDA's `attach` requires.
fn defaults(arguments: &mut Value, mirror: &Mirror, port: Option<u16>) {
    let object = arguments.as_object_mut().expect("an object");
    object.insert("projectRoot".into(), json!(mirror.root.display().to_string()));
    if let Some(port) = port {
        object.insert("port".into(), json!(port));
        object.insert("hostName".into(), json!("127.0.0.1"));
    }
    object.entry("port").or_insert(json!(5005));
    object.entry("hostName").or_insert(json!("127.0.0.1"));
    object.entry("timeout").or_insert(json!(ATTACH_TIMEOUT_MS));
    for key in ["args", "cwd", "env"] {
        object.remove(key);
    }
}

/// The `kargo` command line of a launch request: its `args` with `--debug=0`, and `-t jvm`
/// unless they name a target.
fn launch_command(arguments: &Value, root: &Path) -> Result<Launch> {
    let mut args: Vec<String> = match &arguments["args"] {
        Value::Null => vec!["run".to_owned()],
        Value::Array(items) => items.iter().map(|a| a.as_str().map(str::to_owned)).collect::<Option<_>>().unwrap_or_default(),
        _ => Vec::new(),
    };
    if args.is_empty() {
        bail!("`args` must be a list of the arguments of `kargo`, such as [\"run\"] or [\"test\", \"com.example.MathTest\"]");
    }
    if !matches!(args[0].as_str(), "run" | "r" | "test" | "t") {
        bail!("`kargo dap` debugs `kargo run` and `kargo test`, not `kargo {}`", args[0]);
    }
    let own = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let names_target =
        args[1..own].iter().any(|a| a == "-t" || a == "--target" || a.starts_with("--target=") || (a.starts_with("-t") && a.len() > 2));
    args.retain(|a| a != "--debug" && !a.starts_with("--debug="));
    let mut extra = vec!["--debug=0".to_owned()];
    if !names_target {
        extra.extend(["-t".to_owned(), "jvm".to_owned()]);
    }
    args.splice(1..1, extra);
    let cwd = match arguments["cwd"].as_str() {
        Some(dir) if !dir.is_empty() => root.join(dir),
        _ => root.to_path_buf(),
    };
    let env =
        arguments["env"].as_object().into_iter().flatten().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect();
    Ok(Launch { args, cwd, env })
}

fn error_response(request: &Value, message: &str) -> Value {
    json!({
        "seq": 0,
        "type": "response",
        "request_seq": request["seq"],
        "success": false,
        "command": request["command"],
        "message": message,
        "body": { "error": { "id": 1, "format": message, "showUser": true } },
    })
}

fn event_message(event: &str, body: Value) -> Value {
    json!({ "seq": 0, "type": "event", "event": event, "body": body })
}

/// The project's sources as KDA expects them: placeholders below `target/dap/src/main/kotlin`.
#[derive(Debug, Default)]
struct Mirror {
    /// The project root KDA is given.
    root: PathBuf,
    /// The real project root.
    project: PathBuf,
    /// The mirrored file of each source file.
    to_mirror: HashMap<String, String>,
    /// The source file of each mirrored file.
    to_source: HashMap<String, String>,
}

impl Mirror {
    /// Mirrors the JVM sources and tests of every package in `metadata` into `dir`.
    fn build(metadata: &Value, dir: &Path) -> Result<Self> {
        let project = PathBuf::from(metadata["root"].as_str().unwrap_or_default());
        let mut mirror = Mirror { root: dir.to_path_buf(), project, ..Mirror::default() };
        kargo_util::fs::remove_dir_all_if_exists(&dir.join("src"))?;
        let mut seen = HashSet::new();
        for package in metadata["packages"].as_array().into_iter().flatten() {
            for target in package["targets"].as_array().into_iter().flatten().filter(|t| t["kind"] == "jvm") {
                for key in ["source-roots", "test-roots"] {
                    for root in target[key].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        for file in kargo_util::fs::collect_files(Path::new(root), &["kt", "java"]) {
                            if seen.insert(file.clone()) {
                                mirror.add(&file);
                            }
                        }
                    }
                }
            }
        }
        Ok(mirror)
    }

    fn kotlin_dir(&self) -> PathBuf {
        self.root.join("src").join("main").join("kotlin")
    }

    /// Mirrors a source file under its package, and returns what it declares.
    fn add(&mut self, file: &Path) -> Option<Declarations> {
        let text = std::fs::read_to_string(file).ok()?;
        let declarations = Declarations::parse(&text);
        let name = file.file_name()?;
        let mirrored = declarations.package_dir(&self.kotlin_dir()).join(name);
        if let Some(parent) = mirrored.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !mirrored.exists() {
            let _ = std::fs::write(&mirrored, "");
        }
        let (source, mirrored) = (file.display().to_string(), mirrored.display().to_string());
        self.to_source.entry(mirrored.clone()).or_insert_with(|| source.clone());
        let canonical = kargo_util::fs::dunce_canonicalize(file).display().to_string();
        if canonical != source {
            self.to_mirror.insert(canonical, mirrored.clone());
        }
        self.to_mirror.insert(source, mirrored);
        Some(declarations)
    }

    /// The mirrored paths KDA should get a source's breakpoints for: one per class the file
    /// declares, so KDA's guess of the class name from the path finds each. Empty for a file
    /// outside the project.
    fn breakpoint_files(&mut self, file: &Path) -> Vec<String> {
        let known = self.to_mirror.contains_key(&file.display().to_string());
        let inside = file.extension().is_some_and(|e| e == "kt" || e == "java")
            && !self.project.as_os_str().is_empty()
            && file.starts_with(&self.project);
        if !known && !inside {
            return Vec::new();
        }
        // Read the file again: it may have changed, or be new.
        let Some(declarations) = self.add(file) else { return Vec::new() };
        let dir = declarations.package_dir(&self.kotlin_dir());
        let mut files = vec![self.to_mirror[&file.display().to_string()].clone()];
        let extension = file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for class in declarations.classes {
            if class == stem {
                continue;
            }
            let path = dir.join(format!("{class}.{extension}")).display().to_string();
            // A real file of the package with that name keeps its own breakpoints.
            if self.to_source.get(&path).is_some_and(|s| s != &file.display().to_string()) {
                continue;
            }
            if !files.contains(&path) {
                files.push(path);
            }
        }
        files
    }

    /// Rewrites the editor's source paths to the mirrored ones.
    fn to_adapter(&self, message: &mut Value) {
        rewrite(message, &self.to_mirror);
    }

    /// Rewrites mirrored paths back to the editor's.
    fn to_client(&self, message: &mut Value) {
        rewrite(message, &self.to_source);
    }
}

fn rewrite(value: &mut Value, map: &HashMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(replacement) = map.get(s.as_str()) {
                *s = replacement.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rewrite(v, map)),
        Value::Object(object) => object.values_mut().for_each(|v| rewrite(v, map)),
        _ => {}
    }
}

/// What a source file declares, as far as KDA's class names go.
#[derive(Debug, Default, PartialEq)]
struct Declarations {
    package: Vec<String>,
    /// Top-level classes, objects and interfaces, and the `@file:JvmName`.
    classes: Vec<String>,
}

impl Declarations {
    fn parse(text: &str) -> Self {
        const MODIFIERS: &[&str] = &[
            "public",
            "internal",
            "private",
            "protected",
            "data",
            "enum",
            "sealed",
            "abstract",
            "open",
            "final",
            "inline",
            "value",
            "annotation",
            "fun",
            "expect",
            "actual",
            "static",
            "strictfp",
        ];
        let mut out = Declarations::default();
        let mut in_comment = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if in_comment {
                in_comment = !trimmed.contains("*/");
                continue;
            }
            if trimmed.starts_with("/*") {
                in_comment = !trimmed.contains("*/");
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("package ") {
                let name = rest.trim_end_matches(';').split_whitespace().next().unwrap_or_default();
                out.package = name.split('.').map(|p| p.trim_matches('`').to_owned()).filter(|p| !p.is_empty()).collect();
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("@file:JvmName(") {
                let name = rest.trim_end_matches(')').trim().trim_matches('"');
                if !name.is_empty() {
                    out.classes.push(name.to_owned());
                }
                continue;
            }
            // Top-level declarations start at the beginning of a line.
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            let mut words = trimmed.split_whitespace().skip_while(|w| MODIFIERS.contains(w) || w.starts_with('@'));
            if let (Some("class" | "object" | "interface" | "record"), Some(name)) = (words.next(), words.next()) {
                let name: String = name.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() && !out.classes.contains(&name) {
                    out.classes.push(name);
                }
            }
        }
        out
    }

    fn package_dir(&self, base: &Path) -> PathBuf {
        self.package.iter().fold(base.to_path_buf(), |dir, part| dir.join(part))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_port_the_jvm_listens_on() {
        assert_eq!(listening_port("Listening for transport dt_socket at address: 5005\n"), Some(5005));
        assert_eq!(listening_port("x\nListening for transport dt_socket at address: 127.0.0.1:41234\r\nhello"), Some(41234));
        assert_eq!(listening_port("Listening for transport dt_socket at address: 50"), None, "the line isn't complete yet");
        assert_eq!(listening_port("Compiling app"), None);
    }

    #[test]
    fn turns_launch_arguments_into_a_kargo_command_line() {
        let root = Path::new("/p");
        let launch = launch_command(&json!({}), root).unwrap();
        assert_eq!(launch.args, ["run", "--debug=0", "-t", "jvm"]);
        assert_eq!(launch.cwd, root);
        let launch =
            launch_command(&json!({ "args": ["test", "--debug=7000", "MathTest"], "cwd": "lib", "env": { "A": "1" } }), root).unwrap();
        assert_eq!(launch.args, ["test", "--debug=0", "-t", "jvm", "MathTest"]);
        assert_eq!(launch.cwd, root.join("lib"));
        assert_eq!(launch.env, [("A".to_owned(), "1".to_owned())]);
        let launch = launch_command(&json!({ "args": ["run", "-t", "jvm", "--bin", "server", "--", "-t"] }), root).unwrap();
        assert_eq!(launch.args, ["run", "--debug=0", "-t", "jvm", "--bin", "server", "--", "-t"]);
        let launch = launch_command(&json!({ "args": ["run", "--", "-t"] }), root).unwrap();
        assert_eq!(launch.args, ["run", "--debug=0", "-t", "jvm", "--", "-t"], "program arguments don't name a target");
        assert!(launch_command(&json!({ "args": ["build"] }), root).unwrap_err().to_string().contains("not `kargo build`"));
        assert!(launch_command(&json!({ "args": "run" }), root).is_err());
    }

    #[test]
    fn parses_packages_and_top_level_classes() {
        let text = "/* header\n package fake\n */\n@file:JvmName(\"Tools\")\npackage com.`example`.app\n\nimport x.y\n\n@Serializable\ndata class User(val name: String)\nsealed interface Shape\nprivate object Cache {\n    class Inner\n}\nfun main() {}\nenum class Color { RED }\nfun interface Action { fun run() }\n";
        let declarations = Declarations::parse(text);
        assert_eq!(declarations.package, ["com", "example", "app"]);
        assert_eq!(declarations.classes, ["Tools", "User", "Shape", "Cache", "Color", "Action"]);
        let java = Declarations::parse("package a.b;\n\npublic final class Main {\n}\n");
        assert_eq!(java.package, ["a", "b"]);
        assert_eq!(java.classes, ["Main"]);
        assert_eq!(Declarations::parse("fun main() {}"), Declarations::default());
    }

    fn exactly<const N: usize>(actions: Vec<Action>) -> [Action; N] {
        let count = actions.len();
        actions.try_into().unwrap_or_else(|a| panic!("expected {N} actions, got {count}: {a:?}"))
    }

    fn project() -> (tempfile::TempDir, Proxy) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("src/server")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("src/Main.kt"), "package com.example\n\nfun main() {}\nclass Greeter\n").unwrap();
        std::fs::write(root.join("src/server/Routes.kt"), "package com.example.server\n\nobject Routes\n").unwrap();
        std::fs::write(root.join("tests/MainTest.kt"), "package com.example\n\nclass MainTest\n").unwrap();
        let metadata = json!({ "root": root.display().to_string(), "packages": [{ "targets": [
            { "kind": "jvm", "source-roots": [root.join("src").display().to_string()], "test-roots": [root.join("tests").display().to_string()] },
            { "kind": "wasm", "source-roots": [root.join("src-wasm").display().to_string()] },
        ] }] });
        // Joined per component, as `Project::target_dir` does, so the paths use the platform's separator.
        let mirror = Mirror::build(&metadata, &root.join("target").join("dap")).unwrap();
        (tmp, Proxy::new(mirror, root))
    }

    fn path(root: &Path, rel: &str) -> String {
        rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part)).display().to_string()
    }

    #[test]
    fn mirrors_sources_in_the_layout_kda_expects() {
        let (tmp, proxy) = project();
        let root = tmp.path();
        let main = path(root, "target/dap/src/main/kotlin/com/example/Main.kt");
        assert!(Path::new(&main).is_file());
        assert!(Path::new(&path(root, "target/dap/src/main/kotlin/com/example/server/Routes.kt")).is_file());
        assert!(Path::new(&path(root, "target/dap/src/main/kotlin/com/example/MainTest.kt")).is_file());
        assert_eq!(proxy.mirror.to_mirror[&path(root, "src/Main.kt")], main);

        // A stack trace from KDA names the mirrored file; the editor gets the real one.
        let mut proxy = proxy;
        let response = json!({ "type": "response", "request_seq": 4, "command": "stackTrace", "success": true,
            "body": { "stackFrames": [{ "id": 1, "name": "main", "line": 3, "source": { "name": "Main.kt", "path": main } }] } });
        let [Action::Client(out)] = exactly(proxy.on_adapter(response)) else { panic!() };
        assert_eq!(out["body"]["stackFrames"][0]["source"]["path"], path(root, "src/Main.kt"));
    }

    #[test]
    fn sends_breakpoints_once_per_class_and_merges_the_answers() {
        let (tmp, mut proxy) = project();
        let root = tmp.path();
        let source = json!({ "name": "Main.kt", "path": path(root, "src/Main.kt") });
        let request = json!({ "seq": 3, "type": "request", "command": "setBreakpoints",
            "arguments": { "source": source, "breakpoints": [{ "line": 3 }, { "line": 4 }] } });
        let actions = proxy.on_client(request);
        let sent: Vec<&Value> = actions.iter().map(|a| if let Action::Adapter(m) = a { m } else { panic!("{a:?}") }).collect();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["seq"], 3);
        assert_eq!(sent[0]["arguments"]["source"]["path"], path(root, "target/dap/src/main/kotlin/com/example/Main.kt"));
        assert_eq!(sent[1]["arguments"]["source"]["path"], path(root, "target/dap/src/main/kotlin/com/example/Greeter.kt"));
        let own = sent[1]["seq"].as_i64().unwrap();
        assert!(own > FIRST_OWN_SEQ);

        let answer = |seq: i64, verified: [bool; 2]| {
            json!({ "seq": 9, "type": "response", "request_seq": seq, "command": "setBreakpoints", "success": true,
                "body": { "breakpoints": [{ "verified": verified[0], "line": 3 }, { "verified": verified[1], "line": 4 }] } })
        };
        assert!(proxy.on_adapter(answer(own, [false, true])).is_empty(), "waits for every answer");
        let [Action::Client(merged)] = exactly(proxy.on_adapter(answer(3, [true, false]))) else { panic!() };
        assert_eq!(merged["request_seq"], 3);
        let breakpoints = merged["body"]["breakpoints"].as_array().unwrap();
        assert_eq!(breakpoints.iter().map(|b| b["verified"] == true).collect::<Vec<_>>(), [true, true]);
        assert_eq!(breakpoints[0]["source"], source);

        // The class is gone: its breakpoints are cleared.
        std::fs::write(root.join("src/Main.kt"), "package com.example\n\nfun main() {}\n").unwrap();
        let request =
            json!({ "seq": 5, "type": "request", "command": "setBreakpoints", "arguments": { "source": source, "breakpoints": [] } });
        let actions = proxy.on_client(request);
        assert_eq!(actions.len(), 2);
        let Action::Adapter(clear) = &actions[1] else { panic!() };
        assert_eq!(clear["arguments"]["source"]["path"], path(root, "target/dap/src/main/kotlin/com/example/Greeter.kt"));
        assert_eq!(clear["arguments"]["breakpoints"], json!([]));

        // Files outside the project pass through.
        let request = json!({ "seq": 6, "type": "request", "command": "setBreakpoints", "arguments": { "source": { "path": "/elsewhere/X.kt" }, "breakpoints": [] } });
        let [Action::Adapter(out)] = exactly(proxy.on_client(request)) else { panic!() };
        assert_eq!(out["arguments"]["source"]["path"], "/elsewhere/X.kt");
    }

    #[test]
    fn launches_kargo_and_attaches_once_the_jvm_listens() {
        let (tmp, mut proxy) = project();
        let root = tmp.path();
        let request = json!({ "seq": 2, "type": "request", "command": "launch", "arguments": { "args": ["run", "--bin", "app"], "logLevel": "DEBUG" } });
        let [Action::Launch(launch)] = exactly(proxy.on_client(request)) else { panic!() };
        assert_eq!(launch.args, ["run", "--debug=0", "-t", "jvm", "--bin", "app"]);

        let [Action::Attach(attach)] = exactly(proxy.listening(40123)) else { panic!() };
        assert_eq!(attach["seq"], 2);
        assert_eq!(attach["command"], "attach");
        assert_eq!(attach["arguments"]["port"], 40123);
        assert_eq!(attach["arguments"]["hostName"], "127.0.0.1");
        assert_eq!(attach["arguments"]["timeout"], ATTACH_TIMEOUT_MS);
        assert_eq!(attach["arguments"]["logLevel"], "DEBUG");
        assert_eq!(attach["arguments"]["projectRoot"], path(root, "target/dap"));
        assert!(attach["arguments"].get("args").is_none());

        let response = json!({ "seq": 7, "type": "response", "request_seq": 2, "command": "attach", "success": true });
        let [Action::Client(out)] = exactly(proxy.on_adapter(response)) else { panic!() };
        assert_eq!(out["command"], "launch", "the editor asked for a launch");

        let actions = proxy.on_client(json!({ "seq": 9, "type": "request", "command": "disconnect", "arguments": {} }));
        assert_eq!(actions.last(), Some(&Action::Stop));
        let terminated = json!({ "seq": 8, "type": "event", "event": "terminated" });
        proxy.on_adapter(terminated);
        assert!(proxy.exited(Some(0)).is_empty(), "KDA already reported the end");
    }

    #[test]
    fn reports_a_program_that_exits_before_it_waits_for_a_debugger() {
        let (_tmp, mut proxy) = project();
        let request = json!({ "seq": 2, "type": "request", "command": "launch", "arguments": { "args": ["test"] } });
        assert!(matches!(proxy.on_client(request).as_slice(), [Action::Launch(_)]));
        let actions = proxy.exited(Some(101));
        let [Action::Client(error), Action::Client(terminated)] = exactly(actions) else { panic!() };
        assert_eq!(error["request_seq"], 2);
        assert_eq!(error["success"], false);
        assert!(error["message"].as_str().unwrap().contains("`kargo test` exited with code 101"), "{error}");
        assert_eq!(terminated["event"], "terminated");
    }

    #[test]
    fn fills_in_what_kda_needs_to_attach() {
        let (tmp, mut proxy) = project();
        let request = json!({ "seq": 2, "type": "request", "command": "attach", "arguments": { "port": 8000 } });
        let [Action::Attach(out)] = exactly(proxy.on_client(request)) else { panic!() };
        assert_eq!(
            out["arguments"],
            json!({ "port": 8000, "hostName": "127.0.0.1", "timeout": ATTACH_TIMEOUT_MS, "projectRoot": path(tmp.path(), "target/dap") })
        );
    }
}
