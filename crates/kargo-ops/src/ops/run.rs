//! `kargo run`: builds and runs the program, a `[[bin]]` (`--bin`), another
//! `main` (`--main`), an example from `examples/` (`--example`) or any `.kt` file
//! (`kargo run <file>`), compiled against the project.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Instant;

#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::{thread, time::Duration};

use anyhow::{Context, Result, bail};

use super::build::{
    BuildOptions, BuildResults, JvmBuild, WasmBuild, announce, bin_main_class, build, compiling, find_bin, finished, hook_env,
    jvm_classpaths, jvm_javac_args, jvm_processor_path, native_cinterop, native_cinterop_defs, native_entry_point, native_libraries,
    sources,
};
use super::context::BuildContext;
use crate::compose::hot_reload::{Answer, HotReload};
use crate::env::EnvOptions;
use crate::warnings;
use crate::web;
use crate::workspace::ProfileKind;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::mainclass::find_entry_point_in;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::{SourceSet, common_kotlin_args};
use kargo_manifest::{Hook, NativeKind, Scope, TargetKind, WasmEnvironment, WasmKind};
use kargo_maven::Platform;
use kargo_toolchain::native::Host;
use kargo_util::fs::join_paths;
use kargo_util::process;
use kargo_util::shell::Shell;

/// Which program `kargo run` starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Program {
    /// The project's own: `main-class` / `entry-point`, or the single `main` in the sources.
    #[default]
    Default,
    /// `--main`: a JVM main class or a Kotlin/Native entry function.
    Main(String),
    /// `--bin`: a `[[bin]]` from kargo.toml.
    Bin(String),
    /// `--example`: `examples/<name>.kt`.
    Example(String),
    /// `kargo run <file>`: a `.kt` file anywhere, compiled against the project like an example
    /// but not part of its build.
    File(PathBuf),
}

impl Program {
    fn flag(&self) -> &'static str {
        match self {
            Program::Default => "",
            Program::Main(_) => "--main",
            Program::Bin(_) => "--bin",
            Program::Example(_) => "--example",
            Program::File(_) => "<file>",
        }
    }
}

/// A single source file `kargo run` compiles into its own program: an example or a file.
struct SingleFile {
    /// Names the program's output directory, module and Kotlin/Native executable.
    name: String,
    file: PathBuf,
    /// The directory of `target/<profile>/<target>` the program is compiled into.
    dir: &'static str,
    /// Shown after the target in the `Compiling` line.
    what: String,
}

/// The single file of `--example` and `kargo run <file>`; `None` for the other programs.
fn single_file(ctx: &BuildContext, program: &Program) -> Result<Option<SingleFile>> {
    Ok(match program {
        Program::Example(name) => {
            let file = example_file(ctx, name)?;
            Some(SingleFile { name: name.clone(), file, dir: "examples", what: format!("example {name}") })
        }
        Program::File(file) => {
            if !file.is_file() {
                bail!("{} does not exist", file.display());
            }
            let shown = file.strip_prefix(ctx.ws.root()).unwrap_or(file).display().to_string();
            Some(SingleFile { name: file_program_name(file), file: file.clone(), dir: "files", what: shown })
        }
        _ => None,
    })
}

/// The name of the program compiled from `file`: its stem, with anything but letters, digits,
/// `-` and `_` replaced, since it names a directory, a module and an executable.
fn file_program_name(file: &Path) -> String {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if name.is_empty() { "main".to_owned() } else { name }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub target: Option<TargetKind>,
    /// The profile to run: `dev`, `release` (`--release`) or a custom one (`--profile <name>`).
    pub profile: ProfileKind,
    pub program: Program,
    pub args: Vec<String>,
    /// The port a Wasm browser app is served on (`--port`); `None` uses [`DEFAULT_PORT`].
    pub port: Option<u16>,
    /// Open the served page in a browser (`--open`).
    pub open: bool,
    /// Start the JVM suspended, waiting for a JDWP debugger on this port (`--debug`).
    pub debug: Option<u16>,
    /// Run on GraalVM with its tracing agent, which writes `native-image` configuration into
    /// `native-image/` (`--native-agent`).
    pub native_agent: bool,
    /// The Android flavors to install (`--flavor`), at most one per dimension.
    pub flavors: Vec<String>,
    /// The `.env` files the program's environment is read from (`--env-file`, `--no-env-file`).
    pub env: EnvOptions,
    /// Start a Compose Desktop app with the hot reload agent (`--hot-reload`), so that
    /// [`Running::replace`] reloads its classes instead of restarting it.
    pub hot_reload: bool,
}

