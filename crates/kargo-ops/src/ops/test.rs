//! `kargo test`: compiles and runs tests for each target.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anstyle::AnsiColor;
use anyhow::{Context, Result};

use super::build::{
    BuildOptions, JvmKspUnit, announce, build_jvm, compiling, finished, hook_env, jvm_classpaths, jvm_javac_args, jvm_processor_path,
    native_cinterop, native_libraries, native_platforms, run_jvm_ksp, select_targets, sources, wasm_libraries,
};
use super::context::BuildContext;
use crate::builtins::JUNIT_VERSION;
use crate::warnings;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::testreport;
use kargo_compiler::wasm::{WasmCompilation, npm_dependencies};
use kargo_compiler::{Outcome, common_kotlin_args};
use kargo_manifest::{Hook, NativeKind, Scope, TargetKind};
use kargo_maven::Platform;
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
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.build.targets)?;
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
        print_target_table(&outcomes, anstream::stdout())?;
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
    /// Gradle compiles and runs Android tests in one step.
    Android,
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

    let jdk = ctx.compiler_jdk()?;
    let jvm = ctx.ws.manifest().targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let launcher = ctx.tool_jar("org.junit.platform", "junit-platform-console-standalone", JUNIT_VERSION)?;
    let mut run_cp = vec![test_classes.clone()];
    run_cp.extend(classpath);
    let mut cmd = Command::new(jdk.java());
    // The launcher loads the tests in its own class loader, so its JVM classpath is just the launcher.
    cmd.args(kargo_compiler::daemon::cds_options(jdk, "junit", std::slice::from_ref(&launcher)))
        .args(&jvm.jvm_args)
        .arg("-jar")
        .arg(&launcher)
        .arg("execute")
        .arg("--disable-banner")
        .arg("--details=tree")
        .args((!std::io::IsTerminal::is_terminal(&std::io::stdout())).then_some("--disable-ansi-colors"))
        .arg("--class-path")
        .arg(join_paths(&run_cp))
        .arg("--scan-class-path")
        .arg(&test_classes)
        .arg("--fail-if-no-tests")
        .args(junit_filter_args(opts.filter.as_deref(), &test_class_names(&test_classes)))
        .current_dir(ctx.ws.root());
    Ok(TestRun::Junit(cmd))
}

fn run_junit(ctx: &BuildContext, mut cmd: Command, opts: &TestOptions) -> Result<bool> {
    let reports = opts.junit_report.then(|| reports_dir(ctx, TargetKind::Jvm)).transpose()?;
    if let Some(dir) = &reports {
        // The launcher writes `TEST-junit-jupiter.xml` (one file per test engine).
        cmd.arg("--reports-dir").arg(dir);
    }
    ctx.shell().status("Running", "JVM tests (JUnit Platform)");
    let status = process::status(ctx.shell(), &mut cmd, "the JUnit console launcher")?;
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
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let test_classes = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join("test-classes");
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let jvm_target = jvm.target_for(jdk.version);

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
    let mut summary = testreport::report(BufReader::new(stdout), anstream::stdout())?;
    let status = child.wait()?;
    if opts.junit_report {
        let dir = reports_dir(ctx, target)?;
        let file = dir.join(format!("TEST-{target}.xml"));
        let package = &ctx.ws.manifest().package.name;
        testreport::write_junit_xml(&file, &format!("{package} ({target})"), &summary.cases)
            .with_context(|| format!("failed to write {}", file.display()))?;
        wrote_reports(ctx, &dir);
    }
    // Runners exit non-zero when a test failed; without a failed test that means a crash.
    summary.crashed = !status.success() && summary.failed == 0;
    if summary.crashed && !summary.ran_tests() {
        ctx.shell().error(format!("{label} exited with {} before running any tests", process::describe_status(status)));
        return Ok(false);
    }
    testreport::print_summary(&summary, anstream::stdout())?;
    if summary.crashed {
        ctx.shell().error(format!("{label} exited with {}", process::describe_status(status)));
    }
    Ok(summary.success())
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
    let main = super::build::build_wasm(ctx)?;
    let setup = ctx.setup(TargetKind::Wasm)?;
    let libraries = wasm_libraries(ctx, &setup, true)?;
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let name = format!("{}-test", manifest.package.name);
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
        main_args: false,
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Wasm), &|| compiling(ctx, TargetKind::Wasm, ", tests"))?;
    // `build_wasm` installed the main npm packages one level up; dev-dependencies
    // that need more get their own `node_modules` next to the test module.
    if npm_dependencies(&unit.libraries)? != npm_dependencies(&wasm_libraries(ctx, &setup, false)?)? {
        crate::npm::install(ctx, &unit.libraries, &unit.output)?;
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

fn test_native(ctx: &BuildContext, opts: &TestOptions) -> Result<TestRun> {
    let manifest = ctx.ws.manifest();
    let tests = sources(ctx, TargetKind::Native, true, &["kt"]);
    if tests.kotlin_files().is_empty() {
        return Ok(no_tests(ctx, TargetKind::Native));
    }
    let host = Host::current()?.konan_target;
    let platforms = native_platforms(ctx)?;
    if !platforms.iter().any(|p| p == host) {
        ctx.shell().warn(format!("skipping native tests: this host ({host}) is not in `platforms`"));
        return Ok(TestRun::Done(true));
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
    if opts.no_run || outcome == Outcome::Empty {
        return Ok(TestRun::Done(true));
    }
    let mut cmd = Command::new(unit.artifact());
    cmd.arg("--ktest_logger=TEAMCITY").current_dir(ctx.ws.root());
    if let Some(filter) = &opts.filter {
        cmd.arg(format!("--ktest_filter=*{filter}*"));
    }
    Ok(TestRun::Reported(cmd, "native tests"))
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
    fn file_urls() {
        assert_eq!(file_url(Path::new("/tmp/x.mjs")), "file:///tmp/x.mjs");
        assert_eq!(file_url(Path::new("/tmp/a b#.mjs")), "file:///tmp/a%20b%23.mjs");
        if cfg!(windows) {
            assert_eq!(file_url(Path::new(r"C:\a\b.mjs")), "file:///C:/a/b.mjs");
        }
    }
}
