//! `kargo test`: compiles and runs tests for each target, or lists them (`--list`).
//! With `--message-format json` each test's start and result is also a JSON message
//! on stdout, and the human-readable report moves to stderr.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anstyle::AnsiColor;
use anyhow::{Context, Result};

use super::build::{
    BuildOptions, JvmKspUnit, announce, build_jvm, compiling, finished, for_platform, hook_env, js_work_dir, jvm_classpaths,
    jvm_javac_args, jvm_processor_path, native_cinterop, native_platforms, prepare_js, prepare_wasm, run_jvm_ksp, sources,
};
use super::context::BuildContext;
use crate::builtins::{JUNIT_MIN_JDK, JUNIT_VERSION};
use crate::env::EnvOptions;
use crate::warnings;
use crate::web::{self, PageEvent, Site};
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_compiler::js::JsCompilation;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::testreport::{self, CaseOutcome, TestEvent};
use kargo_compiler::wasm::{WasmCompilation, npm_dependencies};
use kargo_compiler::{Outcome, SourceSet, common_kotlin_args};
use kargo_compiler::{diagnostics, junit};
use kargo_manifest::{Hook, JsEnvironment, JsModuleKind, NativeKind, Scope, TargetKind, WasmEnvironment};
use kargo_maven::Platform;
use kargo_toolchain::browser::{self, Browser};
use kargo_toolchain::native::Host;
use kargo_util::fs::{collect_files, join_paths};
use kargo_util::process;

#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    pub build: BuildOptions,
    /// The `.env` files the tests' environment is read from (`--env-file`, `--no-env-file`).
    pub env: EnvOptions,
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
    /// Only run JVM tests with one of these JUnit tags (`--include-tag`).
    pub include_tags: Vec<String>,
    /// Skip JVM tests with one of these JUnit tags (`--exclude-tag`).
    pub exclude_tags: Vec<String>,
    /// Only run the JVM test classes that failed in the last run (`--failed`).
    pub failed: bool,
    /// Run JVM tests with the optimized jar on the runtime classpath.
    pub against_min: bool,
    /// Run the Android tests of `android-tests/` on a device instead of the local ones (`--instrumented`).
    pub instrumented: bool,
    /// `--shard index/count` of the instrumented tests, the index counted from 1.
    pub shard: Option<(u32, u32)>,
    /// Run the instrumented tests with Android Test Orchestrator (`--orchestrator`).
    pub orchestrator: bool,
    /// Measure JVM line and branch coverage with JaCoCo (`--coverage`).
    pub coverage: bool,
    /// Fail below this overall line percentage instead of the manifest's thresholds (`--fail-under`).
    pub fail_under: Option<f64>,
    /// The device, emulator and `--stop-emulator` of the instrumented tests.
    pub android: crate::android_device::AndroidRun,
    /// Test suites to run (`--suite`): `unit` (the default), a `[test.suites.<name>]`
    /// suite, or `all`. Empty runs the default suite.
    pub suites: Vec<String>,
}

/// One test suite to run: the default `unit` suite (`None`) or a declared suite.
#[derive(Debug, Clone)]
struct SuiteSelection {
    /// `unit` for the default suite, else the suite name.
    name: String,
    /// The declared suite, if any.
    suite: Option<kargo_manifest::TestSuite>,
}

impl SuiteSelection {
    fn is_unit(&self) -> bool {
        self.suite.is_none()
    }
}

/// The suites `opts` runs for this package, in order: the default `unit` suite and each
/// declared suite `--suite` names, with `--suite all` expanding to all of them.
/// Suites the package does not declare are left out for the caller to skip.
fn resolve_suites_for_package(ctx: &BuildContext, opts: &TestOptions) -> Result<Vec<SuiteSelection>> {
    let manifest = ctx.ws.manifest();
    if opts.suites.is_empty() {
        return Ok(vec![SuiteSelection { name: kargo_manifest::TestSettings::UNIT.to_owned(), suite: None }]);
    }
    let mut out: Vec<SuiteSelection> = Vec::new();
    for requested in &opts.suites {
        if requested == kargo_manifest::TestSettings::ALL {
            let unit = SuiteSelection { name: kargo_manifest::TestSettings::UNIT.to_owned(), suite: None };
            if !out.iter().any(|s| s.is_unit()) {
                out.push(unit);
            }
            for name in manifest.test.suites.keys() {
                if !out.iter().any(|s| &s.name == name) {
                    out.push(SuiteSelection { name: name.clone(), suite: manifest.test.suite(name).cloned() });
                }
            }
            continue;
        }
        if requested == kargo_manifest::TestSettings::UNIT {
            if !out.iter().any(|s| s.is_unit()) {
                out.push(SuiteSelection { name: requested.clone(), suite: None });
            }
            continue;
        }
        match manifest.test.suite(requested) {
            Some(suite) => {
                if !out.iter().any(|s| &s.name == requested) {
                    out.push(SuiteSelection { name: requested.clone(), suite: Some(suite.clone()) });
                }
            }
            None => {
                // Left out: the caller notes the package has no such suite and skips it.
                // Unknown names are reported by `validate_suites` before any package runs.
                continue;
            }
        }
    }
    Ok(out)
}

/// Fails when `--suite` names no suite of the selected packages: a typo that would otherwise
/// run nothing and pass. Workspaces may still have members without the suite; those are skipped.
pub fn validate_suites(project: &crate::workspace::Project, selected: &[usize], suites: &[String]) -> Result<()> {
    use kargo_manifest::TestSettings as TS;
    for requested in suites {
        if requested == TS::UNIT || requested == TS::ALL {
            continue;
        }
        let declared = selected.iter().any(|&i| project.package(i).manifest().test.suite(requested).is_some());
        if !declared {
            let mut all: Vec<String> = selected.iter().flat_map(|&i| project.package(i).manifest().test.suites.keys().cloned()).collect();
            all.sort();
            all.dedup();
            let known = if all.is_empty() { "none (only the default `unit` suite)".to_owned() } else { all.join(", ") };
            let mut known_list = vec![TS::UNIT.to_owned(), TS::ALL.to_owned()];
            known_list.extend(all);
            return Err(kargo_util::help::unknown(
                format!(
                    "no test suite `{requested}` in the selected packages (declared suites: {known}); suites are declared as `[test.suites.<name>]`"
                ),
                requested,
                &known_list,
            ));
        }
    }
    Ok(())
}

/// The `kargo.lock` key of a test resolution for `suite`: `jvm/test` for the default
/// suite, `jvm/test-<name>` for a declared one. `base` is the unit key (`jvm/test`,
/// `wasm/test`, `js/test` or `native-<konan>/test`).
fn suite_test_key(base: &str, suite: Option<&kargo_manifest::TestSuite>) -> String {
    match suite {
        None => base.to_owned(),
        Some(suite) => format!("{base}-{}", suite.name),
    }
}