/// The JVM option that waits for a debugger to attach over JDWP on `port` of the loopback
/// interface before running anything. Port 0 lets the JVM pick one; it prints the port it uses.
pub fn jdwp_option(port: u16) -> String {
    format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}")
}

/// The option that runs GraalVM's tracing agent, which records the reflection, resources and
/// proxies the program uses as `native-image` configuration in `dir`. A directory that already has
/// configuration is merged into, so several runs can cover several code paths.
pub fn native_agent_option(dir: &Path) -> String {
    let existing = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    let mode = if existing { "config-merge-dir" } else { "config-output-dir" };
    format!("-agentlib:native-image-agent={mode}={}", dir.display())
}

/// Tells the user how to attach to a JVM started with [`jdwp_option`].
pub fn note_debugger(shell: &kargo_util::shell::Shell, port: u16, what: &str) {
    let port = if port == 0 { "the port it prints".to_owned() } else { format!("port {port}") };
    shell.note(format!("{what} waits for a debugger: attach one over JDWP to 127.0.0.1 on {port}"));
}

/// The port `kargo run` serves a Wasm browser app on unless told otherwise.
pub const DEFAULT_PORT: u16 = 8080;

/// Picks the target to run: explicit, else JVM, Native, Wasm, Android in that order.
pub fn runnable_target(enabled: &[TargetKind], requested: Option<TargetKind>) -> Result<TargetKind> {
    if let Some(t) = requested {
        if !enabled.contains(&t) {
            bail!("target `{t}` is not enabled in kargo.toml");
        }
        return Ok(t);
    }
    [TargetKind::Jvm, TargetKind::Native, TargetKind::Wasm, TargetKind::Android]
        .into_iter()
        .find(|t| enabled.contains(t))
        .context("no runnable target is enabled")
}

/// The source file of `examples/<name>.kt`, or an error listing the examples.
fn example_file(ctx: &BuildContext, name: &str) -> Result<PathBuf> {
    let file = ctx.ws.examples_dir().join(format!("{name}.kt"));
    if file.is_file() {
        return Ok(file);
    }
    let examples = ctx.ws.examples();
    if examples.is_empty() {
        bail!("no example named `{name}`: add examples/{name}.kt with a `main` function");
    }
    bail!("no example named `{name}` in examples/; available examples: {}", examples.join(", "))
}

/// Fails early, before anything compiles, when the selected program cannot exist.
fn validate(ctx: &BuildContext, target: TargetKind, opts: &RunOptions) -> Result<()> {
    let program = &opts.program;
    let wasm = ctx.ws.manifest().targets.wasm.as_ref().filter(|_| target == TargetKind::Wasm);
    if wasm.is_some_and(|w| w.kind == WasmKind::Library) {
        bail!(
            "[target.wasm] kind = \"library\" builds an npm package and has no program to run; \
             build it with `kargo build` and publish target/<profile>/wasm"
        );
    }
    if (opts.port.is_some() || opts.open) && !wasm.is_some_and(|w| w.environment == WasmEnvironment::Browser) {
        bail!("`--port` and `--open` serve a Wasm browser app; they need `-t wasm` with [target.wasm] environment = \"browser\"");
    }
    if !opts.flavors.is_empty() && target != TargetKind::Android {
        bail!("`--flavor` picks an Android product flavor and needs `-t android`, not {target}");
    }
    if target == TargetKind::Android {
        crate::gradle::validate_run(ctx, &opts.flavors)?;
    }
    if opts.debug.is_some() && target != TargetKind::Jvm {
        bail!("`--debug` starts the JVM under a debugger and only works for the jvm target, not {target}");
    }
    if opts.native_agent && target != TargetKind::Jvm {
        bail!("`--native-agent` traces a JVM program for `native-image` and only works for the jvm target, not {target}");
    }
    if opts.hot_reload {
        if target != TargetKind::Jvm {
            bail!("`--hot-reload` reloads a Compose Desktop app and only works for the jvm target, not {target}");
        }
        if !crate::compose::enabled(ctx.ws.manifest()) {
            bail!("`--hot-reload` reloads the composables of a Compose Desktop app; enable Compose with `[frameworks] compose = true`");
        }
        if opts.native_agent {
            bail!("`--hot-reload` runs the app on JetBrains Runtime, and `--native-agent` on GraalVM; use one of them");
        }
    }
    if *program != Program::Default && !matches!(target, TargetKind::Jvm | TargetKind::Native) {
        bail!("`kargo run {}` only works for the jvm and native targets, not {target}", program.flag());
    }
    match program {
        Program::Bin(name) => {
            let bin = find_bin(ctx.ws.manifest(), name)?;
            if target == TargetKind::Native && bin.entry_point.is_none() && bin.path.is_none() {
                bail!("[[bin]] `{name}` has no Kotlin/Native entry point; set `entry-point` or `path` in kargo.toml");
            }
            if target == TargetKind::Jvm && bin.main.is_none() && bin.path.is_none() {
                bail!("[[bin]] `{name}` has no JVM main class; set `main` or `path` in kargo.toml");
            }
        }
        Program::Example(_) | Program::File(_) => {
            single_file(ctx, program)?;
        }
        Program::Main(_) | Program::Default => {}
    }
    // An example links its own executable even when the project is a library.
    if target == TargetKind::Native
        && matches!(program, Program::Main(_) | Program::Bin(_))
        && ctx.ws.manifest().targets.native.as_ref().is_some_and(|n| n.kind != NativeKind::Executable)
    {
        bail!("the native target builds a library, not an executable");
    }
    Ok(())
}

