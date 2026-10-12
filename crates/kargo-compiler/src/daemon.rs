//! Client for the Kargo compiler daemon: a JVM that stays running between
//! builds and runs the Kotlin compiler and javac in-process (see
//! `daemon/KargoDaemon.java`). A warm daemon skips JVM startup and runs on an
//! already JIT-compiled compiler, which is most of the time of a small build.
//!
//! One daemon runs per JDK, compiler classpath and JVM options; it exits after
//! [`IDLE_TIMEOUT_SECS`] without work. When the daemon cannot be used (disabled
//! with `KARGO_DAEMON=false`, no `javac` to build it, failure to start), callers
//! fall back to running the tool as a separate process.

use std::io::{BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::diagnostics;
use kargo_toolchain::Jdk;
use kargo_util::hash::{Fingerprint, sha256_bytes};
use kargo_util::shell::Shell;

const SOURCE: &str = include_str!("daemon/KargoDaemon.java");
/// The incremental compilation bridge, built against the project's Build Tools API.
const IC_SOURCE: &str = include_str!("daemon/KargoIc.java");
const PROTOCOL: u32 = 1;
/// Exit code with which the daemon asks the client to run the tool as a process.
const UNSUPPORTED: i32 = -1000;
/// Exit code of an incremental compilation that failed for reasons other than compile
/// errors (corrupt caches, an API mismatch); the caller should rebuild from scratch.
pub const IC_FAILED: i32 = -1001;
/// Seconds a daemon waits for work before exiting.
pub const IDLE_TIMEOUT_SECS: u64 = 3 * 60 * 60;
/// How long to wait for a freshly started daemon to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Environment variable that disables the daemon when set to `false`, `0`, `no` or `off`.
pub const DAEMON_ENV: &str = "KARGO_DAEMON";

/// `<home>/daemon` and whether the daemon is enabled, once [`configure`] ran.
static STATE: OnceLock<(PathBuf, bool)> = OnceLock::new();
/// Serializes starting daemons within this process (parallel targets share one daemon).
static STARTING: Mutex<()> = Mutex::new(());

/// Enables the daemon for this process, keeping its state under `<home>/daemon`.
/// Without this call (as in unit tests) tools always run as separate processes.
pub fn configure(home: &Path) {
    let enabled = std::env::var(DAEMON_ENV).map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off"));
    let _ = STATE.set((home.join("daemon"), enabled.unwrap_or(true)));
}

fn root() -> Option<&'static Path> {
    STATE.get().filter(|(_, enabled)| *enabled).map(|(dir, _)| dir.as_path())
}

/// A tool the daemon can run.
#[derive(Debug, Clone, Copy)]
pub enum Tool<'a> {
    /// A Kotlin compiler main class such as `K2JVMCompiler`.
    Kotlin(&'a str),
    Javac,
    /// Incremental Kotlin/JVM compilation through the Build Tools API (arguments as in `KargoIc.java`).
    KotlinIncremental,
}

/// The JVM options a daemon (or a standalone compiler process) runs with.
pub fn jvm_options(jdk: &Jdk, cds_archive: Option<&Path>) -> Vec<String> {
    let mut opts = super::compiler_jvm_options(jdk);
    opts.push("-XX:-UsePerfData".to_owned());
    if let Some(archive) = cds_archive {
        opts.extend(cds_flags(jdk, archive));
    }
    opts
}

/// On JDK 19+, records the classes a JVM loads at startup into `archive` on the first
/// run and maps them on later runs, which cuts JVM startup by a third or more.
fn cds_flags(jdk: &Jdk, archive: &Path) -> Vec<String> {
    if jdk.version < 19 {
        return Vec::new();
    }
    vec![
        "-XX:+AutoCreateSharedArchive".to_owned(),
        format!("-XX:SharedArchiveFile={}", archive.display()),
        // Archiving notes (e.g. classes skipped while dumping) are not the user's concern.
        "-Xlog:cds*=off".to_owned(),
        "-Xlog:class+path=off".to_owned(),
    ]
}

