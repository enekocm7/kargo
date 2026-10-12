//! `kargo test`: compiles and runs tests for each target, or lists them (`--list`).
//! With `--message-format json` each test's start and result is also a JSON message
//! on stdout, and the human-readable report moves to stderr.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anstyle::AnsiColor;
use anyhow::{Context, Result};

use super::build::{
    BuildOptions, JvmKspUnit, announce, build_jvm, compiling, finished, hook_env, jvm_classpaths, jvm_javac_args, jvm_processor_path,
    native_cinterop, native_libraries, native_platforms, prepare_wasm, run_jvm_ksp, select_targets, sources, wasm_libraries,
};
use super::context::BuildContext;
use crate::builtins::JUNIT_VERSION;
use crate::warnings;
use crate::web::{self, PageEvent, Site};
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::testreport::{self, TestEvent};
use kargo_compiler::wasm::{WasmCompilation, npm_dependencies};
use kargo_compiler::{Outcome, common_kotlin_args};
use kargo_compiler::{diagnostics, junit};
use kargo_manifest::{Hook, NativeKind, Scope, TargetKind, WasmEnvironment};
use kargo_maven::Platform;
use kargo_toolchain::browser::{self, Browser};
use kargo_toolchain::native::Host;
use kargo_util::fs::join_paths;
use kargo_util::process;

#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    pub build: BuildOptions,
    /// Only run tests whose name contains this string.
    pub filter: Option<String>,
    /// Compile tests without running them.
    pub no_run: bool,
    /// Keep testing the remaining targets after one fails (`--no-fail-fast`),
    /// overriding `[test] fail-fast` in `kargo.toml`.
    pub no_fail_fast: bool,
    /// Write JUnit XML reports to `target/<profile>/<target>/test-reports` (`--report junit`).
    pub junit_report: bool,
    /// Start the JVM tests suspended, waiting for a JDWP debugger on this port (`--debug`).
    pub debug: Option<u16>,
    /// Print the tests instead of running them (`--list`).
    pub list: Option<ListFormat>,
}

/// How `kargo test --list` prints the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListFormat {
    /// `<target>: <Class>.<name>` per test.
    #[default]
    Text,
    /// One `test-discovered` JSON message per test.
    Json,
}

/// What happened to one target in a `kargo test` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetOutcome {
    Passed,
    /// Its tests ran and at least one failed.
    Failed,
    /// Its tests could not be compiled or started.
    Error,
    /// Not run because an earlier target failed.
    Skipped,
}

/// Runs the tests; returns `true` when everything passed.
pub fn test(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
    if let Some(format) = opts.list {
        list(ctx, opts, format)?;
        return Ok(true);
    }
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.build.targets)?;
    if opts.debug.is_some() && targets != [TargetKind::Jvm] {
        anyhow::bail!("`--debug` runs the JVM tests under a debugger; select the JVM target with `-t jvm`");
    }
    let env = hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreTest, &env)?;
    // A project may test code that lives elsewhere, so missing main sources are not worth a warning here.
    warnings::emit(ctx.shell(), ctx.project, ctx.ws, warnings::ignored_files(ctx.ws));

    // Compile every target's tests at the same time, then run them one target at a
    // time so their output doesn't interleave.
    let prepared = super::parallel_map(&targets, |&target| match target {
        TargetKind::Jvm => test_jvm(ctx, opts),
        TargetKind::Wasm => test_wasm(ctx, opts),
        TargetKind::Native => test_native(ctx, opts),
        TargetKind::Android => Ok(TestRun::Android),
    });
    let fail_fast = !opts.no_fail_fast && ctx.ws.manifest().test.fail_fast;
    let mut outcomes = Vec::new();
    // In fail-fast mode an error ends the run once the skipped targets are listed.
    let mut fatal = None;
    for (&target, run) in targets.iter().zip(prepared) {
        if outcomes.iter().any(|(_, o)| *o != TargetOutcome::Passed) && fail_fast {
            outcomes.push((target, TargetOutcome::Skipped));
            continue;
        }
        let passed = run.and_then(|run| match run {
            TestRun::Done(passed) => Ok(passed),
            TestRun::Junit(cmd) => run_junit(ctx, cmd, opts),
            TestRun::Reported(cmd, label) => run_reported(ctx, cmd, label, target, opts),
            TestRun::Browser(run) => run_in_browser(ctx, run, opts),
            TestRun::Android => test_android(ctx, opts),
        });
        let outcome = match passed {
            Ok(true) => TargetOutcome::Passed,
            Ok(false) => TargetOutcome::Failed,
            Err(e) if fail_fast || targets.len() == 1 => {
                fatal = Some(e);
                TargetOutcome::Error
            }
            Err(e) => {
                ctx.shell().error(format!("could not test the {target} target: {e:#}"));
                TargetOutcome::Error
            }
        };
        outcomes.push((target, outcome));
    }
    let skipped: Vec<&str> = outcomes.iter().filter(|(_, o)| *o == TargetOutcome::Skipped).map(|(t, _)| t.as_str()).collect();
    if !skipped.is_empty() {
        ctx.shell().note(format!(
            "skipped the {} target{} after a failure; pass `--no-fail-fast` to test every target",
            skipped.join(", "),
            if skipped.len() == 1 { "" } else { "s" }
        ));
    } else if outcomes.len() > 1 && !fail_fast {
        print_target_table(&outcomes, human_out())?;
    }
    if let Some(e) = fatal {
        return Err(e);
    }
    let ok = outcomes.iter().all(|(_, o)| *o == TargetOutcome::Passed);
    let mut env = env;
    env.push(("KARGO_TEST_RESULT".to_owned(), if ok { "success" } else { "failure" }.to_owned()));
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostTest, &env)?;
    if opts.no_run {
        finished(ctx, start);
    }
    Ok(ok)
}

/// Where the human-readable test report goes: stdout, or stderr when stdout carries
/// JSON messages (`--message-format json`).
pub(crate) fn human_out() -> Box<dyn Write> {
    if diagnostics::is_json() { Box::new(anstream::stderr()) } else { Box::new(anstream::stdout()) }
}