/// The hint appended when the program to run is ambiguous and bins exist.
fn bin_hint(ctx: &BuildContext) -> String {
    let bins = &ctx.ws.manifest().bins;
    if bins.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = bins.iter().map(|b| b.name.as_str()).collect();
    format!("\nrun a [[bin]] with `kargo run --bin <name>`; available bins: {}", names.join(", "))
}

/// A built program, ready to start: its command and the label `kargo run` shows for it.
pub struct Launch {
    cmd: Command,
    label: String,
    /// Set when the program runs with the hot reload agent.
    hot_reload: Option<HotReload>,
}

impl Launch {
    /// Starts the program, printing the `Running` status line.
    pub fn spawn(mut self, shell: &Shell) -> Result<Child> {
        shell.status("Running", format!("`{}`", self.label));
        shell.verbose("Command", process::describe(&self.cmd));
        self.cmd.spawn().with_context(|| format!("failed to start `{}`", self.cmd.get_program().to_string_lossy()))
    }
}

/// What `kargo run` does once the program is built.
pub enum Start {
    /// Start this program as a child process.
    Process(Box<Launch>),
    /// The program already ran in the foreground (Android and browser apps), with this exit code.
    Finished(i32),
}

/// Builds the program and returns how to start it. Android and browser apps run in the foreground here instead.
pub fn start(ctx: &BuildContext, opts: &RunOptions) -> Result<Start> {
    let target = runnable_target(&ctx.ws.manifest().targets.enabled(), opts.target)?;
    validate(ctx, target, opts)?;
    // Fails before anything builds when a required variable is not set. Android and Wasm browser
    // apps get no environment, so they neither need nor read it.
    let project_env = if ctx.program_env_applies(target) {
        let env = ctx.project_env(&opts.env)?;
        warnings::emit(ctx.shell(), ctx.project, ctx.ws, env.tracked_secret_warnings(ctx.ws.root()));
        env.for_child()
    } else {
        Vec::new()
    };
    // `--main`, `--example` and a file on Kotlin/Native link their own executable instead of the project's.
    let separate_native = target == TargetKind::Native && matches!(opts.program, Program::Main(_) | Program::Example(_) | Program::File(_));
    let results = if separate_native {
        BuildResults::default()
    } else {
        build(ctx, &BuildOptions { targets: vec![target], profile: opts.profile.clone(), ..Default::default() })?
    };

    let root = ctx.ws.root();
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut label;
    let mut hot_reload = None;
    let mut cmd = match target {
        TargetKind::Jvm => {
            let jvm = results.jvm.context("JVM build produced no output")?;
            let (main, mut classpath) = jvm_program(ctx, &jvm, &opts.program)?;
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx, &opts.env)?)?;
            let hot_reload_state = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("hot-reload");
            if opts.hot_reload {
                // The app reads its classes from a copy that changes only here, between builds: the
                // compiler rewrites `classes/` while the app is running and loading classes lazily.
                let live = crate::compose::hot_reload::sync_classes(&jvm.classes, &hot_reload_state)?;
                for entry in classpath.iter_mut().filter(|e| **e == jvm.classes) {
                    *entry = live.clone();
                }
            }
            let mut enhanced_redefinition = false;
            let jdk = if opts.native_agent {
                let graalvm = kargo_toolchain::graalvm::resolve(ctx.config, &ctx.ws.manifest().toolchain.graalvm)?;
                kargo_toolchain::Jdk { home: graalvm.home, version: graalvm.version }
            } else if opts.hot_reload {
                let (jdk, jbr) = hot_reload_jdk(ctx)?;
                enhanced_redefinition = jbr;
                jdk
            } else {
                ctx.jdk()?.clone()
            };
            let mut cmd = Command::new(jdk.java());
            if enhanced_redefinition {
                cmd.arg(kargo_toolchain::jbr::ENHANCED_REDEFINITION);
            }
            if opts.hot_reload {
                let live = crate::compose::hot_reload::live_classes(&hot_reload_state);
                cmd.args(HotReload::agent_args(ctx.config, &jdk, &hot_reload_state, &[live])?);
                hot_reload = Some(hot_reload_state);
            }
            if let Some(port) = opts.debug {
                cmd.arg(jdwp_option(port));
                note_debugger(ctx.shell(), port, "the program");
            }
            if opts.native_agent {
                let dir = ctx.ws.root().join("native-image");
                cmd.arg(native_agent_option(&dir));
                ctx.shell().note(format!("the GraalVM tracing agent writes the program's native-image configuration to {}", rel(&dir)));
            }
            cmd.args(jdk.default_run_args());
            let manifest = ctx.ws.manifest();
            if crate::compose::enabled(manifest) {
                cmd.args(crate::compose::run_jvm_args(manifest, ctx.ws.root(), &manifest.package.name, std::env::consts::OS));
            }
            cmd.args(&jvm.jvm_args);
            let joined = join_paths(&classpath);
            if joined.len() >= process::LONG_CLASSPATH {
                // A long classpath would pass the limit of a command line on Windows (32,767 characters).
                let argfile = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("run.args");
                process::write_argfile(&argfile, &["-cp".to_owned(), process::classpath_for_argfile(&classpath)])?;
                cmd.arg(format!("@{}", process::slash(&argfile)));
            } else {
                cmd.arg("-cp").arg(joined);
            }
            cmd.arg(&main);
            label = format!("java {main}");
            cmd
        }
        TargetKind::Wasm => {
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx, &opts.env)?)?;
            let wasm = results.wasm.context("there are no Wasm sources to run")?;
            if wasm.environment == WasmEnvironment::Browser {
                return Ok(Start::Finished(serve_wasm_app(ctx, &wasm, opts)?));
            }
            let mut cmd = Command::new(kargo_toolchain::node()?);
            cmd.arg(&wasm.module);
            label = format!("node {}", rel(&wasm.module));
            cmd
        }
        TargetKind::Native => {
            let artifact =
                if separate_native { build_native_program(ctx, &opts.program)? } else { native_artifact(ctx, &results, &opts.program)? };
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx, &opts.env)?)?;
            label = rel(&artifact);
            Command::new(&artifact)
        }
        TargetKind::Android => {
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx, &opts.env)?)?;
            return Ok(Start::Finished(crate::gradle::run(ctx, &opts.flavors)?));
        }
    };
    cmd.args(&opts.args).current_dir(ctx.ws.root()).envs(project_env);
    for arg in &opts.args {
        label.push(' ');
        label.push_str(&process::quote_for_display(arg));
    }
    let hot_reload = hot_reload.map(|state| HotReload { state, command: process::describe(&cmd) });
    ctx.unlock()?;
    Ok(Start::Process(Box::new(Launch { cmd, label, hot_reload })))
}