/// Class-data-sharing flags for a short-lived JVM (a standalone compiler, the JUnit launcher)
/// whose classpath is the jars in `classpath`. Empty before [`configure`] or on old JDKs.
/// The classpath must not contain directories, which archives don't support.
pub fn cds_options(jdk: &Jdk, name: &str, classpath: &[PathBuf]) -> Vec<String> {
    let Some((root, _)) = STATE.get() else { return Vec::new() };
    let mut fp = Fingerprint::new();
    fp.str(&jdk.java().to_string_lossy()).str(&jdk.version.to_string()).str(name);
    for p in classpath {
        fp.file_stamp(p);
    }
    let dir = root.join("cds");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    cds_flags(jdk, &dir.join(format!("{name}-{}.jsa", &fp.finish()[..16])))
}

/// Runs `tool` with `args` in the daemon for this JDK and compiler classpath, streaming its
/// output to stderr. Returns the exit code, or `None` when the daemon can't be used and the
/// caller should run the tool as a process instead.
pub fn run(shell: &Shell, jdk: &Jdk, compiler_classpath: &[PathBuf], tool: Tool, args: &[String]) -> Option<i32> {
    let root = root()?;
    match try_run(shell, root, jdk, compiler_classpath, tool, args) {
        Ok(Some(code)) => Some(code),
        Ok(None) => None,
        Err(e) => {
            shell.verbose("Fallback", format!("not using the compiler daemon: {e:#}"));
            None
        }
    }
}

fn try_run(shell: &Shell, root: &Path, jdk: &Jdk, classpath: &[PathBuf], tool: Tool, args: &[String]) -> Result<Option<i32>> {
    let classpath = &with_ic_bridge(shell, root, jdk, classpath);
    let dir = root.join(daemon_key(jdk, classpath));
    let info = dir.join("info");
    let (command, mut list): (&str, Vec<&str>) = match tool {
        Tool::Kotlin(main) => ("kotlin", vec![main]),
        Tool::Javac => ("javac", Vec::new()),
        Tool::KotlinIncremental => ("kotlin-ic", Vec::new()),
    };
    list.extend(args.iter().map(String::as_str));
    if shell.is_verbose() {
        shell.verbose("Running", format!("{command} in the compiler daemon ({})", dir.display()));
    }
    let seen = std::fs::read_to_string(&info).ok();
    if let Some((stream, token)) = connect(&info) {
        match request(stream, &token, command, &list)? {
            Some(code) => return Ok((code != UNSUPPORTED).then_some(code)),
            // Rejected without an answer: the port belongs to another process now (a stale
            // `info` file, e.g. after a crash or restored from a CI cache). Start a new daemon.
            None => shell.verbose("Restarting", "the compiler daemon did not answer"),
        }
    }
    let (stream, token) = {
        let _guard = STARTING.lock().unwrap_or_else(|e| e.into_inner());
        // Another thread may have started a daemon while this one waited for the lock.
        let fresh = (std::fs::read_to_string(&info).ok() != seen).then(|| connect(&info)).flatten();
        match fresh {
            Some(c) => c,
            None => start(shell, root, &dir, jdk, classpath)?,
        }
    };
    let code = request(stream, &token, command, &list)?.context("the new compiler daemon closed the connection")?;
    Ok((code != UNSUPPORTED).then_some(code))
}

/// Where classpath snapshots of jars are cached for incremental compilation, once the
/// daemon is enabled (incremental compilation runs only in the daemon).
pub fn snapshot_cache_dir() -> Option<PathBuf> {
    root().map(|r| r.join("ic-snapshots"))
}

/// Whether `classpath` can compile incrementally (it contains the Build Tools API implementation).
pub fn supports_incremental(classpath: &[PathBuf]) -> bool {
    find_jar(classpath, "kotlin-build-tools-api-").is_some() && find_jar(classpath, "kotlin-build-tools-impl-").is_some()
}

fn find_jar<'a>(classpath: &'a [PathBuf], prefix: &str) -> Option<&'a PathBuf> {
    classpath.iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix)))
}

