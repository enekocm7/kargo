//! `kargo run`: builds and runs the program, a `[[bin]]` (`--bin`), another
//! `main` (`--main`) or an example from `examples/` (`--example`).

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
use crate::env::EnvOptions;
use crate::warnings;
use crate::web;
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
}

impl Program {
    fn flag(&self) -> &'static str {
        match self {
            Program::Default => "",
            Program::Main(_) => "--main",
            Program::Bin(_) => "--bin",
            Program::Example(_) => "--example",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub target: Option<TargetKind>,
    pub release: bool,
    pub program: Program,
    pub args: Vec<String>,
    /// The port a Wasm browser app is served on (`--port`); `None` uses [`DEFAULT_PORT`].
    pub port: Option<u16>,
    /// Open the served page in a browser (`--open`).
    pub open: bool,
    /// Start the JVM suspended, waiting for a JDWP debugger on this port (`--debug`).
    pub debug: Option<u16>,
    /// The Android flavors to install (`--flavor`), at most one per dimension.
    pub flavors: Vec<String>,
    /// The `.env` files the program's environment is read from (`--env-file`, `--no-env-file`).
    pub env: EnvOptions,
}

/// The JVM option that waits for a debugger to attach over JDWP on `port` of the loopback
/// interface before running anything. Port 0 lets the JVM pick one; it prints the port it uses.
pub fn jdwp_option(port: u16) -> String {
    format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}")
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
        Program::Example(name) => {
            example_file(ctx, name)?;
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
    // `--main` and `--example` on Kotlin/Native link their own executable instead of the project's.
    let separate_native = target == TargetKind::Native && matches!(opts.program, Program::Main(_) | Program::Example(_));
    let results = if separate_native {
        BuildResults::default()
    } else {
        build(ctx, &BuildOptions { targets: vec![target], release: opts.release, keep_going: false })?
    };

    let root = ctx.ws.root();
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut label;
    let mut cmd = match target {
        TargetKind::Jvm => {
            let jvm = results.jvm.context("JVM build produced no output")?;
            let (main, classpath) = jvm_program(ctx, &jvm, &opts.program)?;
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx, &opts.env)?)?;
            let jdk = ctx.jdk()?;
            let mut cmd = Command::new(jdk.java());
            if let Some(port) = opts.debug {
                cmd.arg(jdwp_option(port));
                note_debugger(ctx.shell(), port, "the program");
            }
            cmd.args(jdk.default_run_args()).args(&jvm.jvm_args).arg("-cp").arg(join_paths(&classpath)).arg(&main);
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
    ctx.unlock()?;
    Ok(Start::Process(Box::new(Launch { cmd, label })))
}

/// Builds and runs; returns the program's exit code.
pub fn run(ctx: &BuildContext, opts: &RunOptions) -> Result<i32> {
    match start(ctx, opts)? {
        Start::Process(launch) => {
            let status = launch.spawn(ctx.shell())?.wait().context("failed to wait for the program")?;
            Ok(status.code().unwrap_or(1))
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
}

impl Running {
    /// Stops the program that is running, if any, and starts `launch` in its place.
    pub fn replace(&mut self, launch: Launch, shell: &Shell) -> Result<()> {
        self.stop();
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
        Program::Example(name) => {
            let file = example_file(ctx, name)?;
            let main = find_entry_point_in(&file, false)?.jvm_class;
            let (classes, example_classpath) = compile_jvm_example(ctx, name, &file, &jvm.classes)?;
            classpath = vec![classes];
            classpath.extend(example_classpath);
            main
        }
    };
    Ok((main, classpath))
}

/// Compiles `examples/<name>.kt` against the main classes and the dev-dependencies into
/// `target/<profile>/jvm/examples/<name>`. Returns the output and the classpath it needs.
fn compile_jvm_example(ctx: &BuildContext, name: &str, file: &Path, main_classes: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
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
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version)?;

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(dev_cp);
    let output = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join("examples").join(name);
    // Each example is its own module, so examples may each declare `fun main` in the same package.
    let module = format!("{}-example-{name}", manifest.package.name);
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
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, "Compiling", TargetKind::Jvm, &format!(", example {name}")))?;
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
/// function (`--main`), or an example linked against the main sources as a library (`--example`).
fn build_native_program(ctx: &BuildContext, program: &Program) -> Result<PathBuf> {
    let start = Instant::now();
    let env = hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let host = Host::current()?.konan_target.to_owned();
    let setup = ctx.setup(TargetKind::Native)?;
    let main_sources = sources(ctx, TargetKind::Native, false, &["kt"])?;
    let example = match program {
        Program::Example(name) => {
            let file = example_file(ctx, name)?;
            Some((name.as_str(), find_entry_point_in(&file, true)?.native_entry(), file))
        }
        _ => None,
    };
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut libraries = native_libraries(ctx, &setup, &host, example.is_some())?;
    libraries.extend(native_cinterop(ctx, &host)?);

    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(&host);
    let unit = |name, kind, sources, libraries, entry_point, dir: PathBuf| NativeCompilation {
        name,
        konan_target: host.clone(),
        kind,
        sources,
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point,
        linker_options: &native.linker_options,
        output_dir: out.join(dir),
        debug: profile.debug,
        optimize: profile.optimize,
        test_runner: false,
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
        (_, Some((name, entry, file))) => {
            let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join("examples");
            std::fs::create_dir_all(&fp_dir)?;
            let mut libraries = libraries;
            // The main sources become a klib the example links against, as any user of the library would.
            if !main_sources.kotlin_files().is_empty() {
                let lib_dir = Path::new("examples").join("lib");
                let lib = unit(&manifest.package.name, NativeKind::Library, main_sources, libraries.clone(), None, lib_dir);
                let what = format!(", {host}");
                let lib_fp_dir = fp_dir.join("lib");
                std::fs::create_dir_all(&lib_fp_dir)?;
                lib.compile(ctx.shell(), toolchain, &lib_fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
                libraries.push(lib.artifact());
            }
            let sources = SourceSet::new(Vec::new(), vec![file], TargetKind::Native.as_str());
            let exe = unit(name, NativeKind::Executable, sources, libraries, Some(entry), PathBuf::from("examples"));
            let what = format!(", {host}, example {name}");
            exe.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            exe.artifact()
        }
        _ => unreachable!("only `--main` and `--example` link a separate executable"),
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