/// Builds and runs; returns the program's exit code.
pub fn run(ctx: &BuildContext, opts: &RunOptions) -> Result<i32> {
    match start(ctx, opts)? {
        Start::Process(launch) => {
            let status = launch.spawn(ctx.shell())?.wait().context("failed to wait for the program")?;
            if status.code().is_none() {
                ctx.shell().error(format!("process {}", kargo_util::process::describe_status(status)));
            }
            Ok(kargo_util::process::exit_code_of(status))
        }
        Start::Finished(code) => Ok(code),
    }
}

/// Fails before anything builds when `kargo run --watch` cannot keep the program running, since Android and browser apps run
/// in the foreground themselves. Then checks the rest of the options as [`start`] does.
pub fn check_watchable(ctx: &BuildContext, opts: &RunOptions) -> Result<()> {
    let target = runnable_target(&ctx.ws.manifest().targets.enabled(), opts.target)?;
    let browser = ctx.ws.manifest().targets.wasm.as_ref().is_some_and(|w| w.environment == WasmEnvironment::Browser);
    if target == TargetKind::Android {
        bail!("`--watch` does not run Android apps: the app runs inside Gradle; use `kargo run -t android` instead");
    }
    if target == TargetKind::Wasm && browser {
        bail!("`--watch` does not serve browser apps yet; `kargo run` serves one until Ctrl+C");
    }
    validate(ctx, target, opts)
}