/// Appends the incremental compilation bridge to a compiler classpath that has the Build
/// Tools API, so every request for this compiler goes to the same daemon.
fn with_ic_bridge(shell: &Shell, root: &Path, jdk: &Jdk, classpath: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = classpath.to_vec();
    if !supports_incremental(classpath) {
        return out;
    }
    match ic_bridge(root, jdk, classpath) {
        Ok(dir) => out.push(dir),
        Err(e) => shell.verbose("Skipping", format!("incremental compilation: {e:#}")),
    }
    out
}

/// Compiles `KargoIc.java` against the classpath's Build Tools API, once per API jar and JDK.
fn ic_bridge(root: &Path, jdk: &Jdk, classpath: &[PathBuf]) -> Result<PathBuf> {
    let api = find_jar(classpath, "kotlin-build-tools-api-").context("no Build Tools API jar")?;
    let mut fp = Fingerprint::new();
    fp.str(&sha256_bytes(IC_SOURCE.as_bytes())).str(&api.to_string_lossy()).str(&jdk.version.to_string());
    let classes = root.join(format!("ic-{}", &fp.finish()[..16]));
    if classes.join("KargoIc.class").is_file() {
        return Ok(classes);
    }
    // Older Build Tools APIs (before Kotlin 2.4) lack what the bridge uses; don't retry every build.
    let failed = classes.with_extension("failed");
    if failed.is_file() {
        bail!("this Kotlin version's Build Tools API is not supported (see {})", failed.display());
    }
    let mut javac_cp = vec![api.clone()];
    javac_cp.extend(find_jar(classpath, "kotlin-stdlib-").cloned());
    if let Err(e) = compile_java(root, jdk, "KargoIc.java", IC_SOURCE, &javac_cp, &classes) {
        let _ = std::fs::create_dir_all(root);
        let _ = std::fs::write(&failed, format!("{e:#}\n"));
        return Err(e);
    }
    Ok(classes)
}

