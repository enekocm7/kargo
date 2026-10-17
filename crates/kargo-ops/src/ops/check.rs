//! `kargo check`: type-checks every enabled target without packaging or linking.
//!
//! - JVM: compiles into the same `classes` directory as `kargo build` (kotlinc has
//!   no analysis-only mode), so a later build reuses the result; no jar, no `lib/`.
//! - Wasm: produces only the klib and skips linking the `.mjs`/`.wasm` module.
//! - Native: produces a klib (`-produce library`) and skips code generation and linking.
//!
//! - Android: runs Gradle's `compile<Variant>Sources`, skipping dexing and packaging.
//!
//! Wasm and Native outputs go to `target/<profile>/check/<target>`, apart from the
//! build outputs.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};

use super::build::{
    BuildOptions, add_npm_declarations, announce, compile_jvm, finished, hook_env, native_cinterop, native_cinterop_defs, native_libraries,
    native_platforms, native_programs, select_targets, sources, warn_about_sources, wasm_libraries,
};
use super::context::BuildContext;
use super::test::compile_jvm_tests;
use crate::env::EnvOptions;
use kargo_compiler::common_kotlin_args;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::wasm::WasmCompilation;
use kargo_manifest::{Hook, NativeKind, TargetKind};

const CHECKING: &str = "Checking";

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub build: BuildOptions,
    /// Also check the test sources.
    pub tests: bool,
}

/// Output directory of `kargo check` for one target.
fn check_dir(ctx: &BuildContext, target: TargetKind) -> PathBuf {
    ctx.ws.profile_dir(ctx.profile).join("check").join(target.as_str())
}

pub fn check(ctx: &BuildContext, opts: &CheckOptions) -> Result<()> {
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.build.targets)?;
    // Pre-build hooks may generate sources, so they run before checking.
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &hook_env(ctx, &EnvOptions::default())?)?;
    warn_about_sources(ctx, &targets);

    let checked = super::parallel_targets(&targets, |target| match target {
        TargetKind::Jvm => check_jvm(ctx, opts.tests),
        TargetKind::Wasm => check_wasm(ctx, opts.tests),
        TargetKind::Native => check_native(ctx, opts.tests),
        TargetKind::Android => crate::gradle::check(ctx, opts.tests),
    });
    super::collect_targets(ctx, "check", opts.build.keep_going, targets.iter().copied().zip(checked))?;
    finished(ctx, start);
    Ok(())
}

fn check_jvm(ctx: &BuildContext, tests: bool) -> Result<()> {
    let main = compile_jvm(ctx, CHECKING)?;
    if tests {
        compile_jvm_tests(ctx, &main.classes, CHECKING)?;
    }
    Ok(())
}

fn check_wasm(ctx: &BuildContext, tests: bool) -> Result<()> {
    let manifest = ctx.ws.manifest();
    let wasm = manifest.targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let jdk = ctx.compiler_jdk()?;
    let setup = ctx.setup(TargetKind::Wasm)?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let out = check_dir(ctx, TargetKind::Wasm);
    let fp_dir = out.join(".fingerprint");

    let libraries = wasm_libraries(ctx, &setup, false)?;
    let mut main_sources = sources(ctx, TargetKind::Wasm, false, &["kt"])?;
    add_npm_declarations(ctx, &libraries, &mut main_sources, &mut kotlin_args)?;
    let main = WasmCompilation {
        name: wasm.module_name(&manifest.package),
        environment: wasm.environment,
        sources: main_sources,
        libraries,
        friends: Vec::new(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: out.clone(),
        debug: profile.debug,
        optimize: false,
        declarations: false,
        main_args: false,
    };
    main.check(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, CHECKING, TargetKind::Wasm, ""))?;

    let test_sources = sources(ctx, TargetKind::Wasm, true, &["kt"])?;
    if !tests || test_sources.kotlin_files().is_empty() {
        return Ok(());
    }
    let main_klib = main.klib_path();
    let name = format!("{}-test", main.name);
    let unit = WasmCompilation {
        name: &name,
        environment: wasm.environment,
        sources: test_sources,
        libraries: wasm_libraries(ctx, &setup, true)?,
        friends: if main_klib.is_file() { vec![main_klib] } else { Vec::new() },
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: out.join("test"),
        debug: true,
        optimize: false,
        declarations: false,
        main_args: false,
    };
    unit.check(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, CHECKING, TargetKind::Wasm, ", tests"))?;
    Ok(())
}

fn check_native(ctx: &BuildContext, tests: bool) -> Result<()> {
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let main_sources = sources(ctx, TargetKind::Native, false, &["kt"])?;
    let test_sources = sources(ctx, TargetKind::Native, true, &["kt"])?;
    if main_sources.kotlin_files().is_empty() && (!tests || test_sources.kotlin_files().is_empty()) {
        return Ok(());
    }
    // `kargo build` needs a single `main` (or a `[[bin]]` per `main`) for an executable; report a missing one here too.
    if !main_sources.kotlin_files().is_empty() {
        native_programs(ctx, native, &main_sources)?;
    }
    native_cinterop_defs(ctx)?;
    let setup = ctx.setup(TargetKind::Native)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let out = check_dir(ctx, TargetKind::Native);
    let fp_dir = out.join(".fingerprint");

    let platforms = native_platforms(ctx)?;
    let checked = super::parallel_map(&platforms, |konan_target| -> Result<()> {
        let what = format!(", {konan_target}");
        // A klib holds the analyzed IR; producing one skips code generation and linking.
        let unit = |name, sources, libraries| NativeCompilation {
            name,
            konan_target: konan_target.clone(),
            kind: NativeKind::Library,
            sources,
            libraries,
            plugins: &plugins,
            kotlin_args: &kotlin_args,
            entry_point: None,
            linker_options: &[],
            output_dir: out.join(konan_target),
            debug: false,
            optimize: false,
            test_runner: false,
        };
        // The C bindings are needed to analyze code that calls them.
        let bindings = native_cinterop(ctx, konan_target)?;
        let with_bindings = |mut libs: Vec<PathBuf>| {
            libs.extend(bindings.iter().cloned());
            libs
        };
        let main = unit(&manifest.package.name, main_sources.clone(), with_bindings(native_libraries(ctx, &setup, konan_target, false)?));
        main.compile(ctx.shell(), toolchain, &fp_dir, &|| announce(ctx, CHECKING, TargetKind::Native, &what))?;

        if tests && !test_sources.kotlin_files().is_empty() {
            // Like `kargo test`, tests are compiled together with the main sources.
            let mut all = main_sources.clone();
            all.extend(&test_sources);
            let name = format!("{}-test", manifest.package.name);
            let test = unit(&name, all, with_bindings(native_libraries(ctx, &setup, konan_target, true)?));
            let what = format!("{what}, tests");
            test.compile(ctx.shell(), toolchain, &fp_dir, &|| announce(ctx, CHECKING, TargetKind::Native, &what))?;
        }
        Ok(())
    });
    checked.into_iter().collect()
}