/// The program `kargo run --watch` keeps running. A build that succeeds replaces it; a build that fails leaves it running.
#[derive(Default)]
pub struct Running {
    child: Option<Child>,
    /// The hot reload agent of the running program, if it has one.
    hot_reload: Option<HotReload>,
}

impl Running {
    /// Stops the program that is running, if any, and starts `launch` in its place. A program with
    /// the hot reload agent and the same command line reloads the new classes instead, unless the agent
    /// says the change needs a restart.
    pub fn replace(&mut self, launch: Launch, shell: &Shell) -> Result<()> {
        let alive = self.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        if let (true, Some(running), Some(new)) = (alive, &self.hot_reload, &launch.hot_reload) {
            if running == new {
                match running.request() {
                    Answer::Reloaded(0) => shell.status("Reloaded", "no loaded class changed"),
                    Answer::Reloaded(n) => shell.status("Reloaded", format!("{n} class{}", if n == 1 { "" } else { "es" })),
                    Answer::Restart(reason) => {
                        shell.note(format!("restarting the app, as the change cannot be reloaded: {reason}"));
                        return self.restart(launch, shell);
                    }
                }
                return Ok(());
            }
            shell.note("restarting the app, as its command line changed");
        }
        self.restart(launch, shell)
    }

    fn restart(&mut self, launch: Launch, shell: &Shell) -> Result<()> {
        self.stop();
        self.hot_reload = launch.hot_reload.clone();
        self.child = Some(launch.spawn(shell)?);
        Ok(())
    }