/// Prints a test message as a JSON line, adding the package and target.
pub(crate) fn emit_event(package: &str, target: TargetKind, mut message: serde_json::Map<String, serde_json::Value>) {
    message.insert("package".to_owned(), package.into());
    message.insert("target".to_owned(), target.as_str().into());
    diagnostics::emit(&serde_json::Value::Object(message));
}

/// [`emit_event`] for a test read from TeamCity messages or a JUnit XML report.
pub(crate) fn emit_report_event(package: &str, target: TargetKind, event: TestEvent<'_>) {
    if let serde_json::Value::Object(message) = testreport::event_json(event) {
        emit_event(package, target, message);
    }
}

/// With `--message-format json`, prints the `test-summary` message that ends a target's results.
pub(crate) fn emit_summary(ctx: &BuildContext, target: TargetKind, passed: usize, failed: usize, ignored: usize, success: bool) {
    if !diagnostics::is_json() {
        return;
    }
    let json = serde_json::json!({ "reason": "test-summary", "passed": passed, "failed": failed, "ignored": ignored, "success": success });
    if let serde_json::Value::Object(message) = json {
        emit_event(&ctx.ws.manifest().package.name, target, message);
    }
}

/// Prints one line per target with how its tests went, after a `--no-fail-fast` run.
fn print_target_table(outcomes: &[(TargetKind, TargetOutcome)], mut out: impl std::io::Write) -> std::io::Result<()> {
    writeln!(
        out,
        "
test results by target:"
    )?;
    for (target, outcome) in outcomes {
        let (style, text) = match outcome {
            TargetOutcome::Passed => (AnsiColor::Green.on_default(), "ok"),
            TargetOutcome::Failed => (AnsiColor::Red.on_default(), "FAILED"),
            TargetOutcome::Error => (AnsiColor::Red.on_default(), "error (could not compile or run the tests)"),
            TargetOutcome::Skipped => (AnsiColor::Yellow.on_default(), "skipped"),
        };
        writeln!(out, "    {:<8} {style}{text}{style:#}", target.as_str())?;
    }
    Ok(())
}