/// Clears and returns the test-reports directory of `target` and `suite`:
/// `test-reports` for the default suite, `test-reports-<name>` for a declared one.
fn reports_dir_for_suite(ctx: &BuildContext, target: TargetKind, suite: Option<&kargo_manifest::TestSuite>) -> Result<PathBuf> {
    let name = match suite {
        None => "test-reports".to_owned(),
        Some(suite) => format!("test-reports-{}", suite.name),
    };
    let dir = ctx.ws.output_dir(&ctx.profile, target).join(name);
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

/// The directory of the compiled JVM test classes of `suite`:
/// `test-classes` for the default suite, `test-classes-<name>` for a declared one.
fn test_classes_dir(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    let name = match suite {
        None => "test-classes".to_owned(),
        Some(suite) => format!("test-classes-{}", suite.name),
    };
    ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join(name)
}

/// The file that records the JVM test classes that failed in the last run of `suite`:
/// `test-failures` for the default suite, `test-failures-<name>` for a declared one.
fn failures_file_for_suite(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    let name = match suite {
        None => "test-failures".to_owned(),
        Some(suite) => format!("test-failures-{}", suite.name),
    };
    ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join(name)
}

/// The output directory of the Wasm test module of `suite`:
/// `test` for the default suite, `test-<name>` for a declared one.
fn wasm_test_dir(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    let name = match suite {
        None => "test".to_owned(),
        Some(suite) => format!("test-{}", suite.name),
    };
    ctx.ws.output_dir(&ctx.profile, TargetKind::Wasm).join(name)
}

/// The output directory of the Kotlin/JS test module of `suite`.
fn js_test_dir(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    let name = match suite {
        None => "test".to_owned(),
        Some(suite) => format!("test-{}", suite.name),
    };
    ctx.ws.output_dir(&ctx.profile, TargetKind::Js).join(name)
}

/// The output directory of the Native test executable of `suite` for `konan_target`.
fn native_test_dir(ctx: &BuildContext, konan_target: &str, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    let name = match suite {
        None => "test".to_owned(),
        Some(suite) => format!("test-{}", suite.name),
    };
    ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(konan_target).join(name)
}

/// The JVM arguments of `suite`: `[test] jvm-args`, then the suite's own.
fn suite_jvm_args(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> Vec<String> {
    let mut args = ctx.ws.manifest().test.jvm_args.clone();
    if let Some(suite) = suite {
        args.extend(suite.jvm_args.iter().cloned());
    }
    args
}

/// The JUnit tags of `suite`: the command line's, then the suite's own.
fn suite_include_tags(opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Vec<String> {
    let mut tags = opts.include_tags.clone();
    if let Some(suite) = suite {
        tags.extend(suite.include_tags.iter().cloned());
    }
    tags
}

/// The excluded JUnit tags of `suite`: the command line's, then the suite's own.
fn suite_exclude_tags(opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Vec<String> {
    let mut tags = opts.exclude_tags.clone();
    if let Some(suite) = suite {
        tags.extend(suite.exclude_tags.iter().cloned());
    }
    tags
}

/// The whole-run timeout of `suite`: the suite's own, else `[test] timeout`.
fn suite_timeout(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> Option<Duration> {
    suite.and_then(|s| s.timeout).or(ctx.ws.manifest().test.timeout)
}

/// The per-test timeout of `suite`: the suite's own, else `[test] test-timeout`.
fn suite_test_timeout(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> Option<Duration> {
    suite.and_then(|s| s.test_timeout).or(ctx.ws.manifest().test.test_timeout)
}

/// The working directory the tests of `suite` run in: the suite's `cwd`, else the package root.
fn suite_cwd(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> PathBuf {
    match suite.and_then(|s| s.cwd.as_ref()) {
        Some(cwd) => ctx.ws.root().join(cwd),
        None => ctx.ws.root().to_path_buf(),
    }
}

/// The environment the tests of `suite` run in: the project environment, with the suite's
/// own variables on top.
fn suite_project_env(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<crate::env::ProjectEnv> {
    let mut env = ctx.project_env(&opts.env)?;
    if let Some(suite) = suite {
        for (name, value) in &suite.env {
            env.vars.insert(
                name.clone(),
                crate::env::Var { value: value.clone(), source: crate::env::Source::Manifest(format!("[test.suites.{}.env]", suite.name)) },
            );
            env.missing.retain(|(missing, _)| missing != name);
        }
    }
    Ok(env)
}

/// The label of a test run for `target` and `suite`: `JVM tests` for the default suite,
/// `JVM tests (integration)` for a declared one.
fn suite_label(target: TargetKind, suite: Option<&kargo_manifest::TestSuite>) -> String {
    match suite {
        None => format!("{target} tests"),
        Some(suite) => format!("{target} tests ({})", suite.name),
    }
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
    let targets = opts.build.select(ctx)?;
    let platforms = if opts.build.platforms.is_empty() {
        ctx.ws.manifest().build.platforms.clone().unwrap_or_default()
    } else {
        opts.build.platforms.clone()
    };
    for target in &targets {
        crate::platform::check_target_platforms(*target, &platforms, None)?;
    }
    if opts.against_min && targets != [TargetKind::Jvm] {
        anyhow::bail!("`--against-min` runs JVM tests; select the JVM target with `-t jvm`");
    }
    if opts.debug.is_some() && targets != [TargetKind::Jvm] {
        anyhow::bail!("`--debug` runs the JVM tests under a debugger; select the JVM target with `-t jvm`");
    }
    if opts.coverage {
        if opts.no_run {
            anyhow::bail!("`--coverage` measures the tests; leave out `--no-run` to run them");
        }
        if let Some(fail_under) = opts.fail_under
            && !(0.0..=100.0).contains(&fail_under)
        {
            anyhow::bail!("`--fail-under {fail_under}` is not a percentage; use a number from 0 to 100");
        }
        if !targets.contains(&TargetKind::Jvm) {
            anyhow::bail!("`--coverage` measures the JVM tests with JaCoCo; select the JVM target with `-t jvm`");
        }
    }
    let suites = resolve_suites_for_package(ctx, opts)?;
    if suites.is_empty() {
        let requested = opts.suites.join(", ");
        ctx.shell().note(format!("package `{}` has no test suite `{requested}`; skipping it (declared suites: {})", ctx.ws.name(), {
            let names: Vec<&str> = ctx.ws.manifest().test.suites.keys().map(String::as_str).collect();
            if names.is_empty() { "none (only the default `unit` suite)".to_owned() } else { names.join(", ") }
        }));
        return Ok(true);
    }
    // Warn about suite directories that exist but are not declared.
    warnings::emit(ctx.shell(), ctx.project, ctx.ws, warnings::undeclared_suites(ctx.ws));
    note_jvm_only_options(ctx, &targets, opts);
    // Fails before anything compiles when a required variable is not set, unless only Android or
    // browser tests run, which get no environment.
    if targets.iter().any(|&target| ctx.program_env_applies(target)) {
        let env = ctx.project_env(&opts.env)?;
        warnings::emit(ctx.shell(), ctx.project, ctx.ws, env.tracked_secret_warnings(ctx.ws.root()));
        // A suite's own variables may add the missing ones; each suite checks again when it runs.
        for selection in &suites {
            if let Some(suite) = selection.suite.as_ref()
                && !suite.env.is_empty()
            {
                let _ = suite_project_env(ctx, opts, Some(suite))?;
            }
        }
    }
    let env = hook_env(ctx, &opts.env)?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PreTest, &env)?;
    // A project may test code that lives elsewhere, so missing main sources are not worth a warning here.
    warnings::emit(ctx.shell(), ctx.project, ctx.ws, warnings::ignored_files(ctx.ws));

    let fail_fast = !opts.no_fail_fast && ctx.ws.manifest().test.fail_fast;
    // (suite, target, outcome), in the order they ran.
    let mut outcomes: Vec<(String, TargetKind, TargetOutcome)> = Vec::new();
    let mut fatal = None;
    for selection in &suites {
        let suite = selection.suite.as_ref();
        let label = if selection.is_unit() { "unit".to_owned() } else { selection.name.clone() };
        // Compile every target's tests at the same time, then run them one target at a
        // time so their output doesn't interleave.
        let prepared = super::parallel_targets(&targets, |target| match target {
            TargetKind::Jvm => test_jvm_for_suite(ctx, opts, suite),
            TargetKind::Wasm => test_wasm_for_suite(ctx, opts, suite),
            TargetKind::Js => test_js_for_suite(ctx, opts, suite),
            TargetKind::Native => test_native_for_suite(ctx, opts, suite),
            TargetKind::Android => {
                if suite.is_some() {
                    ctx.shell().note(format!(
                        "skipping the android target for the `{}` suite: test suites are not supported for Android yet; its tests run in the `unit` suite",
                        selection.name
                    ));
                    Ok(TestRun::Done(true))
                } else {
                    Ok(TestRun::Android)
                }
            }
        });
        // The whole suite's time limit counts from when its tests start running, not from compiling them.
        let deadline = suite_timeout(ctx, suite).map(|limit| Instant::now() + limit);
        for (&target, run) in targets.iter().zip(prepared) {
            if outcomes.iter().any(|(_, _, o)| *o != TargetOutcome::Passed) && fail_fast {
                outcomes.push((label.clone(), target, TargetOutcome::Skipped));
                continue;
            }
            let passed = run.and_then(|run| match run {
                TestRun::Done(passed) => Ok(passed),
                TestRun::Junit(run) => run_junit_for_suite(ctx, run, opts, suite, deadline),
                TestRun::Reported(cmd, run_label) => run_reported_for_suite(ctx, cmd, run_label, target, opts, suite, deadline),
                TestRun::Browser(run) => run_in_browser_for_suite(ctx, run, target, opts, suite, deadline),
                TestRun::Android => test_android(ctx, opts, deadline),
            });
            let outcome = match passed {
                Ok(true) => TargetOutcome::Passed,
                Ok(false) => TargetOutcome::Failed,
                Err(e) if fail_fast || (targets.len() == 1 && suites.len() == 1) => {
                    fatal = Some(e);
                    TargetOutcome::Error
                }
                Err(e) => {
                    let where_ = if selection.is_unit() {
                        format!("the {target} target")
                    } else {
                        format!("the {target} target of the `{}` suite", selection.name)
                    };
                    ctx.shell().error(format!("could not test {where_}: {e:#}"));
                    TargetOutcome::Error
                }
            };
            outcomes.push((label.clone(), target, outcome));
        }
        if fatal.is_some() {
            // Still list the skipped suites/targets below.
            let remaining: Vec<String> = suites
                .iter()
                .skip_while(|s| s.name != selection.name)
                .skip(1)
                .flat_map(|s| targets.iter().map(move |t| (s.name.clone(), *t)))
                .chain(targets.iter().skip_while(|_| false).filter(|_| false).map(|t| (label.clone(), *t)))
                .map(|(s, _)| s)
                .collect();
            let _ = remaining;
            // Mark the rest as skipped.
            for later in suites.iter().skip_while(|s| s.name != selection.name).skip(1) {
                for &target in &targets {
                    outcomes.push((later.name.clone(), target, TargetOutcome::Skipped));
                }
            }
            break;
        }
    }
    let unit_only = suites.len() == 1 && suites[0].is_unit();
    let skipped: Vec<String> = outcomes
        .iter()
        .filter(|(_, _, o)| *o == TargetOutcome::Skipped)
        .map(|(suite, target, _)| if suite == "unit" { target.as_str().to_owned() } else { format!("{target} ({suite})") })
        .collect();
    if !skipped.is_empty() {
        if unit_only {
            ctx.shell().note(format!(
                "skipped the {} target{} after a failure; pass `--no-fail-fast` to test every target",
                skipped.join(", "),
                if skipped.len() == 1 { "" } else { "s" }
            ));
        } else {
            ctx.shell()
                .note(format!("skipped {} after a failure; pass `--no-fail-fast` to test every target and suite", skipped.join(", ")));
        }
    } else if outcomes.len() > 1 && !fail_fast {
        if unit_only {
            let target_outcomes: Vec<(TargetKind, TargetOutcome)> = outcomes.iter().map(|(_, t, o)| (*t, *o)).collect();
            print_target_table(&target_outcomes, human_out())?;
        } else {
            print_suite_table(&outcomes, human_out())?;
        }
    }
    if let Some(e) = fatal {
        return Err(e);
    }
    let ok = outcomes.iter().all(|(_, _, o)| *o == TargetOutcome::Passed);
    let mut env = env;
    env.push(("KARGO_TEST_RESULT".to_owned(), if ok { "success" } else { "failure" }.to_owned()));
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostTest, &env)?;
    if opts.no_run {
        finished(ctx, start);
    }
    Ok(ok)
}

/// Where the human-readable test report goes: stdout, or stderr when stdout carries
/// JSON messages (`--message-format json`).
pub(crate) fn human_out() -> Box<dyn Write> {
    if diagnostics::is_json() { Box::new(anstream::stderr()) } else { Box::new(kargo_util::capture::Tee::new(anstream::stdout())) }
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
    emit_summary_for_suite(ctx, target, None, passed, failed, ignored, success)
}

pub(crate) fn emit_summary_for_suite(
    ctx: &BuildContext,
    target: TargetKind,
    suite: Option<&kargo_manifest::TestSuite>,
    passed: usize,
    failed: usize,
    ignored: usize,
    success: bool,
) {
    if !diagnostics::is_json() {
        return;
    }
    let mut json =
        serde_json::json!({ "reason": "test-summary", "passed": passed, "failed": failed, "ignored": ignored, "success": success });
    if let Some(suite) = suite
        && let serde_json::Value::Object(map) = &mut json
    {
        map.insert("test-suite".to_owned(), suite.name.clone().into());
    }
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

/// Prints one line per suite and target with how its tests went, after a `--no-fail-fast` run.
fn print_suite_table(outcomes: &[(String, TargetKind, TargetOutcome)], mut out: impl std::io::Write) -> std::io::Result<()> {
    writeln!(
        out,
        "
test results by suite and target:"
    )?;
    for (suite, target, outcome) in outcomes {
        let (style, text) = match outcome {
            TargetOutcome::Passed => (AnsiColor::Green.on_default(), "ok"),
            TargetOutcome::Failed => (AnsiColor::Red.on_default(), "FAILED"),
            TargetOutcome::Error => (AnsiColor::Red.on_default(), "error (could not compile or run the tests)"),
            TargetOutcome::Skipped => (AnsiColor::Yellow.on_default(), "skipped"),
        };
        let what = if suite == "unit" { target.as_str().to_owned() } else { format!("{target} ({suite})") };
        writeln!(out, "    {what:<24} {style}{text}{style:#}")?;
    }
    Ok(())
}

/// A target's compiled tests, ready to run.
enum TestRun {
    /// Nothing (left) to run; whether the target passed.
    Done(bool),
    /// The JUnit console launcher.
    Junit(JunitRun),
    /// A test executable reporting TeamCity messages, with its label.
    Reported(Command, &'static str),
    /// Kotlin/Wasm or Kotlin/JS tests in a headless browser, which reports TeamCity messages to Kargo.
    Browser(BrowserRun),
    /// Gradle compiles and runs Android tests in one step.
    Android,
}

/// The JUnit console launcher with the main classes its coverage report reads.
struct JunitRun {
    cmd: Command,
    main_classes: PathBuf,
}

/// Kotlin/Wasm or Kotlin/JS tests that run in a browser.
struct BrowserRun {
    /// What `kargo test` calls them: `Wasm tests (headless browser)`.
    label: &'static str,
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

fn no_tests_for_suite(ctx: &BuildContext, target: TargetKind, suite: Option<&kargo_manifest::TestSuite>) -> TestRun {
    match suite {
        None => no_tests(ctx, target),
        Some(suite) => {
            ctx.shell().note(format!(
                "no `{}` tests found for the {target} target (put them in `{}/` or `{}-{target}/`)",
                suite.name,
                suite.sources.display(),
                suite.sources.display()
            ));
            TestRun::Done(true)
        }
    }
}

fn test_android(ctx: &BuildContext, opts: &TestOptions, deadline: Option<Instant>) -> Result<bool> {
    super::local::test_fixtures_unsupported(ctx, TargetKind::Android)?;
    if opts.instrumented {
        let reports = (opts.junit_report && !opts.no_run).then(|| reports_dir(ctx, TargetKind::Android)).transpose()?;
        return test_android_instrumented(ctx, opts, reports.as_deref(), deadline);
    }
    if sources(ctx, TargetKind::Android, true, &["kt", "java"])?.is_empty() {
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

/// `--instrumented`: the tests of `android-tests/` on a device, which is chosen like `kargo run -t android` does and
/// started if it is a virtual device that is not running.
fn test_android_instrumented(ctx: &BuildContext, opts: &TestOptions, reports: Option<&Path>, deadline: Option<Instant>) -> Result<bool> {
    let dir = ctx.ws.instrumented_tests();
    if !dir.is_dir() {
        let rel = dir.strip_prefix(ctx.ws.root()).unwrap_or(&dir).display().to_string();
        anyhow::bail!("there are no instrumented tests: put them in `{rel}/` (for example `{rel}/ExampleTest.kt`)");
    }
    if sources_in(&dir).is_empty() {
        let rel = dir.strip_prefix(ctx.ws.root()).unwrap_or(&dir).display().to_string();
        ctx.shell().note(format!("no instrumented tests found (put them in {rel}/)"));
        return Ok(true);
    }
    let selection = if opts.no_run { None } else { Some(crate::gradle::choose_device(ctx, &opts.android)?) };
    let run = crate::gradle::InstrumentedRun {
        filter: opts.filter.as_deref(),
        no_run: opts.no_run,
        report_dir: reports,
        include: &opts.include_tags,
        exclude: &opts.exclude_tags,
        failed: opts.failed,
        shard: opts.shard,
        orchestrator: opts.orchestrator,
        test_timeout: ctx.ws.manifest().test.test_timeout,
        deadline,
    };
    let result = crate::gradle::test_instrumented(ctx, &run, selection.as_ref());
    if let Some(dir) = reports {
        wrote_reports(ctx, dir);
    }
    if let (Some(selection), true) = (&selection, opts.android.stop_emulator)
        && selection.started
    {
        let toolbox = crate::android_device::Toolbox::locate(ctx.config, Some(ctx.ws.manifest()));
        crate::android_device::stop_emulator(&toolbox.adb()?, ctx.shell(), &selection.serial)?;
    }
    result
}

/// The Kotlin and Java files under `dir`.
fn sources_in(dir: &Path) -> Vec<PathBuf> {
    kargo_util::fs::collect_files(dir, &["kt", "java"])
}

/// Clears and returns `target/<profile>/<target>/test-reports`, where `--report junit` writes.
fn reports_dir(ctx: &BuildContext, target: TargetKind) -> Result<PathBuf> {
    let dir = ctx.ws.output_dir(&ctx.profile, target).join("test-reports");
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

fn wrote_reports(ctx: &BuildContext, dir: &Path) {
    let shown = dir.strip_prefix(ctx.ws.root()).unwrap_or(dir);
    ctx.shell().status("Reported", format!("JUnit XML in {}", shown.display()));
}

fn test_jvm_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<TestRun> {
    let main = build_jvm(ctx)?;
    let Some(JvmTestClasses { classes: test_classes, mut classpath }) =
        compile_jvm_tests_for_suite(ctx, &main.classes, "Compiling", suite)?
    else {
        return Ok(no_tests_for_suite(ctx, TargetKind::Jvm, suite));
    };
    if opts.against_min {
        let files = super::package(ctx, &super::PackageOptions { optimize: true, ..Default::default() })?;
        if classpath.first() == Some(&main.classes) {
            classpath.remove(0);
        }
        classpath.insert(0, files[0].clone());
    }
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    let rerun = if opts.failed { failed_jvm_classes_for_suite(ctx, suite)? } else { None };
    if opts.failed && rerun.is_none() {
        let where_ = match suite {
            None => "no failed JVM tests are recorded; running all of them".to_owned(),
            Some(suite) => format!("no failed JVM tests of the `{}` suite are recorded; running all of them", suite.name),
        };
        ctx.shell().note(where_);
    }
    if rerun.as_ref().is_some_and(Vec::is_empty) {
        let where_ = match suite {
            None => "no JVM test failed in the last run".to_owned(),
            Some(suite) => format!("no JVM test of the `{}` suite failed in the last run", suite.name),
        };
        ctx.shell().note(where_);
        return Ok(TestRun::Done(true));
    }
    let mode = if diagnostics::is_json() { JunitMode::Events } else { JunitMode::Run };
    let tests = JvmTestClasses { classes: test_classes, classpath };
    let cmd = junit_command_for_suite(ctx, opts, &tests, mode, rerun.as_deref(), suite)?;
    Ok(TestRun::Junit(JunitRun { cmd, main_classes: main.classes.clone() }))
}

/// The test classes that failed in the last JVM run (`--failed`); `None` when no run has recorded any.
fn failed_jvm_classes_for_suite(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> Result<Option<Vec<String>>> {
    let path = failures_file_for_suite(ctx, suite);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Records the JVM test classes that failed in the last run, for `--failed`. `None` forgets them,
/// which is what an unfinished run leaves: its failures are unknown.
fn record_jvm_failures_for_suite(ctx: &BuildContext, failed: Option<&[String]>, suite: Option<&kargo_manifest::TestSuite>) -> Result<()> {
    let path = failures_file_for_suite(ctx, suite);
    match failed {
        Some(classes) => {
            let text: String = classes.iter().map(|class| format!("{class}\n")).collect();
            std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to remove {}", path.display())),
        },
    }
}

/// The test classes with a failed test in the JUnit XML reports of a run, in name order.
fn failed_classes(reports: &Path) -> Result<Vec<String>> {
    let mut classes = std::collections::BTreeSet::new();
    for file in kargo_util::fs::collect_files(reports, &["xml"]) {
        let xml = std::fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
        let summary = testreport::report_junit_xml(&xml, std::io::sink()).with_context(|| format!("failed to read {}", file.display()))?;
        for case in summary.cases.iter().filter(|case| matches!(case.outcome, CaseOutcome::Failed { .. })) {
            if !case.class.is_empty() {
                classes.insert(case.class.clone());
            }
        }
    }
    Ok(classes.into_iter().collect())
}

/// Notes the options that only apply to JVM tests, for the targets that will not use them.
fn note_jvm_only_options(ctx: &BuildContext, targets: &[TargetKind], opts: &TestOptions) {
    // Instrumented Android tests take tag filters (as annotations), `--failed` and the timeouts.
    let others: Vec<&str> = targets
        .iter()
        .filter(|target| **target != TargetKind::Jvm && !(opts.instrumented && **target == TargetKind::Android))
        .map(|target| target.as_str())
        .collect();
    if others.is_empty() {
        return;
    }
    let others = others.join(", ");
    if !opts.include_tags.is_empty() || !opts.exclude_tags.is_empty() {
        ctx.shell().warn(format!("tag filters only apply to JVM tests; the {others} tests run unfiltered"));
    }
    if ctx.ws.manifest().test.test_timeout.is_some() {
        ctx.shell().warn(format!("`[test] test-timeout` only applies to JVM tests; the {others} tests have no per-test limit"));
    }
    if opts.failed {
        ctx.shell().warn(format!("`--failed` only reruns JVM tests; running all of the {others} tests"));
    }
    if opts.coverage {
        ctx.shell().warn(format!("`--coverage` measures the JVM tests with JaCoCo; the {others} tests run without coverage"));
    }
}

/// A duration from `kargo.toml` as it reads in messages: `5m`, `30s`, `1h` or `500ms`.
fn describe_limit(limit: Duration) -> String {
    let ms = limit.as_millis();
    if ms > 0 && ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms > 0 && ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms > 0 && ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

/// Why a run stopped at the `[test] timeout`, for the error that says so.
fn timed_out_message_for_suite(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> String {
    let limit = suite_timeout(ctx, suite).map(describe_limit).unwrap_or_default();
    let where_ = match suite {
        None => "[test] timeout".to_owned(),
        Some(suite) => format!("[test.suites.{}] timeout", suite.name),
    };
    format!("timed out after {limit}; the whole test run is limited by `{where_}`")
}

/// Waits for a test process to exit. When `deadline` passes first, the process is killed.
/// Returns its status and whether it was killed for that reason.
pub(crate) fn wait_until(child: &mut Child, deadline: Option<Instant>) -> std::io::Result<(ExitStatus, bool)> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            return Ok((child.wait()?, true));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The launcher's (relocated) picocli setting for the terminal width; a number keeps it from running `stty`.
const JUNIT_USAGE_WIDTH_PROPERTY: &str = "org.junit.platform.console.shadow.picocli.usage.width";

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

/// The JUnit console launcher jar that runs the JVM tests (downloaded on first use, or by `kargo fetch`).
pub fn console_launcher(ctx: &BuildContext) -> Result<PathBuf> {
    ctx.tool_jar("org.junit.platform", "junit-platform-console-standalone", JUNIT_VERSION)
}

/// The JUnit console launcher command that runs (or lists) the compiled tests.
/// `rerun` names the test classes to run in place of scanning every test (`--failed`).
fn junit_command_for_suite(
    ctx: &BuildContext,
    opts: &TestOptions,
    tests: &JvmTestClasses,
    mode: JunitMode,
    rerun: Option<&[String]>,
    suite: Option<&kargo_manifest::TestSuite>,
) -> Result<Command> {
    let jdk = ctx.compiler_jdk()?;
    jdk.require_version(JUNIT_MIN_JDK).context("JVM tests run on JUnit 6; set `[toolchain] jdk` to a newer JDK")?;
    let jvm = ctx.ws.manifest().targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let launcher = console_launcher(ctx)?;
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
    // The JVM can't dump a class data archive with the debugger's agent loaded (and neither with JaCoCo's).
    if opts.debug.is_none() && !opts.coverage {
        cmd.args(kargo_compiler::daemon::cds_options(jdk, "junit", std::slice::from_ref(&launcher)));
    }
    // Same order as `kargo run`: `[target.jvm]`, the profile, then `[test]` and the suite, which is the most specific.
    let profile = ctx.ws.profile(&ctx.profile)?;
    cmd.args(jdk.default_run_args()).args(&jvm.jvm_args).args(&profile.jvm_args).args(suite_jvm_args(ctx, suite));
    if opts.coverage {
        let exec = super::coverage::exec_file(ctx);
        if let Some(parent) = exec.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        cmd.arg(super::coverage::agent_arg(ctx, &exec)?);
    }
    // Without a terminal the launcher's probe for the terminal width prints `stty: /dev/tty: No such device`.
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        cmd.arg(format!("-D{JUNIT_USAGE_WIDTH_PROPERTY}=120"));
    }
    if mode == JunitMode::List {
        cmd.arg(format!("-D{}=true", junit::LIST_PROPERTY));
    }
    let colors = mode == JunitMode::Run && std::io::IsTerminal::is_terminal(&std::io::stdout());
    // JUnit's own limit for each test, which a test that sleeps too long fails with.
    let test_timeout = suite_test_timeout(ctx, suite);
    let include_tags = suite_include_tags(opts, suite);
    let exclude_tags = suite_exclude_tags(opts, suite);
    cmd.arg("-jar")
        .arg(&launcher)
        .arg("execute")
        .arg("--disable-banner")
        .arg(if mode == JunitMode::List { "--details=none" } else { "--details=tree" })
        .args((!colors).then_some("--disable-ansi-colors"))
        .args(include_tags.iter().map(|tag| format!("--include-tag={tag}")))
        .args(exclude_tags.iter().map(|tag| format!("--exclude-tag={tag}")))
        .args(test_timeout.map(|limit| format!("--config=junit.jupiter.execution.timeout.default={} ms", limit.as_millis())));
    let joined = join_paths(&run_cp);
    if joined.len() >= process::LONG_CLASSPATH {
        // A long classpath would pass the limit of a command line on Windows (32,767 characters); the launcher reads `@argfiles`.
        let argfile_name = match suite {
            None => "test.args".to_owned(),
            Some(suite) => format!("test-{}.args", suite.name),
        };
        let argfile = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join(argfile_name);
        process::write_argfile(&argfile, &["--class-path".to_owned(), process::classpath_for_argfile(&run_cp)])?;
        cmd.arg(format!("@{}", process::slash(&argfile)));
    } else {
        cmd.arg("--class-path").arg(joined);
    }
    match rerun {
        // Only the classes that failed last time: scanning the whole output would run every test again.
        Some(classes) => {
            cmd.args(classes.iter().map(|class| format!("--select-class={class}")));
        }
        None => {
            cmd.arg("--scan-class-path").arg(&tests.classes);
        }
    }
    cmd.args(junit_filter_args(opts.filter.as_deref(), &test_class_names(&tests.classes)))
        .current_dir(suite_cwd(ctx, suite))
        .envs(suite_project_env(ctx, opts, suite)?.for_child());
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

fn run_junit_for_suite(
    ctx: &BuildContext,
    run: JunitRun,
    opts: &TestOptions,
    suite: Option<&kargo_manifest::TestSuite>,
    deadline: Option<Instant>,
) -> Result<bool> {
    let JunitRun { mut cmd, main_classes } = run;
    // The launcher writes `TEST-junit-jupiter.xml` (one file per test engine); `--failed` reads the failures from it.
    let reports = reports_dir_for_suite(ctx, TargetKind::Jvm, suite)?;
    cmd.arg("--reports-dir").arg(&reports);
    let label = suite_label(TargetKind::Jvm, suite);
    ctx.shell().status("Running", format!("{label} (JUnit Platform)"));
    if let Some(port) = opts.debug {
        super::run::note_debugger(ctx.shell(), port, "the test JVM");
    }
    let end = if diagnostics::is_json() { run_junit_events(ctx, cmd, deadline)? } else { run_junit_terminal(ctx, cmd, deadline)? };
    if opts.junit_report {
        wrote_reports(ctx, &reports);
    }
    if let Some(tally) = &end.events {
        emit_summary_for_suite(ctx, TargetKind::Jvm, suite, tally.passed, tally.failed, tally.ignored, end.status.success());
    }
    if end.timed_out {
        ctx.shell().error(format!("{} {}", label, timed_out_message_for_suite(ctx, suite)));
        record_jvm_failures_for_suite(ctx, None, suite)?;
        return Ok(false);
    }
    if let Some(reason) = end.abnormal() {
        ctx.shell().error(format!("test JVM {reason} before finishing"));
        record_jvm_failures_for_suite(ctx, None, suite)?;
        return Ok(false);
    }
    record_jvm_failures_for_suite(ctx, Some(&failed_classes(&reports)?), suite)?;
    let filter = opts.filter.as_deref();
    let tagged = !suite_include_tags(opts, suite).is_empty() || !suite_exclude_tags(opts, suite).is_empty();
    if end.status.code() == Some(JUNIT_NO_TESTS) {
        match filter {
            Some(filter) => ctx.shell().error(format!("no JVM tests match `{filter}`; the filter matches `package.Class.method` names")),
            None if tagged => ctx.shell().error("no JVM tests match the --include-tag and --exclude-tag filters"),
            None if opts.failed => ctx.shell().error("none of the test classes that failed in the last run has tests any more"),
            None => ctx.shell().error(
                "no JVM tests found; mark test functions with `@Test` (kotlin.test or JUnit) or add your test framework's \
                 JUnit Platform engine to [dev-dependencies]",
            ),
        }
    }
    let tests_passed = end.status.success();
    if opts.coverage && end.status.code() != Some(JUNIT_NO_TESTS) {
        let classfiles = super::coverage::classfiles(ctx, &main_classes);
        let sourcefiles = super::coverage::sourcefiles(ctx);
        let exec = super::coverage::exec_file(ctx);
        let (_, coverage_passed) = super::coverage::report(ctx, &exec, &classfiles, &sourcefiles, opts.fail_under)?;
        return Ok(tests_passed && coverage_passed);
    }
    Ok(tests_passed)
}

/// How the JUnit console launcher ended.
struct JvmEnd {
    status: ExitStatus,
    /// Killed because the whole test run passed its `[test] timeout`.
    timed_out: bool,
    /// What the listener reported (`--message-format json`); `None` when the launcher printed to the terminal.
    events: Option<Tally>,
}

/// How many tests the listener saw finish, and whether it saw the test plan end.
#[derive(Debug, Default)]
struct Tally {
    passed: usize,
    failed: usize,
    ignored: usize,
    /// The listener reported the end of the test plan. A run that never reports it was cut short.
    finished: bool,
}

impl JvmEnd {
    /// Why the launcher stopped before the tests ran to the end, if it did: a test called `System.exit`,
    /// the JVM ran out of memory, and so on. A failed test also ends the launcher with exit code 1, so
    /// that counts as an early stop only when the listener did not see the test plan end.
    fn abnormal(&self) -> Option<String> {
        match self.status.code() {
            Some(0) | Some(JUNIT_NO_TESTS) => None,
            Some(1) if self.events.as_ref().is_none_or(|tally| tally.finished) => None,
            Some(code) => Some(format!("exited with code {code}")),
            None => Some("was terminated by a signal".to_owned()),
        }
    }
}

/// Runs the launcher with its output going straight to the terminal, until it exits or `deadline` passes.
fn run_junit_terminal(ctx: &BuildContext, mut cmd: Command, deadline: Option<Instant>) -> Result<JvmEnd> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.spawn().context("failed to start the JUnit console launcher")?;
    let (status, timed_out) = wait_until(&mut child, deadline)?;
    Ok(JvmEnd { status, timed_out, events: None })
}

/// Runs the launcher with Kargo's listener, passing each test message on with the package
/// and target added, and the launcher's own output to stderr. The output is read on its own
/// thread, so `deadline` still ends a run whose tests hang.
fn run_junit_events(ctx: &BuildContext, mut cmd: Command, deadline: Option<Instant>) -> Result<JvmEnd> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.stdout(Stdio::piped()).spawn().context("failed to start the JUnit console launcher")?;
    let stdout = child.stdout.take().context("missing test stdout")?;
    let package = ctx.ws.manifest().package.name.clone();
    let reader = std::thread::spawn(move || -> std::io::Result<Tally> {
        let mut tally = Tally::default();
        let mut err = anstream::stderr();
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            match junit::parse_event(&line) {
                Some(message) => {
                    match message.get("reason").and_then(|r| r.as_str()) {
                        Some("test-finished") => match message.get("outcome").and_then(|o| o.as_str()) {
                            Some("passed") => tally.passed += 1,
                            Some("ignored") => tally.ignored += 1,
                            _ => tally.failed += 1,
                        },
                        // Kargo's own marker of the end of the test plan, not a message for the JSON stream.
                        Some("run-finished") => {
                            tally.finished = true;
                            continue;
                        }
                        _ => {}
                    }
                    emit_event(&package, TargetKind::Jvm, message);
                }
                // The listener starts each message on a new line, which leaves blank lines behind.
                None if line.trim().is_empty() => {}
                None => writeln!(err, "{line}")?,
            }
        }
        Ok(tally)
    });
    let (status, timed_out) = wait_until(&mut child, deadline)?;
    let tally = reader.join().map_err(|_| anyhow::anyhow!("the test output reader panicked"))??;
    Ok(JvmEnd { status, timed_out, events: Some(tally) })
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
    let test_sources = sources(ctx, TargetKind::Jvm, true, &["kt", "java"])?;
    if test_sources.is_empty() {
        return Ok(None);
    }
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    // The test fixtures of this package and of the `test-fixtures = true` dev-dependencies come first.
    let mut test_cp = super::local::test_fixtures(ctx)?;
    test_cp.extend(super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &all_scopes, true)?);
    test_cp.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let test_classes = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("test-classes");
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version)?;

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(test_cp);
    let name = format!("{}-test", manifest.package.name);
    let mut test_sources = test_sources;
    let mut resources = ctx.ws.test_resource_dirs();
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

/// Compiles the JVM test fixtures against the main jar,
/// Like [`compile_jvm_tests`], for the test suite `suite` (`None` is the default `unit` suite).
/// Compiles the suite's sources against `main_classes` into `test-classes` (`unit`)
/// or `test-classes-<name>` (a declared suite).
pub(crate) fn compile_jvm_tests_for_suite(
    ctx: &BuildContext,
    main_classes: &Path,
    verb: &str,
    suite: Option<&kargo_manifest::TestSuite>,
) -> Result<Option<JvmTestClasses>> {
    use super::build::{jvm_processor_path_for_suite, run_jvm_ksp_for_suite, suite_sources};
    let manifest = ctx.ws.manifest();
    let test_sources = if suite.is_none() {
        sources(ctx, TargetKind::Jvm, true, &["kt", "java"])?
    } else {
        suite_sources(ctx, TargetKind::Jvm, suite, &["kt", "java"])?
    };
    if test_sources.is_empty() {
        return Ok(None);
    }
    let setup = ctx.setup_for_suite(TargetKind::Jvm, suite)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &ctx.setup(TargetKind::Jvm)?)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    // The test fixtures of this package and of the `test-fixtures = true` dev-dependencies come first.
    let mut test_cp = super::local::test_fixtures(ctx)?;
    test_cp.extend(super::local::artifacts_for_suite(ctx, super::local::LocalTarget::Jvm, &all_scopes, true, suite)?);
    let test_key = suite_test_key("jvm/test", suite);
    test_cp.extend(ctx.resolve(&test_key, Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let test_classes = test_classes_dir(ctx, suite);
    let processor_path = jvm_processor_path_for_suite(ctx, &setup, true, suite)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version)?;

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(test_cp);
    let name = match suite {
        None => format!("{}-test", manifest.package.name),
        Some(suite) => format!("{}-test-{}", manifest.package.name, suite.name),
    };
    let mut test_sources = test_sources;
    let mut resources = match suite {
        None => ctx.ws.test_resource_dirs(),
        Some(suite) => {
            // The default test resources plus the suite's own; missing suite directories are left out
            // (the defaults may be generated later, and an empty classpath entry is harmless).
            let defaults = ctx.ws.test_resource_dirs();
            let mut resources = defaults.clone();
            resources.push(ctx.ws.root().join(&suite.resources));
            resources.dedup();
            resources.into_iter().filter(|p| defaults.contains(p) || p.is_dir()).collect()
        }
    };
    if resources.is_empty() {
        resources = ctx.ws.test_resource_dirs();
    }
    let ksp = JvmKspUnit {
        name: &name,
        test: true,
        libraries: classpath.clone(),
        friends: vec![main_classes.to_path_buf()],
        jdk,
        jvm_target: &jvm_target,
    };
    run_jvm_ksp_for_suite(ctx, &setup, ksp, &mut test_sources, &mut resources, suite)?;
    let what = match suite {
        None => ", tests".to_owned(),
        Some(suite) => format!(", {} tests", suite.name),
    };
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
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, verb, TargetKind::Jvm, &what))?;
    Ok(Some(JvmTestClasses { classes: test_classes, classpath }))
}

/// its dependencies and dev-dependencies, into `<name>-<version>-test-fixtures.jar`. Returns `None`
/// without fixture sources.
pub(crate) fn build_jvm_test_fixtures(ctx: &BuildContext) -> Result<Option<PathBuf>> {
    let ws = ctx.ws;
    let manifest = ws.manifest();
    let collect = |dirs: &[PathBuf]| dirs.iter().flat_map(|d| collect_files(d, &["kt", "java"])).collect::<Vec<_>>();
    let mut fixtures = SourceSet::new(
        collect(&ws.common_test_fixture_dirs()),
        collect(&ws.target_test_fixture_dirs(TargetKind::Jvm)),
        TargetKind::Jvm.as_str(),
    );
    fixtures.groups = manifest
        .source_groups_for(TargetKind::Jvm)
        .into_iter()
        .map(|(g, parents)| kargo_compiler::SourceGroup::new(&g.name, collect(&ws.group_test_fixture_dirs(&g.name)), &parents))
        .collect();
    if fixtures.is_empty() {
        return Ok(None);
    }
    let main = super::build::build_jvm(ctx)?;
    let setup = ctx.setup(TargetKind::Jvm)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut classpath = vec![main.jar.clone()];
    classpath.extend(super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &all_scopes, true)?);
    classpath.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let out = ws.output_dir(&ctx.profile, TargetKind::Jvm);
    let name = format!("{}-test-fixtures", manifest.package.name);
    let unit = JvmCompilation {
        name: &name,
        sources: fixtures,
        classpath,
        // Fixtures may use the package's `internal` declarations, like its tests.
        friend_paths: vec![main.classes.clone()],
        output: out.join("test-fixtures-classes"),
        jvm_target: &jvm.target_for(jdk.version)?,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &jvm_javac_args(&setup, jvm),
        processor_path: &jvm_processor_path(ctx, &setup, true)?,
        resources: Vec::new(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let outcome = unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, "Compiling", TargetKind::Jvm, ", test fixtures"))?;
    let jar = out.join(format!("{}-{}-test-fixtures.jar", manifest.package.name, manifest.package.version));
    if outcome == Outcome::Compiled || !jar.is_file() {
        write_jar(&jar, &unit.output, &JarManifest::default(), &[])?;
        kargo_compiler::abi::update_stamp(&jar)?;
    }
    Ok(Some(jar))
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
fn run_reported_for_suite(
    ctx: &BuildContext,
    mut cmd: Command,
    label: &str,
    target: TargetKind,
    opts: &TestOptions,
    suite: Option<&kargo_manifest::TestSuite>,
    deadline: Option<Instant>,
) -> Result<bool> {
    ctx.shell().status("Running", label);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let started = Instant::now();
    let mut child = cmd.stdout(Stdio::piped()).spawn().with_context(|| format!("failed to start {label}"))?;
    let stdout = child.stdout.take().context("missing test stdout")?;
    let package = ctx.ws.manifest().package.name.clone();
    let json = diagnostics::is_json();
    // The report is read on its own thread, so a test that hangs still reaches the deadline.
    let reader = std::thread::spawn(move || {
        testreport::report_with(BufReader::new(stdout), human_out(), |event| {
            if json {
                emit_report_event(&package, target, event);
            }
        })
    });
    let (status, timed_out) = wait_until(&mut child, deadline)?;
    let summary = reader.join().map_err(|_| anyhow::anyhow!("the test reporter panicked"))??;
    // Runners exit non-zero when a test failed; without a failed test that means a crash.
    let abnormal = if timed_out {
        Some(timed_out_message_for_suite(ctx, suite))
    } else {
        (!status.success()).then(|| {
            let how = format!("exited with {}", process::describe_status(status));
            if summary.ran_tests() { how } else { format!("{how} before running any tests") }
        })
    };
    let ending = Ending { abnormal, timed_out, elapsed: started.elapsed() };
    finish_reported_for_suite(ctx, summary, label, target, opts, suite, ending)
}

/// How a test process that reports TeamCity messages ended.
struct Ending {
    /// What went wrong, when the process did not finish normally.
    abnormal: Option<String>,
    /// Killed at the `[test] timeout`, which is certain to have ended the run early.
    timed_out: bool,
    /// How long the process ran.
    elapsed: Duration,
}

/// Writes the reports and prints the summary of a test run. `ending.abnormal` says how the runner
/// ended when it did not finish normally (`exited with exit code 1`); that only counts as a crash
/// when no test failed, since runners also end that way when a test failed. A timeout always does.
fn finish_reported_for_suite(
    ctx: &BuildContext,
    mut summary: testreport::TestSummary,
    label: &str,
    target: TargetKind,
    opts: &TestOptions,
    suite: Option<&kargo_manifest::TestSuite>,
    ending: Ending,
) -> Result<bool> {
    let Ending { abnormal, timed_out, elapsed } = ending;
    if opts.junit_report {
        let dir = reports_dir_for_suite(ctx, target, suite)?;
        let file = dir.join(format!("TEST-{target}.xml"));
        let package = &ctx.ws.manifest().package.name;
        let title = match suite {
            None => format!("{package} ({target})"),
            Some(suite) => format!("{package} ({target}, {})", suite.name),
        };
        testreport::write_junit_xml(&file, &title, &summary.cases).with_context(|| format!("failed to write {}", file.display()))?;
        wrote_reports(ctx, &dir);
    }
    summary.crashed = abnormal.is_some() && (timed_out || summary.failed == 0);
    emit_summary_for_suite(ctx, target, suite, summary.passed, summary.failed, summary.ignored, summary.success());
    if summary.crashed && !summary.ran_tests() {
        ctx.shell().error(format!("{label} {}", abnormal.unwrap_or_default()));
        return Ok(false);
    }
    testreport::print_summary(&summary, Some(elapsed), human_out())?;
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
fn run_in_browser_for_suite(
    ctx: &BuildContext,
    run: BrowserRun,
    target: TargetKind,
    opts: &TestOptions,
    suite: Option<&kargo_manifest::TestSuite>,
    deadline: Option<Instant>,
) -> Result<bool> {
    let started = Instant::now();
    let (events_tx, events) = mpsc::channel();
    let server = web::Server::start(web::bind(0)?, run.site, Some(events_tx))?;
    let label = run.label;
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
                emit_report_event(&package, target, event);
            }
        })
    });
    let outcome = wait_for_page(&mut child, &events, &lines_tx, browser_timeout(), deadline);
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
    let timed_out = outcome.is_err() && deadline.is_some_and(|deadline| Instant::now() >= deadline);
    let abnormal = if timed_out { Some(timed_out_message_for_suite(ctx, suite)) } else { outcome.err() };
    finish_reported_for_suite(ctx, summary, label, target, opts, suite, Ending { abnormal, timed_out, elapsed: started.elapsed() })
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
    deadline: Option<Instant>,
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
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err("the test run passed its time limit".to_owned());
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

fn test_wasm_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<TestRun> {
    use super::build::{suite_sources, wasm_libraries_for_suite};
    let manifest = ctx.ws.manifest();
    let wasm = manifest.targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let tests = suite_sources(ctx, TargetKind::Wasm, suite, &["kt"])?;
    if tests.kotlin_files().is_empty() {
        return Ok(no_tests_for_suite(ctx, TargetKind::Wasm, suite));
    }
    super::local::test_fixtures_unsupported(ctx, TargetKind::Wasm)?;
    // Tests link against the main klib as a friend module, so the program's
    // `main` is not part of the test binary.
    let main = prepare_wasm(ctx)?;
    let setup = ctx.setup_for_suite(TargetKind::Wasm, suite)?;
    let libraries = wasm_libraries_for_suite(ctx, &setup, true, suite)?;
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    super::build::npm_opt_in(ctx, TargetKind::Wasm, &mut kotlin_args);
    let name = match suite {
        None => format!("{}-test", wasm.module_name(&manifest.package)),
        Some(suite) => format!("{}-test-{}", wasm.module_name(&manifest.package), suite.name),
    };
    let main_node_modules = main.as_ref().and_then(|m| m.node_modules.clone());
    let output = wasm_test_dir(ctx, suite);
    let unit = WasmCompilation {
        name: &name,
        environment: wasm.environment,
        sources: tests,
        libraries,
        friends: main.map(|m| vec![m.klib]).unwrap_or_default(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: output.clone(),
        debug: true,
        debug_info: false,
        optimize: false,
        array_range_checks: wasm.wasm_enable_array_range_checks,
        debugger_custom_formatters: wasm.wasm_debugger_custom_formatters,
        declarations: false,
        main_args: false,
    };
    let what = match suite {
        None => ", tests".to_owned(),
        Some(suite) => format!(", {} tests", suite.name),
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Wasm), &|| compiling(ctx, TargetKind::Wasm, &what))?;
    // The main module's JavaScript and TypeScript modules, which the test module imports from its own directory.
    let main_dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Wasm);
    crate::js_sources::install(ctx, TargetKind::Wasm, &unit.output, &main_dir, main_node_modules.as_deref())?;
    crate::js_sources::alias_module(ctx.ws, TargetKind::Wasm, &unit.output, wasm.module_name(&manifest.package), &name)?;
    // `prepare_wasm` installed the main npm packages one level up; dev-dependencies
    // that need more get their own `node_modules` next to the test module.
    let main_libs = wasm_libraries_for_suite(ctx, &ctx.setup_for_suite(TargetKind::Wasm, None)?, false, None)?;
    let node_modules = if main_node_modules.is_none()
        || npm_dependencies(&unit.libraries)? != npm_dependencies(&main_libs)?
        || !crate::npm::dev_dependencies(manifest, TargetKind::Wasm).is_empty()
        || suite.is_some()
    {
        crate::npm::install_for_tests(ctx, TargetKind::Wasm, &unit.libraries, &unit.output)?
    } else {
        main_node_modules
    };
    if wasm.environment == WasmEnvironment::Browser {
        return browser_tests_for_suite(ctx, opts, &unit, node_modules, suite);
    }
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    if opts.filter.is_some() {
        ctx.shell().warn("test filters are not supported for the wasm target; running all tests");
    }
    let mut cmd = Command::new(kargo_toolchain::node()?);
    cmd.arg("-e")
        .arg(format!("import({:?}).then(m => m.startUnitTests())", process::file_url(&unit.module_path())))
        .current_dir(suite_cwd(ctx, suite))
        .envs(suite_project_env(ctx, opts, suite)?.for_child());
    let label: &'static str = "Wasm tests (Node.js)";
    Ok(TestRun::Reported(cmd, label))
}

/// Writes the page that runs the compiled Wasm tests and finds the browser to run it in.
fn browser_tests_for_suite(
    ctx: &BuildContext,
    opts: &TestOptions,
    unit: &WasmCompilation,
    node_modules: Option<PathBuf>,
    _suite: Option<&kargo_manifest::TestSuite>,
) -> Result<TestRun> {
    let imports = node_modules.as_deref().map(crate::wasm_output::import_map).unwrap_or_default();
    // The tests get the web resources of the package's libraries and of the package itself, after theirs.
    let mut shipments = crate::web_resources::dependencies(ctx, true)?;
    shipments.extend(crate::web_resources::own(ctx.ws, false)?);
    crate::wasm_output::copy_site_files(&unit.output, &[], &shipments)?;
    let inits = crate::web_resources::init_scripts(&shipments);
    std::fs::write(unit.output.join("index.html"), crate::wasm_output::test_page(unit.name, &imports, &inits))
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
    let label: &'static str = "Wasm tests (headless browser)";
    Ok(TestRun::Browser(BrowserRun { label, browser, site, profile }))
}

/// Compiles the Kotlin/JS tests into an ES module below the main module and prepares to run them on
/// Node.js or in a headless browser, with the runner of [`crate::js_output`].
fn test_js_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<TestRun> {
    use super::build::{js_libraries_for_suite, suite_sources};
    let manifest = ctx.ws.manifest();
    let js = manifest.targets.js.as_ref().context("the JS target is not enabled")?;
    let tests = suite_sources(ctx, TargetKind::Js, suite, &["kt"])?;
    if tests.kotlin_files().is_empty() {
        return Ok(no_tests_for_suite(ctx, TargetKind::Js, suite));
    }
    super::local::test_fixtures_unsupported(ctx, TargetKind::Js)?;
    // Tests link against the main klib as a friend module, so the program's `main` does not run.
    let main = prepare_js(ctx)?;
    let setup = ctx.setup_for_suite(TargetKind::Js, suite)?;
    let libraries = js_libraries_for_suite(ctx, &setup, true, suite)?;
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    super::build::npm_opt_in(ctx, TargetKind::Js, &mut kotlin_args);
    let name = match suite {
        None => format!("{}-test", js.module_name(&manifest.package)),
        Some(suite) => format!("{}-test-{}", js.module_name(&manifest.package), suite.name),
    };
    let main_node_modules = main.as_ref().and_then(|m| m.node_modules.clone());
    // Below the main module, so Node.js finds its npm packages from here too.
    let output = js_test_dir(ctx, suite);
    let unit = JsCompilation {
        name: &name,
        environment: js.environment,
        // Whatever the program is, the runner imports the tests as an ES module.
        module_kind: JsModuleKind::Es,
        sources: tests,
        libraries,
        friends: main.map(|m| vec![m.klib]).unwrap_or_default(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: output.clone(),
        work: js_work_dir(ctx, &name),
        root: ctx.ws.root().to_path_buf(),
        debug: true,
        optimize: false,
        declarations: false,
        main_args: false,
    };
    let what = match suite {
        None => ", tests".to_owned(),
        Some(suite) => format!(", {} tests", suite.name),
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Js), &|| compiling(ctx, TargetKind::Js, &what))?;
    // The main module's JavaScript and TypeScript modules, which the test module imports from its own directory.
    let main_dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Js);
    crate::js_sources::install(ctx, TargetKind::Js, &output, &main_dir, main_node_modules.as_deref())?;
    crate::js_sources::alias_module(ctx.ws, TargetKind::Js, &output, js.module_name(&manifest.package), &name)?;
    // The main npm packages are one level up; dev-dependencies that need more get their own.
    let main_libs = js_libraries_for_suite(ctx, &ctx.setup_for_suite(TargetKind::Js, None)?, false, None)?;
    let node_modules = if main_node_modules.is_none()
        || npm_dependencies(&unit.libraries)? != npm_dependencies(&main_libs)?
        || !crate::npm::dev_dependencies(manifest, TargetKind::Js).is_empty()
        || suite.is_some()
    {
        crate::npm::install_for_tests(ctx, TargetKind::Js, &unit.libraries, &output)?
    } else {
        main_node_modules
    };
    let browser = js.environment == JsEnvironment::Browser;
    let imports = match (&node_modules, browser) {
        (Some(node_modules), true) => crate::wasm_output::import_map(node_modules),
        _ => Default::default(),
    };
    crate::js_output::write_test_files(&output, &name, browser, opts.filter.as_deref(), &imports)?;
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    if browser {
        let browser = browser::browser()?;
        let mut site = Site::new(&output);
        if let Some(node_modules) = node_modules {
            site = site.mount("node_modules", node_modules);
        }
        let profile = output.join(format!(".browser-profile-{}", std::process::id()));
        return Ok(TestRun::Browser(BrowserRun { label: "Kotlin/JS tests (headless browser)", browser, site, profile }));
    }
    let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
    let script = format!(
        "import({}).then(r => r.run(() => import({}), {{ filter: {} }})).then(failed => {{ process.exitCode = failed ? 1 : 0 }})",
        quoted(&process::file_url(&output.join(crate::js_output::TEST_RUNNER))),
        quoted(&process::file_url(&unit.module_path())),
        quoted(opts.filter.as_deref().unwrap_or_default()),
    );
    let mut cmd = Command::new(kargo_toolchain::node()?);
    cmd.arg("--enable-source-maps")
        .arg("-e")
        .arg(script)
        .current_dir(suite_cwd(ctx, suite))
        .envs(suite_project_env(ctx, opts, suite)?.for_child());
    Ok(TestRun::Reported(cmd, "Kotlin/JS tests (Node.js)"))
}

fn test_native_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<TestRun> {
    super::local::test_fixtures_unsupported(ctx, TargetKind::Native)?;
    let Some(executable) = compile_native_tests_for_suite(ctx, suite)? else {
        return Ok(TestRun::Done(true));
    };
    if opts.no_run {
        return Ok(TestRun::Done(true));
    }
    let mut cmd = Command::new(executable);
    cmd.arg("--ktest_logger=TEAMCITY").current_dir(suite_cwd(ctx, suite)).envs(suite_project_env(ctx, opts, suite)?.for_child());
    if let Some(filter) = &opts.filter {
        cmd.arg(format!("--ktest_filter=*{filter}*"));
    }
    Ok(TestRun::Reported(cmd, "native tests"))
}

/// Compiles the Kotlin/Native tests for this host into a test executable. Returns `None`
/// (after saying why) when there is nothing to run.
fn compile_native_tests_for_suite(ctx: &BuildContext, suite: Option<&kargo_manifest::TestSuite>) -> Result<Option<PathBuf>> {
    use super::build::{native_libraries_for_suite, suite_sources};
    let manifest = ctx.ws.manifest();
    let tests = suite_sources(ctx, TargetKind::Native, suite, &["kt"])?;
    if tests.kotlin_files().is_empty() {
        no_tests_for_suite(ctx, TargetKind::Native, suite);
        return Ok(None);
    }
    let host = Host::current()?.konan_target;
    let platforms = native_platforms(ctx)?;
    if !platforms.iter().any(|p| p == host) {
        ctx.shell().warn(format!("skipping native tests: this host ({host}) is not in `platforms`"));
        return Ok(None);
    }
    let setup = ctx.setup_for_suite(TargetKind::Native, suite)?;
    let _ = native_libraries_for_suite(ctx, &ctx.setup_for_suite(TargetKind::Native, None)?, host, false, None)?;
    let mut libraries = native_libraries_for_suite(ctx, &setup, host, true, suite)?;
    libraries.extend(native_cinterop(ctx, host)?);
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut all = for_platform(ctx, sources(ctx, TargetKind::Native, false, &["kt"])?, host);
    all.extend(&for_platform(ctx, tests, host));
    let name = match suite {
        None => format!("{}-test", manifest.package.name),
        Some(suite) => format!("{}-test-{}", manifest.package.name, suite.name),
    };
    let output_dir = native_test_dir(ctx, host, suite);
    let what = match suite {
        None => ", tests".to_owned(),
        Some(suite) => format!(", {} tests", suite.name),
    };
    let unit = NativeCompilation {
        name: &name,
        konan_target: host.to_owned(),
        kind: NativeKind::Executable,
        sources: all,
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point: None,
        link: crate::native_interop::native_link(ctx, host)?,
        output_dir,
        debug: true,
        optimize: false,
        test_runner: true,
        exported: Vec::new(),
        bundle_id: None,
    };
    let outcome =
        unit.compile(ctx.shell(), toolchain, &ctx.fingerprint_dir(TargetKind::Native), &|| compiling(ctx, TargetKind::Native, &what))?;
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
    let targets = opts.build.select(ctx)?;
    let suites = resolve_suites_for_package(ctx, opts)?;
    if suites.is_empty() {
        ctx.shell().note(format!("package `{}` has no test suite `{}`; skipping it", ctx.ws.name(), opts.suites.join(", ")));
        return Ok(());
    }
    let package = &ctx.ws.manifest().package.name;
    let mut out = anstream::stdout();
    for selection in &suites {
        let suite = selection.suite.as_ref();
        let listed = super::parallel_targets(&targets, |target| match target {
            TargetKind::Jvm => list_jvm_for_suite(ctx, opts, suite),
            TargetKind::Native => list_native_for_suite(ctx, opts, suite),
            TargetKind::Wasm | TargetKind::Js | TargetKind::Android => {
                if suite.is_none() {
                    ctx.shell().note(format!("`--list` does not list {target} tests yet"));
                } else {
                    ctx.shell().note(format!("`--list` does not list {target} tests of the `{}` suite yet", selection.name));
                }
                Ok(Vec::new())
            }
        });
        for (&target, tests) in targets.iter().zip(listed) {
            let mut tests = tests?;
            let test_sources = if suite.is_none() {
                sources(ctx, target, true, &["kt", "java"])?
            } else {
                use super::build::suite_sources;
                suite_sources(ctx, target, suite, &["kt", "java"])?
            };
            locate_tests(&mut tests, &test_sources.all().cloned().collect::<Vec<_>>());
            for test in &tests {
                match format {
                    ListFormat::Text => {
                        if selection.is_unit() {
                            writeln!(out, "{target}: {}", test.full_name())?;
                        } else {
                            writeln!(out, "{target} ({}): {}", selection.name, test.full_name())?;
                        }
                    }
                    ListFormat::Json => {
                        let mut json = serde_json::json!({
                            "reason": "test-discovered",
                            "id": test.id,
                            "parent-id": test.parent_id,
                            "suite": test.suite,
                            "name": test.name,
                            "display-name": test.display_name,
                            "file": test.file.as_ref().map(|f| f.display().to_string()),
                            "line": test.line,
                        });
                        if let Some(suite) = suite
                            && let serde_json::Value::Object(map) = &mut json
                        {
                            map.insert("test-suite".to_owned(), suite.name.clone().into());
                        }
                        if let serde_json::Value::Object(message) = json {
                            emit_event(package, target, message);
                        }
                    }
                }
            }
            let where_ = if selection.is_unit() { format!("{target}") } else { format!("{target} ({})", selection.name) };
            ctx.shell().status("Listed", format!("{} {where_} test{}", tests.len(), if tests.len() == 1 { "" } else { "s" }));
        }
    }
    Ok(())
}

fn list_jvm_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<Vec<ListedTest>> {
    let main = build_jvm(ctx)?;
    let Some(tests) = compile_jvm_tests_for_suite(ctx, &main.classes, "Compiling", suite)? else {
        no_tests_for_suite(ctx, TargetKind::Jvm, suite);
        return Ok(Vec::new());
    };
    let mut cmd = junit_command_for_suite(ctx, opts, &tests, JunitMode::List, None, suite)?;
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

fn list_native_for_suite(ctx: &BuildContext, opts: &TestOptions, suite: Option<&kargo_manifest::TestSuite>) -> Result<Vec<ListedTest>> {
    let Some(executable) = compile_native_tests_for_suite(ctx, suite)? else {
        return Ok(Vec::new());
    };
    let mut cmd = Command::new(&executable);
    cmd.arg("--ktest_list_tests")
        .current_dir(suite_cwd(ctx, suite))
        .envs(suite_project_env(ctx, opts, suite)?.for_child())
        .stdin(Stdio::null());
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
    fn describes_limits_as_written_in_kargo_toml() {
        let limit = |ms| describe_limit(Duration::from_millis(ms));
        assert_eq!((limit(300_000), limit(30_000), limit(3_600_000)), ("5m".to_owned(), "30s".to_owned(), "1h".to_owned()));
        assert_eq!((limit(500), limit(90_000), limit(1_500)), ("500ms".to_owned(), "90s".to_owned(), "1500ms".to_owned()));
    }

    #[test]
    fn finds_failed_classes_in_junit_reports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("TEST-junit-jupiter.xml"),
            r#"<testsuites><testsuite name="a">
                <testcase classname="app.MathTest" name="adds()" time="0.01"/>
                <testcase classname="app.MathTest" name="fails()" time="0.02"><failure message="no"/></testcase>
                <testcase classname="app.Outer$Inner" name="deep()"><error message="boom"/></testcase>
                <testcase classname="app.Passes" name="ok()"/>
            </testsuite></testsuites>"#,
        )
        .unwrap();
        assert_eq!(failed_classes(dir.path()).unwrap(), ["app.MathTest", "app.Outer$Inner"]);
        assert!(failed_classes(&dir.path().join("missing")).unwrap().is_empty());
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(code as u32)
    }

    #[test]
    fn explains_how_the_launcher_ended() {
        let end = |code, events: Option<bool>| JvmEnd {
            status: exit_status(code),
            timed_out: false,
            events: events.map(|finished| Tally { finished, ..Tally::default() }),
        };
        assert_eq!(end(0, Some(true)).abnormal(), None);
        assert_eq!(end(JUNIT_NO_TESTS, None).abnormal(), None);
        // A failed test ends the launcher with 1, which is not an early stop...
        assert_eq!(end(1, None).abnormal(), None);
        assert_eq!(end(1, Some(true)).abnormal(), None);
        // ...unless the listener never saw the test plan end, as when the JVM ran out of memory.
        assert_eq!(end(1, Some(false)).abnormal().as_deref(), Some("exited with code 1"));
        // A test that called `System.exit(5)`.
        assert_eq!(end(5, None).abnormal().as_deref(), Some("exited with code 5"));
    }

    #[cfg(unix)]
    #[test]
    fn kills_a_process_that_passes_its_deadline() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let (status, timed_out) = wait_until(&mut child, Some(Instant::now() + Duration::from_millis(100))).unwrap();
        assert!(timed_out);
        assert!(!status.success());
        let mut quick = Command::new("true").spawn().unwrap();
        let (status, timed_out) = wait_until(&mut quick, Some(Instant::now() + Duration::from_secs(30))).unwrap();
        assert_eq!((status.success(), timed_out), (true, false));
    }
}