    /// Stops the program that is running, if any.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate(&mut child);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The JDK `--hot-reload` runs the app on: JetBrains Runtime of the project's Java version, whose
/// enhanced class redefinition reloads new functions and fields too. Without one (offline, or no
/// download for this platform) the project's JDK, which reloads changed function bodies only.
/// Returns the JDK and whether it is JetBrains Runtime.
fn hot_reload_jdk(ctx: &BuildContext) -> Result<(kargo_toolchain::Jdk, bool)> {
    let jdk = ctx.jdk()?.clone();
    match kargo_toolchain::jbr::resolve(ctx.config, jdk.version) {
        Ok(jbr) => Ok((jbr, true)),
        Err(e) => {
            ctx.shell().warn(format!(
                "hot reload runs on {} instead of JetBrains Runtime, so only changes inside function bodies reload: {e:#}",
                jdk.home.display()
            ));
            Ok((jdk, false))
        }
    }
}

/// How long a program has to exit after SIGTERM before it is killed.
#[cfg(unix)]
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Stops `child`. On Unix it gets SIGTERM first, so a server can close its sockets and run its shutdown hooks. Windows has no
/// such signal for a console program, so there the program is killed at once.
fn terminate(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        if signal_and_wait(child, STOP_TIMEOUT) {
            return;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Sends SIGTERM to `child` and waits up to `timeout` for it to exit. Returns whether it did.
#[cfg(unix)]
fn signal_and_wait(child: &mut Child, timeout: Duration) -> bool {
    let _ = Command::new("kill").arg("-TERM").arg(child.id().to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Serves a Wasm browser app until the process is stopped (Ctrl+C).
fn serve_wasm_app(ctx: &BuildContext, wasm: &WasmBuild, opts: &RunOptions) -> Result<i32> {
    let listener = web::bind(opts.port.unwrap_or(DEFAULT_PORT))?;
    let mut site = web::Site::new(&wasm.dir);
    if let Some(node_modules) = &wasm.node_modules {
        site = site.mount("node_modules", node_modules);
    }
    // Other `kargo` commands may build while the page is being served.
    ctx.unlock()?;
    let server = web::Server::start(listener, site, None)?;
    let dir = wasm.dir.strip_prefix(ctx.ws.root()).unwrap_or(&wasm.dir);
    ctx.shell().status("Serving", format!("{} at {} (press Ctrl+C to stop)", dir.display(), server.url()));
    if opts.open {
        web::open_in_browser(std::ffi::OsStr::new(&server.url()))?;
    }
    server.wait();
    Ok(0)
}

/// The main class and classpath of the JVM program to run.
fn jvm_program(ctx: &BuildContext, jvm: &JvmBuild, program: &Program) -> Result<(String, Vec<PathBuf>)> {
    let mut classpath = vec![jvm.classes.clone()];
    classpath.extend(jvm.runtime_classpath.iter().cloned());
    let main = match program {
        Program::Default => match &jvm.main_class {
            Some(main) => main.clone(),
            None => {
                let reason = jvm.main_class_error.as_deref().unwrap_or("set `[target.jvm] main-class` in kargo.toml");
                bail!("could not determine the main class: {reason}{}", bin_hint(ctx));
            }
        },
        Program::Main(main) => main.clone(),
        Program::Bin(name) => bin_main_class(ctx.ws.root(), find_bin(ctx.ws.manifest(), name)?)?,
        Program::Example(_) | Program::File(_) => {
            let single = single_file(ctx, program)?.expect("an example or a file");
            let main = find_entry_point_in(&single.file, false)?.jvm_class;
            let (classes, single_classpath) = compile_jvm_single(ctx, &single, &jvm.classes)?;
            classpath = vec![classes];
            classpath.extend(single_classpath);
            main
        }
    };
    Ok((main, classpath))
}

/// Compiles an example or a file against the main classes and the dev-dependencies into
/// `target/<profile>/jvm/<examples|files>/<name>`. Returns the output and the classpath it needs.
fn compile_jvm_single(ctx: &BuildContext, single: &SingleFile, main_classes: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let SingleFile { name, file, dir, what } = single;
    let manifest = ctx.ws.manifest();
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut dev_cp = super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &all_scopes, true)?;
    dev_cp.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version)?;

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(dev_cp);
    let output = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join(dir).join(name);
    // Each example or file is its own module, so they may each declare `fun main` in the same package.
    let module = format!("{}-{}-{name}", manifest.package.name, dir.trim_end_matches('s'));
    let unit = JvmCompilation {
        name: &module,
        sources: SourceSet::new(Vec::new(), vec![file.to_path_buf()], TargetKind::Jvm.as_str()),
        classpath: classpath.clone(),
        friend_paths: Vec::new(),
        output: output.clone(),
        jvm_target: &jvm_target,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &javac_args,
        processor_path: &processor_path,
        resources: Vec::new(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, "Compiling", TargetKind::Jvm, &format!(", {what}")))?;
    Ok((output, classpath))
}

/// The host executable of the project or of a `[[bin]]`, from a finished build.
fn native_artifact(ctx: &BuildContext, results: &BuildResults, program: &Program) -> Result<PathBuf> {
    let host = Host::current()?.konan_target;
    let for_host: Vec<_> = results.native.iter().filter(|b| b.konan_target == host).collect();
    if for_host.is_empty() {
        bail!("the native target has no build for this host ({host}); add \"host\" to `platforms`");
    }
    if for_host.iter().any(|b| b.kind != NativeKind::Executable) {
        bail!("the native target builds a library, not an executable");
    }
    let bin = match program {
        Program::Bin(name) => Some(name.as_str()),
        _ => None,
    };
    if let Some(build) = for_host.iter().find(|b| b.bin.as_deref() == bin) {
        return Ok(build.artifact.clone());
    }
    // The project's own executable is skipped when its `main` is ambiguous; say why.
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    let reason = native_entry_point(native, &sources(ctx, TargetKind::Native, false, &["kt"])?).err().map(|e| e.to_string());
    bail!("could not determine the entry point: {}{}", reason.unwrap_or_else(|| "no executable was built".to_owned()), bin_hint(ctx))
}

/// Links a host executable outside `kargo build`: the main sources with another entry
/// function (`--main`), or an example or a file linked against the main sources as a library
/// (`--example`, `kargo run <file>`).
fn build_native_program(ctx: &BuildContext, program: &Program) -> Result<PathBuf> {
    let start = Instant::now();
    super::build::prepare_native_hooks(ctx, &[TargetKind::Native])?;
    let env = hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    let manifest = ctx.ws.manifest();
    manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let host = Host::current()?.konan_target.to_owned();
    let setup = ctx.setup(TargetKind::Native)?;
    let main_sources = sources(ctx, TargetKind::Native, false, &["kt"])?;
    let example = match single_file(ctx, program)? {
        Some(single) => {
            let entry = find_entry_point_in(&single.file, true)?.native_entry();
            Some((single, entry))
        }
        None => None,
    };
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut libraries = native_libraries(ctx, &setup, &host, example.is_some())?;
    libraries.extend(native_cinterop(ctx, &host)?);
    let link = crate::native_interop::native_link(ctx, &host)?;

    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(&host);
    let unit = |name, kind, sources, libraries, entry_point, dir: PathBuf| NativeCompilation {
        name,
        konan_target: host.clone(),
        kind,
        sources,
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point,
        link: link.clone(),
        output_dir: out.join(dir),
        debug: profile.debug,
        optimize: profile.optimize,
        test_runner: false,
        exported: Vec::new(),
        bundle_id: None,
    };
    let artifact = match (program, example) {
        (Program::Main(entry), _) => {
            let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join("main");
            std::fs::create_dir_all(&fp_dir)?;
            let exe =
                unit(&manifest.package.name, NativeKind::Executable, main_sources, libraries, Some(entry.clone()), PathBuf::from("main"));
            let what = format!(", {host}, main {entry}");
            exe.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            exe.artifact()
        }
        (_, Some((single, entry))) => {
            let SingleFile { name, file, dir, what } = single;
            let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join(dir);
            std::fs::create_dir_all(&fp_dir)?;
            let mut libraries = libraries;
            // The main sources become a klib the example links against, as any user of the library would.
            if !main_sources.kotlin_files().is_empty() {
                let lib_dir = Path::new(dir).join("lib");
                let lib = unit(&manifest.package.name, NativeKind::Library, main_sources, libraries.clone(), None, lib_dir);
                let what = format!(", {host}");
                let lib_fp_dir = fp_dir.join("lib");
                std::fs::create_dir_all(&lib_fp_dir)?;
                lib.compile(ctx.shell(), toolchain, &lib_fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
                libraries.push(lib.artifact());
            }
            let sources = SourceSet::new(Vec::new(), vec![file], TargetKind::Native.as_str());
            let exe = unit(&name, NativeKind::Executable, sources, libraries, Some(entry), PathBuf::from(dir));
            let what = format!(", {host}, {what}");
            exe.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            exe.artifact()
        }
        _ => unreachable!("only `--main`, `--example` and a file link a separate executable"),
    };
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostBuild, &env)?;
    finished(ctx, start);
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_preference() {
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], None).unwrap(), TargetKind::Jvm);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Native], None).unwrap(), TargetKind::Native);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], Some(TargetKind::Wasm)).unwrap(), TargetKind::Wasm);
        assert!(runnable_target(&[TargetKind::Jvm], Some(TargetKind::Native)).is_err());
        assert!(runnable_target(&[], None).is_err());
    }

    #[test]
    fn file_program_names() {
        assert_eq!(file_program_name(Path::new("tools/migrate.kt")), "migrate");
        assert_eq!(file_program_name(Path::new("my tool.main.kt")), "my_tool_main");
    }

    #[test]
    fn debugger_listens_on_loopback() {
        assert_eq!(jdwp_option(5005), "-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:5005");
    }

    #[cfg(unix)]
    #[test]
    fn a_program_is_stopped_with_sigterm_before_it_is_killed() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let start = Instant::now();
        terminate(&mut child);
        assert!(start.elapsed() < STOP_TIMEOUT, "the program was not stopped by SIGTERM");
        assert!(child.try_wait().unwrap().is_some_and(|status| !status.success()));
    }
}