/// A target's compiled tests, ready to run.
enum TestRun {
    /// Nothing (left) to run; whether the target passed.
    Done(bool),
    /// The JUnit console launcher.
    Junit(Command),
    /// A test executable reporting TeamCity messages, with its label.
    Reported(Command, &'static str),
    /// Kotlin/Wasm tests in a headless browser, which reports TeamCity messages to Kargo.
    Browser(BrowserRun),
    /// Gradle compiles and runs Android tests in one step.
    Android,
}

/// Kotlin/Wasm tests that run in a browser.
struct BrowserRun {
    browser: Browser,
    /// The test page and the module it loads, and the npm packages it imports.
    site: Site,
    /// Where the browser keeps its state while it runs.
    profile: PathBuf,
}

fn no_tests(ctx: &BuildContext, target: TargetKind) -> TestRun {
    ctx.shell().note(format!("no tests found for the {target} target (put them in tests/ or tests-{target}/)"));
    TestRun::Done(true)
}

fn test_android(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
    if sources(ctx, TargetKind::Android, true, &["kt", "java"]).is_empty() {
        no_tests(ctx, TargetKind::Android);
        return Ok(true);
    }
    let reports = (opts.junit_report && !opts.no_run).then(|| reports_dir(ctx, TargetKind::Android)).transpose()?;
    let passed = crate::gradle::test(ctx, opts.filter.as_deref(), opts.no_run, reports.as_deref())?;
    if let Some(dir) = &reports {
        wrote_reports(ctx, dir);
    }
    Ok(passed)
}

/// Clears and returns `target/<profile>/<target>/test-reports`, where `--report junit` writes.
fn reports_dir(ctx: &BuildContext, target: TargetKind) -> Result<PathBuf> {
    let dir = ctx.ws.output_dir(ctx.profile, target).join("test-reports");
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn wrote_reports(ctx: &BuildContext, dir: &Path) {
    let shown = dir.strip_prefix(ctx.ws.root()).unwrap_or(dir);
    ctx.shell().status("Reported", format!("JUnit XML in {}", shown.display()));
}

fn test_jvm(ctx: &BuildContext, opts: &TestOptions) -> Result<TestRun> {
    let main = build_jvm(ctx)?;
    let Some(JvmTestClasses { classes: test_classes, classpath }) = compile_jvm_tests(ctx, &main.classes, "Compiling")? else {
        return Ok(no_tests(ctx, TargetKind::Jvm));
    };
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    let mode = if diagnostics::is_json() { JunitMode::Events } else { JunitMode::Run };
    Ok(TestRun::Junit(junit_command(ctx, opts, &JvmTestClasses { classes: test_classes, classpath }, mode)?))
}

/// What the JUnit console launcher is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JunitMode {
    /// Run the tests, printing a tree of results.
    Run,
    /// Run the tests, with Kargo's listener reporting each one (`--message-format json`).
    Events,
    /// List the tests without running them (`--list`).
    List,
}

/// The JUnit console launcher command that runs (or lists) the compiled tests.
fn junit_command(ctx: &BuildContext, opts: &TestOptions, tests: &JvmTestClasses, mode: JunitMode) -> Result<Command> {
    let jdk = ctx.compiler_jdk()?;
    let jvm = ctx.ws.manifest().targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let launcher = ctx.tool_jar("org.junit.platform", "junit-platform-console-standalone", JUNIT_VERSION)?;
    let mut run_cp = vec![tests.classes.clone()];
    run_cp.extend(tests.classpath.iter().cloned());
    if mode != JunitMode::Run {
        // The launcher registers the listener through its `META-INF/services` entry.
        let cache = ctx.config.home().join("cache").join("junit");
        run_cp.push(junit::listener(&cache, jdk, &launcher).context("failed to build Kargo's JUnit listener")?);
    }
    let mut cmd = Command::new(jdk.java());
    if let Some(port) = opts.debug {
        cmd.arg(super::run::jdwp_option(port));
    }
    // The launcher loads the tests in its own class loader, so its JVM classpath is just the launcher.
    cmd.args(kargo_compiler::daemon::cds_options(jdk, "junit", std::slice::from_ref(&launcher))).args(&jvm.jvm_args);
    if mode == JunitMode::List {
        cmd.arg(format!("-D{}=true", junit::LIST_PROPERTY));
    }
    let colors = mode == JunitMode::Run && std::io::IsTerminal::is_terminal(&std::io::stdout());
    cmd.arg("-jar")
        .arg(&launcher)
        .arg("execute")
        .arg("--disable-banner")
        .arg(if mode == JunitMode::List { "--details=none" } else { "--details=tree" })
        .args((!colors).then_some("--disable-ansi-colors"))
        .arg("--class-path")
        .arg(join_paths(&run_cp))
        .arg("--scan-class-path")
        .arg(&tests.classes)
        .args(junit_filter_args(opts.filter.as_deref(), &test_class_names(&tests.classes)))
        .current_dir(ctx.ws.root());
    match mode {
        JunitMode::Run => {
            cmd.arg("--fail-if-no-tests");
        }
        JunitMode::Events => {
            // Captured output reaches the listener, which reports it with the test.
            cmd.arg("--fail-if-no-tests")
                .arg("--config=junit.platform.output.capture.stdout=true")
                .arg("--config=junit.platform.output.capture.stderr=true");
        }
        JunitMode::List => {
            cmd.arg("--config=junit.platform.execution.dryRun.enabled=true");
        }
    }
    Ok(cmd)
}

fn run_junit(ctx: &BuildContext, mut cmd: Command, opts: &TestOptions) -> Result<bool> {
    let reports = opts.junit_report.then(|| reports_dir(ctx, TargetKind::Jvm)).transpose()?;
    if let Some(dir) = &reports {
        // The launcher writes `TEST-junit-jupiter.xml` (one file per test engine).
        cmd.arg("--reports-dir").arg(dir);
    }
    ctx.shell().status("Running", "JVM tests (JUnit Platform)");
    if let Some(port) = opts.debug {
        super::run::note_debugger(ctx.shell(), port, "the test JVM");
    }
    let status = if diagnostics::is_json() {
        run_junit_events(ctx, cmd)?
    } else {
        process::status(ctx.shell(), &mut cmd, "the JUnit console launcher")?
    };
    if let Some(dir) = &reports {
        wrote_reports(ctx, dir);
    }
    let filter = opts.filter.as_deref();
    if status.code() == Some(JUNIT_NO_TESTS) {
        match filter {
            Some(filter) => ctx.shell().error(format!("no JVM tests match `{filter}`; the filter matches `package.Class.method` names")),
            None => ctx.shell().error(
                "no JVM tests found; mark test functions with `@Test` (kotlin.test or JUnit) or add your test framework's \
                 JUnit Platform engine to [dev-dependencies]",
            ),
        }
    }
    Ok(status.success())
}

/// Runs the launcher with Kargo's listener, passing each test message on with the package
/// and target added, and the launcher's own output to stderr.
fn run_junit_events(ctx: &BuildContext, mut cmd: Command) -> Result<std::process::ExitStatus> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.stdout(Stdio::piped()).spawn().context("failed to start the JUnit console launcher")?;
    let stdout = child.stdout.take().context("missing test stdout")?;
    let package = &ctx.ws.manifest().package.name;
    let (mut passed, mut failed, mut ignored) = (0, 0, 0);
    let mut err = anstream::stderr();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        match junit::parse_event(&line) {
            Some(message) => {
                if message.get("reason").and_then(|r| r.as_str()) == Some("test-finished") {
                    match message.get("outcome").and_then(|o| o.as_str()) {
                        Some("passed") => passed += 1,
                        Some("ignored") => ignored += 1,
                        _ => failed += 1,
                    }
                }
                emit_event(package, TargetKind::Jvm, message);
            }
            // The listener starts each message on a new line, which leaves blank lines behind.
            None if line.trim().is_empty() => {}
            None => writeln!(err, "{line}")?,
        }
    }
    let status = child.wait()?;
    emit_summary(ctx, TargetKind::Jvm, passed, failed, ignored, status.success());
    Ok(status)
}

/// Compiled JVM test classes and the classpath they were compiled against.
pub(crate) struct JvmTestClasses {
    pub classes: PathBuf,
    /// Main classes followed by the test classpath.
    pub classpath: Vec<PathBuf>,
}

/// Compiles the JVM tests against `main_classes` into `target/<profile>/jvm/test-classes`.
/// Returns `None` when there are no test sources.
pub(crate) fn compile_jvm_tests(ctx: &BuildContext, main_classes: &Path, verb: &str) -> Result<Option<JvmTestClasses>> {
    let manifest = ctx.ws.manifest();
    let test_sources = sources(ctx, TargetKind::Jvm, true, &["kt", "java"]);
    if test_sources.is_empty() {
        return Ok(None);
    }
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut test_cp = super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &all_scopes, true)?;
    test_cp.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let test_classes = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join("test-classes");
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version)?;

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(test_cp);
    let name = format!("{}-test", manifest.package.name);
    let mut test_sources = test_sources;
    let mut resources = vec![ctx.ws.test_resources()];
    let ksp = JvmKspUnit {
        name: &name,
        test: true,
        libraries: classpath.clone(),
        friends: vec![main_classes.to_path_buf()],
        jdk,
        jvm_target: &jvm_target,
    };
    run_jvm_ksp(ctx, &setup, ksp, &mut test_sources, &mut resources)?;
    let unit = JvmCompilation {
        name: &name,
        sources: test_sources,
        classpath: classpath.clone(),
        friend_paths: vec![main_classes.to_path_buf()],
        output: test_classes.clone(),
        jvm_target: &jvm_target,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &javac_args,
        processor_path: &processor_path,
        resources,
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, verb, TargetKind::Jvm, ", tests"))?;
    Ok(Some(JvmTestClasses { classes: test_classes, classpath }))
}