/// Compiles one Java source into `classes` (atomically, so concurrent builds don't clash).
fn compile_java(root: &Path, jdk: &Jdk, file_name: &str, source: &str, classpath: &[PathBuf], classes: &Path) -> Result<()> {
    if !jdk.has_javac() {
        bail!("{} has no `javac` to build the daemon", jdk.home.display());
    }
    let staging = root.join(format!(".staging-{}-{}", std::process::id(), file_name.trim_end_matches(".java")));
    kargo_util::fs::remove_dir_all_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;
    let source_path = staging.join(file_name);
    std::fs::write(&source_path, source)?;
    let out = staging.join("classes");
    let mut cmd = Command::new(jdk.javac());
    cmd.args(["-nowarn", "-encoding", "UTF-8", "--release", "11", "-d"]).arg(&out);
    if !classpath.is_empty() {
        cmd.arg("-cp").arg(kargo_util::fs::join_paths(classpath));
    }
    let output = cmd.arg(&source_path).stdin(Stdio::null()).output().context("failed to run javac")?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("javac failed to build {file_name}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let main_class = format!("{}.class", file_name.trim_end_matches(".java"));
    if std::fs::rename(&out, classes).is_err() && !classes.join(&main_class).is_file() {
        bail!("failed to install the daemon classes into {}", classes.display());
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// The daemon directory name: everything that must match for a daemon to be reused.
fn daemon_key(jdk: &Jdk, classpath: &[PathBuf]) -> String {
    let mut fp = Fingerprint::new();
    fp.str(&sha256_bytes(SOURCE.as_bytes()));
    fp.str(&jdk.java().to_string_lossy()).str(&jdk.version.to_string());
    for p in classpath {
        fp.str(&p.to_string_lossy());
    }
    for o in super::compiler_jvm_options(jdk) {
        fp.str(&o);
    }
    fp.finish()[..16].to_owned()
}

/// Connects to the daemon described by `info` (`<port> <token> <pid>`).
fn connect(info: &Path) -> Option<(TcpStream, String)> {
    let text = std::fs::read_to_string(info).ok()?;
    let mut parts = text.split_whitespace();
    let port: u16 = parts.next()?.parse().ok()?;
    let token = parts.next()?.to_owned();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    Some((stream, token))
}

/// Compiles the daemon class once per source version and JDK major, shared by all daemons.
fn daemon_classes(root: &Path, jdk: &Jdk) -> Result<PathBuf> {
    let classes = root.join(format!("classes-{}-{}", &sha256_bytes(SOURCE.as_bytes())[..12], jdk.version));
    if !classes.join("KargoDaemon.class").is_file() {
        compile_java(root, jdk, "KargoDaemon.java", SOURCE, &[], &classes)?;
    }
    Ok(classes)
}

fn start(shell: &Shell, root: &Path, dir: &Path, jdk: &Jdk, classpath: &[PathBuf]) -> Result<(TcpStream, String)> {
    let classes = daemon_classes(root, jdk)?;
    std::fs::create_dir_all(dir)?;
    let info = dir.join("info");
    let before = std::fs::read_to_string(&info).ok();
    let log = std::fs::File::create(dir.join("daemon.log"))?;
    let mut cp = vec![classes];
    cp.extend(classpath.iter().cloned());
    let mut cmd = Command::new(jdk.java());
    cmd.args(jvm_options(jdk, Some(&dir.join("cds.jsa"))))
        .arg("-cp")
        .arg(kargo_util::fs::join_paths(&cp))
        .arg("KargoDaemon")
        .arg(&info)
        .arg(IDLE_TIMEOUT_SECS.to_string())
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    shell.verbose("Starting", format!("compiler daemon `{}`", kargo_util::process::describe(&cmd)));
    let mut child = cmd.spawn().context("failed to start the compiler daemon")?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        // Wait for a new info file (a stale one may point at a dead daemon).
        if std::fs::read_to_string(&info).ok() != before {
            if let Some(c) = connect(&info) {
                return Ok(c);
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!("the compiler daemon exited during startup ({status}); see {}", dir.join("daemon.log").display());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("the compiler daemon did not start within {}s", START_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Keeps the daemon alive after Kargo exits and out of the terminal's Ctrl-C.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

fn write_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Sends one request and relays its output; returns the exit code, or `None` if the
/// connection closed before any response (not a daemon of ours, or the wrong token).
fn request(mut stream: TcpStream, token: &str, command: &str, args: &[&str]) -> Result<Option<i32>> {
    stream.set_nodelay(true).ok();
    let mut buf = Vec::new();
    buf.extend_from_slice(&PROTOCOL.to_be_bytes());
    write_bytes(&mut buf, token.as_bytes());
    write_bytes(&mut buf, command.as_bytes());
    buf.extend_from_slice(&(args.len() as u32).to_be_bytes());
    for a in args {
        write_bytes(&mut buf, a.as_bytes());
    }
    if stream.write_all(&buf).and_then(|()| stream.flush()).is_err() {
        return Ok(None);
    }

    let mut reader = BufReader::new(stream);
    let mut output = OutputRelay::new();
    let mut answered = false;
    loop {
        let mut kind = [0u8; 1];
        if let Err(e) = reader.read_exact(&mut kind) {
            if !answered {
                return Ok(None);
            }
            return Err(e).context("the compiler daemon closed the connection");
        }
        if !answered && !matches!(kind[0], b'o' | b'x') {
            return Ok(None);
        }
        answered = true;
        let mut word = [0u8; 4];
        reader.read_exact(&mut word)?;
        match kind[0] {
            b'o' => {
                let mut chunk = vec![0u8; u32::from_be_bytes(word) as usize];
                reader.read_exact(&mut chunk)?;
                output.push(&chunk);
            }
            b'x' => {
                output.finish();
                return Ok(Some(i32::from_be_bytes(word)));
            }
            other => bail!("unexpected frame {other:#x} from the daemon"),
        }
    }
}

/// Passes compiler output to [`diagnostics::Relay`] line by line, making paths under
/// the working directory relative as a standalone compiler would.
struct OutputRelay {
    pending: Vec<u8>,
    /// The working directory, as written natively and with forward slashes (how Kargo
    /// passes source paths, so how the compiler prints them on Windows).
    prefixes: Vec<String>,
    relay: diagnostics::Relay,
}

impl OutputRelay {
    fn new() -> Self {
        let mut prefixes = Vec::new();
        if let Ok(dir) = std::env::current_dir() {
            prefixes.push(format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR));
            prefixes.push(format!("{}/", kargo_util::process::slash(&dir)));
            prefixes.dedup();
        }
        Self { pending: Vec::new(), prefixes, relay: diagnostics::Relay::new() }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        if let Some(end) = self.pending.iter().rposition(|&b| b == b'\n') {
            let rest = self.pending.split_off(end + 1);
            let complete = std::mem::replace(&mut self.pending, rest);
            self.emit(&complete);
        }
    }

    fn finish(&mut self) {
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.emit(&rest);
        }
        self.relay.flush();
    }

    fn emit(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        self.relay.text(&relativize(&text, &self.prefixes));
    }
}

fn relativize(text: &str, prefixes: &[String]) -> String {
    text.split_inclusive('\n').map(|line| prefixes.iter().find_map(|p| line.strip_prefix(p.as_str())).unwrap_or(line)).collect()
}

/// A running daemon, as listed by `kargo daemon status`.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub dir: PathBuf,
    pub pid: u32,
}

/// Lists the daemons that accept connections.
pub fn running(home: &Path) -> Vec<DaemonInfo> {
    let Ok(entries) = std::fs::read_dir(home.join("daemon")) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let info = entry.path().join("info");
        let Some(pid) = std::fs::read_to_string(&info).ok().and_then(|t| t.split_whitespace().nth(2)?.parse().ok()) else { continue };
        if connect(&info).is_some() {
            out.push(DaemonInfo { dir: entry.path(), pid });
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Asks every running daemon to exit; returns how many were stopped.
pub fn stop_all(home: &Path) -> usize {
    let mut stopped = 0;
    for daemon in running(home) {
        let info = daemon.dir.join("info");
        if connect(&info).is_some_and(|(stream, token)| matches!(request(stream, &token, "stop", &[]), Ok(Some(_)))) {
            stopped += 1;
        }
        let _ = std::fs::remove_file(&info);
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativizes_paths_under_the_working_directory() {
        let text = "/p/src/Main.kt:2:16: error: x\n    val x = 1\n/other/A.kt:1:1: warning: y\n";
        assert_eq!(relativize(text, &["/p/".to_owned()]), "src/Main.kt:2:16: error: x\n    val x = 1\n/other/A.kt:1:1: warning: y\n");
        assert_eq!(relativize("no newline", &[]), "no newline");
        // On Windows the compiler prints the forward-slash paths Kargo passes it.
        let prefixes = [r"C:\p\".to_owned(), "C:/p/".to_owned()];
        assert_eq!(
            relativize("C:/p/src/A.kt:1:1: error: x\nC:\\p\\B.kt:2:2: error: y\n", &prefixes),
            "src/A.kt:1:1: error: x\nB.kt:2:2: error: y\n"
        );
    }

    #[test]
    fn relay_splits_lines_across_chunks() {
        let mut relay = OutputRelay { pending: Vec::new(), prefixes: Vec::new(), relay: diagnostics::Relay::new() };
        relay.pending.extend_from_slice(b"partial");
        relay.push(b" line\nnext");
        assert_eq!(relay.pending, b"next");
    }

    #[test]
    fn daemon_key_depends_on_jdk_and_classpath() {
        let jdk = Jdk { home: PathBuf::from("/jdk"), version: 21 };
        let a = daemon_key(&jdk, &[PathBuf::from("a.jar")]);
        assert_eq!(a, daemon_key(&jdk, &[PathBuf::from("a.jar")]));
        assert_ne!(a, daemon_key(&jdk, &[PathBuf::from("b.jar")]));
        assert_ne!(a, daemon_key(&Jdk { home: PathBuf::from("/jdk"), version: 17 }, &[PathBuf::from("a.jar")]));
    }

    #[test]
    fn unconfigured_daemon_is_not_used() {
        let jdk = Jdk { home: PathBuf::from("/nonexistent"), version: 21 };
        if STATE.get().is_none() {
            assert!(run(&Shell::new(kargo_util::shell::Verbosity::Quiet), &jdk, &[], Tool::Javac, &[]).is_none());
        }
    }
}
