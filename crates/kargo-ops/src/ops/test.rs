//! `kargo test`: compiles and runs tests for each target.

use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};

use super::build::{
    BuildOptions, build_jvm, compiling, finished, hook_env, jvm_classpaths, native_libraries, native_platforms, select_targets, sources,
    wasm_libraries,
};
use super::context::BuildContext;
use crate::builtins::JUNIT_VERSION;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::testreport::{self, TestSummary};
use kargo_compiler::wasm::WasmCompilation;
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
}

/// Runs the tests; returns `true` when everything passed.
pub fn test(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.build.targets)?;
    let env = hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreTest, &env)?;

    let mut ok = true;
    for target in targets {
        let passed = match target {
            TargetKind::Jvm => test_jvm(ctx, opts)?,
            TargetKind::Wasm => test_wasm(ctx, opts)?,
            TargetKind::Native => test_native(ctx, opts)?,
            TargetKind::Android => crate::gradle::test(ctx)?,
        };
        ok &= passed;
    }
    let mut env = env;
    env.push(("KARGO_TEST_RESULT".to_owned(), if ok { "success" } else { "failure" }.to_owned()));
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostTest, &env)?;
    if opts.no_run {
        finished(ctx, start);
    }
    Ok(ok)
}

fn no_tests(ctx: &BuildContext, target: TargetKind) -> bool {
    ctx.shell().note(format!("no tests found for the {target} target (put them in tests/ or tests-{target}/)"));
    true
}

fn test_jvm(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
    let main = build_jvm(ctx)?;
    let manifest = ctx.ws.manifest();
    let test_sources = sources(ctx, TargetKind::Jvm, true, &["kt", "java"]);
    if test_sources.is_empty() {
        return Ok(no_tests(ctx, TargetKind::Jvm));
    }
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let test_cp = ctx
        .resolve("jvm/test", Platform::Jvm, &setup.requests(&[Scope::Compile, Scope::Runtime, Scope::Provided], true), &setup.boms)?
        .files();
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm);
    let test_classes = out.join("test-classes");

    let mut classpath = vec![main.classes.clone()];
    classpath.extend(test_cp.iter().cloned());
    let name = format!("{}-test", manifest.package.name);
    let unit = JvmCompilation {
        name: &name,
        sources: test_sources,
        classpath: classpath.clone(),
        friend_paths: vec![main.classes.clone()],
        output: test_classes.clone(),
        jvm_target: &jvm.jvm_target,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &setup.contributions.javac_args,
        processor_path: &[],
        resources: vec![ctx.ws.test_resources()],
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Jvm), &|| compiling(ctx, TargetKind::Jvm, ", tests"))?;
    if opts.no_run {
        return Ok(true);
    }

    let launcher = ctx.tool_jar("org.junit.platform", "junit-platform-console-standalone", JUNIT_VERSION)?;
    let mut run_cp = vec![test_classes.clone()];
    run_cp.extend(classpath);
    let mut cmd = Command::new(jdk.java());
    cmd.args(&jvm.jvm_args)
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
        .current_dir(ctx.ws.root());
    if let Some(filter) = &opts.filter {
        cmd.arg(format!("--include-classname=.*{}.*", regex_escape(filter)));
    }
    ctx.shell().status("Running", "JVM tests (JUnit Platform)");
    let status = process::status(ctx.shell(), &mut cmd, "the JUnit console launcher")?;
    Ok(status.success())
}

fn regex_escape(s: &str) -> String {
    s.chars().flat_map(|c| if "\\.+*?()|[]{}^$".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// Runs a test executable whose stdout carries TeamCity messages.
fn run_reported(ctx: &BuildContext, mut cmd: Command, label: &str) -> Result<bool> {
    ctx.shell().status("Running", label);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.stdout(Stdio::piped()).spawn().with_context(|| format!("failed to start {label}"))?;
    let stdout = child.stdout.take().context("missing test stdout")?;
    let summary = testreport::report(BufReader::new(stdout), anstream::stdout())?;
    let status = child.wait()?;
    testreport::print_summary(&summary, anstream::stdout())?;
    let crashed = !status.success() && summary.success();
    if crashed {
        ctx.shell().error(format!("{label} exited with {}", process::describe_status(status)));
    }
    Ok(summary.success() && !crashed && !(summary == TestSummary::default() && !status.success()))
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

fn test_wasm(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
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
    };
    unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Wasm), &|| compiling(ctx, TargetKind::Wasm, ", tests"))?;
    if opts.no_run {
        return Ok(true);
    }
    if opts.filter.is_some() {
        ctx.shell().warn("test filters are not supported for the wasm target; running all tests");
    }
    let mut cmd = Command::new(kargo_toolchain::node()?);
    cmd.arg("-e").arg(format!("import({:?}).then(m => m.startUnitTests())", file_url(&unit.module_path()))).current_dir(ctx.ws.root());
    run_reported(ctx, cmd, "Wasm tests (Node.js)")
}

fn test_native(ctx: &BuildContext, opts: &TestOptions) -> Result<bool> {
    let manifest = ctx.ws.manifest();
    let tests = sources(ctx, TargetKind::Native, true, &["kt"]);
    if tests.kotlin_files().is_empty() {
        return Ok(no_tests(ctx, TargetKind::Native));
    }
    let host = Host::current()?.konan_target;
    let platforms = native_platforms(ctx)?;
    if !platforms.iter().any(|p| p == host) {
        ctx.shell().warn(format!("skipping native tests: this host ({host}) is not in `platforms`"));
        return Ok(true);
    }
    let setup = ctx.setup(TargetKind::Native)?;
    let _ = native_libraries(ctx, &setup, host, false)?;
    let libraries = native_libraries(ctx, &setup, host, true)?;
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
        return Ok(true);
    }
    let mut cmd = Command::new(unit.artifact());
    cmd.arg("--ktest_logger=TEAMCITY").current_dir(ctx.ws.root());
    if let Some(filter) = &opts.filter {
        cmd.arg(format!("--ktest_filter=*{filter}*"));
    }
    run_reported(ctx, cmd, "native tests")
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
    fn file_urls() {
        assert_eq!(file_url(Path::new("/tmp/x.mjs")), "file:///tmp/x.mjs");
        assert_eq!(file_url(Path::new("/tmp/a b#.mjs")), "file:///tmp/a%20b%23.mjs");
        if cfg!(windows) {
            assert_eq!(file_url(Path::new(r"C:\a\b.mjs")), "file:///C:/a/b.mjs");
        }
    }
}