/// Exit code of the JUnit console launcher when `--fail-if-no-tests` found nothing to run.
const JUNIT_NO_TESTS: i32 = 2;

/// The JUnit console launcher arguments that select the tests to run.
///
/// Without a filter every class is scanned: JUnit's default only includes
/// classes named `Test*`, `*Test` or `*Tests`, which silently skips a class
/// such as `KSpec`. A filter matches tests whose `package.Class.method`
/// contains it (`#` may separate class and method). When it names a test
/// class, a class filter is used, which works for every test engine
/// (Kotest included); otherwise it is matched against method names, which
/// needs a method-based engine such as JUnit Jupiter (used by kotlin.test).
fn junit_filter_args(filter: Option<&str>, classes: &[String]) -> Vec<String> {
    let all_classes = "--include-classname=.*".to_owned();
    let Some(filter) = filter else {
        return vec![all_classes];
    };
    let filter = filter.replace('#', ".");
    if classes.iter().any(|c| c.replace('$', ".").contains(&filter)) {
        vec![format!("--include-classname=.*{}.*", filter_regex(&filter, "[.$]"))]
    } else {
        vec![all_classes, format!("--include-methodname=.*{}.*", filter_regex(&filter, "[.$#]"))]
    }
}

/// Escapes `filter` as a Java regex in which each `.` matches any of `separators`.
fn filter_regex(filter: &str, separators: &str) -> String {
    filter.split('.').map(regex_escape).collect::<Vec<_>>().join(separators)
}

/// The binary names (`pkg.Outer$Inner`) of the classes compiled into `dir`,
/// leaving out lambdas and other synthetic classes.
fn test_class_names(dir: &Path) -> Vec<String> {
    kargo_util::fs::collect_files(dir, &["class"])
        .iter()
        .filter_map(|file| {
            let name = process::slash(&file.strip_prefix(dir).ok()?.with_extension("")).replace('/', ".");
            let named = name.split('$').skip(1).all(|part| part.starts_with(char::is_uppercase));
            named.then_some(name)
        })
        .collect()
}

fn regex_escape(s: &str) -> String {
    s.chars().flat_map(|c| if "\\.+*?()|[]{}^$".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// Runs a test executable whose stdout carries TeamCity messages.
fn run_reported(ctx: &BuildContext, mut cmd: Command, label: &str, target: TargetKind, opts: &TestOptions) -> Result<bool> {
    ctx.shell().status("Running", label);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.stdout(Stdio::piped()).spawn().with_context(|| format!("failed to start {label}"))?;
    let stdout = child.stdout.take().context("missing test stdout")?;
    let package = ctx.ws.manifest().package.name.clone();
    let json = diagnostics::is_json();
    let summary = testreport::report_with(BufReader::new(stdout), human_out(), |event| {
        if json {
            emit_report_event(&package, target, event);
        }
    })?;
    let status = child.wait()?;
    // Runners exit non-zero when a test failed; without a failed test that means a crash.
    let abnormal = (!status.success()).then(|| {
        let how = format!("exited with {}", process::describe_status(status));
        if summary.ran_tests() { how } else { format!("{how} before running any tests") }
    });
    finish_reported(ctx, summary, label, target, opts, abnormal)
}

/// Writes the reports and prints the summary of a test run. `abnormal` says how the runner
/// ended when it did not finish normally (`exited with exit code 1`); that only counts as a crash
/// when no test failed, since runners also end that way when a test failed.
fn finish_reported(
    ctx: &BuildContext,
    mut summary: testreport::TestSummary,
    label: &str,
    target: TargetKind,
    opts: &TestOptions,
    abnormal: Option<String>,
) -> Result<bool> {
    if opts.junit_report {
        let dir = reports_dir(ctx, target)?;
        let file = dir.join(format!("TEST-{target}.xml"));
        let package = &ctx.ws.manifest().package.name;
        testreport::write_junit_xml(&file, &format!("{package} ({target})"), &summary.cases)
            .with_context(|| format!("failed to write {}", file.display()))?;
        wrote_reports(ctx, &dir);
    }
    summary.crashed = abnormal.is_some() && summary.failed == 0;
    emit_summary(ctx, target, summary.passed, summary.failed, summary.ignored, summary.success());
    if summary.crashed && !summary.ran_tests() {
        ctx.shell().error(format!("{label} {}", abnormal.unwrap_or_default()));
        return Ok(false);
    }
    testreport::print_summary(&summary, human_out())?;
    if summary.crashed {
        ctx.shell().error(format!("{label} {}", abnormal.unwrap_or_default()));
    }
    Ok(summary.success())
}

/// How long a browser may stay silent before its tests count as hung (`KARGO_BROWSER_TIMEOUT`, in seconds).
fn browser_timeout() -> Duration {
    let seconds = std::env::var(browser::BROWSER_TIMEOUT_ENV).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(120);
    Duration::from_secs(seconds)
}

/// After the entry point returned and every suite ended, how long to wait for another suite to
/// start before the run counts as finished (asynchronous tests may start the next suite late).
const SETTLE: Duration = Duration::from_millis(400);

/// Runs the tests in a headless browser: serves the test page, starts the browser on it and
/// turns the console output the page reports into the usual test report.
fn run_in_browser(ctx: &BuildContext, run: BrowserRun, opts: &TestOptions) -> Result<bool> {
    let (events_tx, events) = mpsc::channel();
    let server = web::Server::start(web::bind(0)?, run.site, Some(events_tx))?;
    let label = "Wasm tests (headless browser)";
    ctx.shell().status("Running", label);
    let mut cmd = run.browser.headless_command(&format!("{}index.html", server.url()), &run.profile);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    run.browser.prepare_profile(&run.profile)?;
    let mut child = cmd.spawn().with_context(|| format!("failed to start {}", run.browser.exe.display()))?;

    // The report is read on its own thread so tests print as they finish.
    let (lines_tx, lines) = mpsc::channel::<String>();
    let package = ctx.ws.manifest().package.name.clone();
    let json = diagnostics::is_json();
    let reporter = std::thread::spawn(move || {
        testreport::report_with(BufReader::new(ChannelReader::new(lines)), human_out(), |event| {
            if json {
                emit_report_event(&package, TargetKind::Wasm, event);
            }
        })
    });
    let outcome = wait_for_page(&mut child, &events, &lines_tx, browser_timeout());
    drop(lines_tx);
    let summary = reporter.join().map_err(|_| anyhow::anyhow!("the test reporter panicked"))??;

    // Ending the process stops most browsers. One that may have handed over to another process
    // is asked to close its window instead, and given time to do so.
    server.ask_to_quit();
    let quits = run.browser.needs_to_be_asked_to_quit();
    if quits {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(server);
    // The browser may still hold files in its profile for a moment.
    for _ in 0..if quits { 50 } else { 10 } {
        if kargo_util::fs::remove_dir_all_if_exists(&run.profile).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    finish_reported(ctx, summary, label, TargetKind::Wasm, opts, outcome.err())
}

/// What a test page has reported so far.
struct PageProgress {
    /// Suites started and not finished yet.
    open_suites: usize,
    /// The entry point returned (asynchronous tests may still be running).
    returned: bool,
    last_event: Instant,
}

impl PageProgress {
    fn new() -> Self {
        Self { open_suites: 0, returned: false, last_event: Instant::now() }
    }

    /// Handles one report, forwarding test output to `lines`. `Some` ends the wait: how the page failed.
    fn apply(&mut self, event: PageEvent, lines: &mpsc::Sender<String>) -> Option<String> {
        self.last_event = Instant::now();
        match event {
            PageEvent::Stdout(text) => {
                for line in text.lines() {
                    match testreport::parse_message(line).map(|(name, _)| name).as_deref() {
                        Some("testSuiteStarted") => self.open_suites += 1,
                        Some("testSuiteFinished") => self.open_suites = self.open_suites.saturating_sub(1),
                        _ => {}
                    }
                }
                let _ = lines.send(text);
            }
            PageEvent::Stderr(text) => anstream::eprint!("{text}"),
            PageEvent::Returned => self.returned = true,
            PageEvent::Failed(message) => return Some(format!("failed: {}", message.trim())),
        }
        None
    }

    /// Whether every test has run: the entry point returned, no suite is open and none has started lately.
    fn done(&self) -> bool {
        self.returned && self.open_suites == 0 && self.last_event.elapsed() >= SETTLE
    }
}

/// Forwards the page's test output to `lines` until its tests are done; the error says why they are not.
fn wait_for_page(
    child: &mut std::process::Child,
    events: &Receiver<PageEvent>,
    lines: &mpsc::Sender<String>,
    timeout: Duration,
) -> Result<(), String> {
    let mut progress = PageProgress::new();
    // Set once the browser process has ended without failing.
    let mut handed_over = false;
    loop {
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                if let Some(failure) = progress.apply(event, lines) {
                    return Err(failure);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if progress.done() {
                    return Ok(());
                }
                if !handed_over && let Ok(Some(status)) = child.try_wait() {
                    // Whatever it reported before it went away is still queued.
                    while let Ok(event) = events.try_recv() {
                        if let Some(failure) = progress.apply(event, lines) {
                            return Err(failure);
                        }
                    }
                    if progress.returned && progress.open_suites == 0 {
                        return Ok(());
                    }
                    if !status.success() {
                        return Err(format!("closed ({}) before the tests finished", process::describe_status(status)));
                    }
                    // A browser that ends successfully has handed over to another process (Firefox
                    // starts again after applying an update), which still runs the page.
                    handed_over = true;
                }
                if progress.last_event.elapsed() > timeout {
                    return Err(format!(
                        "timed out after {}s without output{}; set {} to wait longer",
                        timeout.as_secs(),
                        if handed_over { ", though the browser process had already exited" } else { "" },
                        browser::BROWSER_TIMEOUT_ENV
                    ));
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err("lost contact with the page".to_owned()),
        }
    }
}

/// Reads text sent over a channel as a byte stream, which ends when the sender is dropped.
struct ChannelReader {
    chunks: Receiver<String>,
    current: Vec<u8>,
    position: usize,
}

impl ChannelReader {
    fn new(chunks: Receiver<String>) -> Self {
        Self { chunks, current: Vec::new(), position: 0 }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.position >= self.current.len() {
            match self.chunks.recv() {
                Ok(text) => (self.current, self.position) = (text.into_bytes(), 0),
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.current.len() - self.position);
        out[..n].copy_from_slice(&self.current[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

fn file_url(path: &Path) -> String {
    let s: String = process::slash(path)
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_owned(),
            '%' => "%25".to_owned(),
            '#' => "%23".to_owned(),
            '?' => "%3F".to_owned(),
            other => other.to_string(),
        })
        .collect();
    if s.starts_with('/') { format!("file://{s}") } else { format!("file:///{s}") }
}

fn test_wasm(ctx: &BuildContext, opts: &TestOptions) -> Result<TestRun> {
    let manifest = ctx.ws.manifest();
    let wasm = manifest.targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let tests = sources(ctx, TargetKind::Wasm, true, &["kt"]);
    if tests.kotlin_files().is_empty() {
        return Ok(no_tests(ctx, TargetKind::Wasm));
    }
    // Tests link against the main klib as a friend module, so the program's
    // `main` is not part of the test binary.
    let main = prepare_wasm(ctx)?;
    let setup = ctx.setup(TargetKind::Wasm)?;
    let libraries = wasm_libraries(ctx, &setup, true)?;
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let name = format!("{}-test", wasm.module_name(&manifest.package));
    let main_node_modules = main.as_ref().and_then(|m| m.node_modules.clone());
    let unit = WasmCompilation {
        name: &name,
        environment: wasm.environment,
        sources: tests,
        libraries,
        friends: main.map(|m| vec![m.klib]).unwrap_or_default(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: ctx.ws.output_dir(ctx.profile, TargetKind::Wasm).join("test"),
        debug: true,
        optimize: false,
        declarations: false,
        main_args: false,
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Wasm), &|| compiling(ctx, TargetKind::Wasm, ", tests"))?;
    // `prepare_wasm` installed the main npm packages one level up; dev-dependencies
    // that need more get their own `node_modules` next to the test module.
    let node_modules =
        if main_node_modules.is_none() || npm_dependencies(&unit.libraries)? != npm_dependencies(&wasm_libraries(ctx, &setup, false)?)? {
            crate::npm::install(ctx, &unit.libraries, &unit.output)?
        } else {
            main_node_modules
        };
    if wasm.environment == WasmEnvironment::Browser {
        return browser_tests(ctx, opts, &unit, node_modules);
    }
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    if opts.filter.is_some() {
        ctx.shell().warn("test filters are not supported for the wasm target; running all tests");
    }
    let mut cmd = Command::new(kargo_toolchain::node()?);
    cmd.arg("-e").arg(format!("import({:?}).then(m => m.startUnitTests())", file_url(&unit.module_path()))).current_dir(ctx.ws.root());
    Ok(TestRun::Reported(cmd, "Wasm tests (Node.js)"))
}

/// Writes the page that runs the compiled Wasm tests and finds the browser to run it in.
fn browser_tests(ctx: &BuildContext, opts: &TestOptions, unit: &WasmCompilation, node_modules: Option<PathBuf>) -> Result<TestRun> {
    let imports = node_modules.as_deref().map(crate::wasm_output::import_map).unwrap_or_default();
    std::fs::write(unit.output.join("index.html"), crate::wasm_output::test_page(unit.name, &imports))
        .context("failed to write the test page")?;
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    if opts.filter.is_some() {
        ctx.shell().warn("test filters are not supported for the wasm target; running all tests");
    }
    let browser = browser::browser()?;
    let mut site = Site::new(&unit.output);
    if let Some(node_modules) = node_modules {
        site = site.mount("node_modules", node_modules);
    }
    let profile = unit.output.join(format!(".browser-profile-{}", std::process::id()));
    Ok(TestRun::Browser(BrowserRun { browser, site, profile }))
}

fn test_native(ctx: &BuildContext, opts: &TestOptions) -> Result<TestRun> {
    let Some(executable) = compile_native_tests(ctx)? else {
        return Ok(TestRun::Done(true));
    };
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    let mut cmd = Command::new(executable);
    cmd.arg("--ktest_logger=TEAMCITY").current_dir(ctx.ws.root());
    if let Some(filter) = &opts.filter {
        cmd.arg(format!("--ktest_filter=*{filter}*"));
    }
    Ok(TestRun::Reported(cmd, "native tests"))
}

/// Compiles the Kotlin/Native tests for this host into a test executable. Returns `None`
/// (after saying why) when there is nothing to run.
fn compile_native_tests(ctx: &BuildContext) -> Result<Option<PathBuf>> {
    let manifest = ctx.ws.manifest();
    let tests = sources(ctx, TargetKind::Native, true, &["kt"]);
    if tests.kotlin_files().is_empty() {
        no_tests(ctx, TargetKind::Native);
        return Ok(None);
    }
    let host = Host::current()?.konan_target;
    let platforms = native_platforms(ctx)?;
    if !platforms.iter().any(|p| p == host) {
        ctx.shell().warn(format!("skipping native tests: this host ({host}) is not in `platforms`"));
        return Ok(None);
    }
    let setup = ctx.setup(TargetKind::Native)?;
    let _ = native_libraries(ctx, &setup, host, false)?;
    let mut libraries = native_libraries(ctx, &setup, host, true)?;
    libraries.extend(native_cinterop(ctx, host)?);
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut all = sources(ctx, TargetKind::Native, false, &["kt"]);
    all.extend(&tests);
    let name = format!("{}-test", manifest.package.name);
    let unit = NativeCompilation {
        name: &name,
        konan_target: host.to_owned(),
        kind: NativeKind::Executable,
        sources: all,
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point: None,
        linker_options: &manifest.targets.native.as_ref().map(|n| n.linker_options.clone()).unwrap_or_default(),
        output_dir: ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(host).join("test"),
        debug: true,
        optimize: false,
        test_runner: true,
    };
    let outcome =
        unit.compile(ctx.shell(), toolchain, &ctx.fingerprint_dir(TargetKind::Native), &|| compiling(ctx, TargetKind::Native, ", tests"))?;
    Ok((outcome != Outcome::Empty).then(|| unit.artifact()))
}

/// A test found by `kargo test --list`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListedTest {
    /// Unique within the target: JUnit's unique id on the JVM, `Class.name` elsewhere.
    id: String,
    /// The enclosing test (such as the parameterized test of an invocation) on the JVM.
    parent_id: Option<String>,
    /// The class or suite (`package.Class`).
    suite: String,
    name: String,
    display_name: String,
    file: Option<PathBuf>,
    line: Option<usize>,
}

impl ListedTest {
    fn from_name(suite: &str, name: &str) -> Self {
        let id = if suite.is_empty() { name.to_owned() } else { format!("{suite}.{name}") };
        ListedTest {
            id,
            parent_id: None,
            suite: suite.to_owned(),
            name: name.to_owned(),
            display_name: name.to_owned(),
            file: None,
            line: None,
        }
    }

    fn full_name(&self) -> String {
        if self.suite.is_empty() { self.name.clone() } else { format!("{}.{}", self.suite, self.name) }
    }
}

/// `kargo test --list`: compiles the tests of each target and prints them without running them.
fn list(ctx: &BuildContext, opts: &TestOptions, format: ListFormat) -> Result<()> {
    let targets = select_targets(ctx, &opts.build.targets)?;
    let listed = super::parallel_map(&targets, |&target| match target {
        TargetKind::Jvm => list_jvm(ctx, opts),
        TargetKind::Native => list_native(ctx, opts),
        TargetKind::Wasm | TargetKind::Android => {
            ctx.shell().note(format!("`--list` does not list {target} tests yet"));
            Ok(Vec::new())
        }
    });
    let package = &ctx.ws.manifest().package.name;
    let mut out = anstream::stdout();
    for (&target, tests) in targets.iter().zip(listed) {
        let mut tests = tests?;
        let test_sources = sources(ctx, target, true, &["kt", "java"]);
        locate_tests(&mut tests, &test_sources.all().cloned().collect::<Vec<_>>());
        for test in &tests {
            match format {
                ListFormat::Text => writeln!(out, "{target}: {}", test.full_name())?,
                ListFormat::Json => {
                    let json = serde_json::json!({
                        "reason": "test-discovered",
                        "id": test.id,
                        "parent-id": test.parent_id,
                        "suite": test.suite,
                        "name": test.name,
                        "display-name": test.display_name,
                        "file": test.file.as_ref().map(|f| f.display().to_string()),
                        "line": test.line,
                    });
                    if let serde_json::Value::Object(message) = json {
                        emit_event(package, target, message);
                    }
                }
            }
        }
        ctx.shell().status("Listed", format!("{} {target} test{}", tests.len(), if tests.len() == 1 { "" } else { "s" }));
    }
    Ok(())
}

fn list_jvm(ctx: &BuildContext, opts: &TestOptions) -> Result<Vec<ListedTest>> {
    let main = build_jvm(ctx)?;
    let Some(tests) = compile_jvm_tests(ctx, &main.classes, "Compiling")? else {
        no_tests(ctx, TargetKind::Jvm);
        return Ok(Vec::new());
    };
    let mut cmd = junit_command(ctx, opts, &tests, JunitMode::List)?;
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.stdin(Stdio::null()).output().context("failed to start the JUnit console launcher")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        anyhow::bail!(
            "the JUnit console launcher failed to list the tests ({}):\n{}{}",
            process::describe_status(output.status),
            stdout.trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    Ok(stdout.lines().filter_map(junit::parse_event).filter_map(|event| listed_from_event(&event)).collect())
}

/// The test in a `test-discovered` message from Kargo's JUnit listener.
fn listed_from_event(event: &serde_json::Map<String, serde_json::Value>) -> Option<ListedTest> {
    let field = |key: &str| event.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    (field("reason")? == "test-discovered").then_some(())?;
    let name = field("name")?;
    Some(ListedTest {
        id: field("id")?,
        parent_id: field("parent-id"),
        suite: field("suite").unwrap_or_default(),
        display_name: field("display-name").unwrap_or_else(|| name.clone()),
        name,
        file: None,
        line: None,
    })
}

fn list_native(ctx: &BuildContext, opts: &TestOptions) -> Result<Vec<ListedTest>> {
    let Some(executable) = compile_native_tests(ctx)? else {
        return Ok(Vec::new());
    };
    let mut cmd = Command::new(&executable);
    cmd.arg("--ktest_list_tests").current_dir(ctx.ws.root()).stdin(Stdio::null());
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().with_context(|| format!("failed to start {}", executable.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "the native test executable failed to list the tests ({}):\n{}",
            process::describe_status(output.status),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    let filter = opts.filter.as_deref().map(|f| f.replace('#', "."));
    let tests = parse_native_list(&String::from_utf8_lossy(&output.stdout));
    Ok(tests.into_iter().filter(|t| filter.as_deref().is_none_or(|f| t.full_name().contains(f))).collect())
}

/// Parses what a Kotlin/Native test executable prints for `--ktest_list_tests`: each suite
/// on a line of its own ending with `.`, followed by its tests, indented.
fn parse_native_list(output: &str) -> Vec<ListedTest> {
    let mut suite = String::new();
    let mut tests = Vec::new();
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            tests.push(ListedTest::from_name(&suite, line.trim()));
        } else {
            suite = line.trim().trim_end_matches('.').to_owned();
        }
    }
    tests
}

/// Fills in the source file and line of each test, by finding the file that declares its
/// class and the function in it. Tests whose source cannot be found keep `None`.
fn locate_tests(tests: &mut [ListedTest], files: &[PathBuf]) {
    let mut texts: Vec<Option<String>> = vec![None; files.len()];
    for test in tests {
        // `app.Outer$Inner` is declared as `class Inner` in package `app`.
        let (package, class) = match test.suite.rsplit_once('.') {
            Some((package, class)) => (package, class),
            None => ("", test.suite.as_str()),
        };
        let simple = class.rsplit('$').next().unwrap_or(class);
        if simple.is_empty() {
            continue;
        }
        for (i, file) in files.iter().enumerate() {
            let text = texts[i].get_or_insert_with(|| std::fs::read_to_string(file).unwrap_or_default());
            if declared_package(text) != package {
                continue;
            }
            let Some(class_line) = find_line(text, |line| declares(line, &["class", "object", "interface"], simple)) else {
                continue;
            };
            let name = test.name.trim_end_matches("()");
            let line = find_line(text, |line| declares(line, &["fun", "void"], name)).unwrap_or(class_line);
            test.file = Some(std::path::absolute(file).unwrap_or_else(|_| file.clone()));
            test.line = Some(line);
            break;
        }
    }
}

/// The package a Kotlin or Java source declares (empty for the default package).
fn declared_package(text: &str) -> &str {
    text.lines().map(str::trim).find_map(|line| line.strip_prefix("package ")).map(|p| p.trim().trim_end_matches(';').trim()).unwrap_or("")
}

/// The 1-based number of the first line matching `matches`.
fn find_line(text: &str, matches: impl Fn(&str) -> bool) -> Option<usize> {
    text.lines().position(matches).map(|i| i + 1)
}

/// Whether `line` declares `name` after one of `keywords` (`class Foo`, `` fun `a test`( ``, `void foo(`).
fn declares(line: &str, keywords: &[&str], name: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let rest = |i: usize| words[i + 1..].join(" ");
    (0..words.len()).any(|i| {
        if !keywords.contains(&words[i]) {
            return false;
        }
        // Skip type parameters: `fun <T> name(`.
        let after = rest(i);
        let after = if after.starts_with('<') { after.split_once("> ").map(|(_, r)| r.to_owned()).unwrap_or_default() } else { after };
        let after = after.strip_prefix(&format!("`{name}`")).or_else(|| after.strip_prefix(name));
        after.is_some_and(|r| r.is_empty() || r.starts_with(['(', ' ', ':', '{', '<']))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_regex() {
        assert_eq!(regex_escape("a.b*c"), "a\\.b\\*c");
        assert_eq!(regex_escape("Plain"), "Plain");
    }

    #[test]
    fn junit_filters() {
        let classes = ["app.MyTest", "app.MyTest$Nested", "app.KSpec"].map(String::from);
        // Without a filter every class is scanned, not only `*Test`.
        assert_eq!(junit_filter_args(None, &classes), ["--include-classname=.*"]);
        // A class name, or part of one, selects classes, which works for every engine.
        assert_eq!(junit_filter_args(Some("KSpec"), &classes), ["--include-classname=.*KSpec.*"]);
        assert_eq!(junit_filter_args(Some("MyTest.Nested"), &classes), ["--include-classname=.*MyTest[.$]Nested.*"]);
        // Anything else is matched against `package.Class#method`.
        assert_eq!(junit_filter_args(Some("MyTest.b"), &classes), ["--include-classname=.*", "--include-methodname=.*MyTest[.$#]b.*"]);
        assert_eq!(junit_filter_args(Some("MyTest#b"), &classes)[1], "--include-methodname=.*MyTest[.$#]b.*");
        assert_eq!(junit_filter_args(Some("adds"), &classes)[1], "--include-methodname=.*adds.*");
    }

    #[test]
    fn named_test_classes() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["app/MyTest.class", "app/MyTest$Nested.class", "app/MyTest$b$1.class", "META-INF/test.kotlin_module", "TopKt.class"] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        assert_eq!(test_class_names(dir.path()), ["TopKt", "app.MyTest$Nested", "app.MyTest"]);
    }

    #[test]
    fn parses_native_test_lists() {
        let tests = parse_native_list("app.MathTest.\n  adds\n  fails\n\nOtherTest.\n  works\n");
        let names: Vec<String> = tests.iter().map(ListedTest::full_name).collect();
        assert_eq!(names, ["app.MathTest.adds", "app.MathTest.fails", "OtherTest.works"]);
        assert_eq!(tests[0].id, "app.MathTest.adds");
        assert_eq!(tests[0].suite, "app.MathTest");
    }

    #[test]
    fn reads_listed_tests_from_the_listener() {
        let event = junit::parse_event(
            r#"##kargo {"reason":"test-discovered","id":"[engine:x]/[method:a()]","parent-id":"[engine:x]","suite":"app.T","name":"a","display-name":"a()"}"#,
        )
        .unwrap();
        let test = listed_from_event(&event).unwrap();
        assert_eq!((test.id.as_str(), test.parent_id.as_deref()), ("[engine:x]/[method:a()]", Some("[engine:x]")));
        assert_eq!((test.suite.as_str(), test.name.as_str(), test.display_name.as_str()), ("app.T", "a", "a()"));
        let started = junit::parse_event(r#"##kargo {"reason":"test-started","id":"x","name":"a"}"#).unwrap();
        assert!(listed_from_event(&started).is_none());
    }

    #[test]
    fn locates_test_functions() {
        let dir = tempfile::tempdir().unwrap();
        let kotlin = dir.path().join("MathTest.kt");
        std::fs::write(
            &kotlin,
            "package app\n\nclass MathTest {\n    @Test\n    fun `adds two`() {}\n\n    @Test fun <T> generic() {}\n\n    class Inner {\n        @Test fun deep() {}\n    }\n}\n",
        )
        .unwrap();
        let java = dir.path().join("JavaTest.java");
        std::fs::write(&java, "package app;\n\nclass JavaTest {\n    @Test\n    void works() {}\n}\n").unwrap();
        let other = dir.path().join("Other.kt");
        std::fs::write(&other, "package elsewhere\n\nclass MathTest {\n    fun adds() {}\n}\n").unwrap();
        let mut tests = vec![
            ListedTest::from_name("app.MathTest", "adds two"),
            ListedTest::from_name("app.MathTest", "generic"),
            ListedTest::from_name("app.MathTest$Inner", "deep"),
            ListedTest::from_name("app.JavaTest", "works()"),
            ListedTest::from_name("app.MathTest", "dynamic name"),
            ListedTest::from_name("app.Missing", "x"),
        ];
        locate_tests(&mut tests, &[other.clone(), kotlin.clone(), java.clone()]);
        let found: Vec<_> = tests.iter().map(|t| (t.file.as_deref().and_then(Path::file_name), t.line)).collect();
        let kt = Some(std::ffi::OsStr::new("MathTest.kt"));
        assert_eq!(found[0], (kt, Some(5)));
        assert_eq!(found[1], (kt, Some(7)));
        assert_eq!(found[2], (kt, Some(10)));
        assert_eq!(found[3], (Some(std::ffi::OsStr::new("JavaTest.java")), Some(5)));
        // A test without a function of its name points at its class.
        assert_eq!(found[4], (kt, Some(3)));
        assert_eq!(found[5], (None, None));
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url(Path::new("/tmp/x.mjs")), "file:///tmp/x.mjs");
        assert_eq!(file_url(Path::new("/tmp/a b#.mjs")), "file:///tmp/a%20b%23.mjs");
        if cfg!(windows) {
            assert_eq!(file_url(Path::new(r"C:\a\b.mjs")), "file:///C:/a/b.mjs");
        }
    }
}
