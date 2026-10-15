//! `kargo build`: compiles every enabled target.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::{BuildContext, TargetSetup};
use super::local::{self, LocalTarget};
use crate::warnings;
use crate::workspace::ProfileKind;
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::ksp::KspRun;
use kargo_compiler::mainclass::{find_entry_point_in, find_jvm_main_class, find_single_entry_point};
use kargo_compiler::native::{CinteropUnit, NativeCompilation};
use kargo_compiler::wasm::WasmCompilation;
use kargo_compiler::{Outcome, SourceSet, common_kotlin_args};
use kargo_manifest::{Bin, Cinterop, Hook, Manifest, NativeKind, Scope, TargetKind, WasmEnvironment, WasmKind};
use kargo_maven::{Platform, Resolution};
use kargo_toolchain::native::resolve_platform;
use kargo_util::fs::collect_files;

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Targets to build; empty means every enabled target.
    pub targets: Vec<kargo_manifest::TargetKind>,
    pub release: bool,
    /// Build every target even when one fails, then report all failures (`--keep-going`).
    pub keep_going: bool,
}

impl BuildOptions {
    pub fn profile(&self) -> ProfileKind {
        if self.release { ProfileKind::Release } else { ProfileKind::Dev }
    }
}

/// Outputs of the JVM target.
#[derive(Debug, Clone)]
pub struct JvmBuild {
    pub classes: PathBuf,
    pub jar: PathBuf,
    pub lib_dir: PathBuf,
    /// Runtime classpath of dependencies (not including `classes`).
    pub runtime_classpath: Vec<PathBuf>,
    pub main_class: Option<String>,
    /// Why no main class was detected, when `main_class` is `None`.
    pub main_class_error: Option<String>,
    pub jvm_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WasmBuild {
    pub module: PathBuf,
    pub klib: PathBuf,
    pub environment: WasmEnvironment,
    pub kind: WasmKind,
    /// The module's name: `module-name`, else the package name. It names the files in `dir`.
    pub name: String,
    /// The output directory (`target/<profile>/wasm`).
    pub dir: PathBuf,
    /// The klibs the module was compiled against.
    pub libraries: Vec<PathBuf>,
    /// The `node_modules` holding the npm packages the module imports, once installed.
    pub node_modules: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct NativeBuild {
    pub konan_target: String,
    pub artifact: PathBuf,
    pub kind: NativeKind,
    /// The `[[bin]]` this executable is, or `None` for the project's own artifact.
    pub bin: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BuildResults {
    pub jvm: Option<JvmBuild>,
    pub wasm: Option<WasmBuild>,
    pub native: Vec<NativeBuild>,
    pub android: Option<PathBuf>,
}

/// Resolves which targets an operation applies to.
pub fn select_targets(ctx: &BuildContext, requested: &[TargetKind]) -> Result<Vec<TargetKind>> {
    let enabled = ctx.ws.manifest().targets.enabled();
    if requested.is_empty() {
        return Ok(enabled);
    }
    for t in requested {
        if !enabled.contains(t) {
            bail!("target `{t}` is not enabled in kargo.toml; add a [target.{t}] section");
        }
    }
    Ok(requested.to_vec())
}

pub fn sources(ctx: &BuildContext, target: TargetKind, tests: bool, extensions: &[&str]) -> SourceSet {
    let ws = ctx.ws;
    let (common, platform) =
        if tests { (ws.common_tests(), ws.target_tests(target)) } else { (ws.common_sources(), ws.target_sources(target)) };
    SourceSet::new(collect_files(&common, extensions), collect_files(&platform, extensions), target.as_str())
}

/// Warns about sources that are missing for `targets` or that Kargo ignores. Call it after the
/// pre-build hooks, which may generate sources.
pub fn warn_about_sources(ctx: &BuildContext, targets: &[TargetKind]) {
    let mut messages = warnings::missing_sources(ctx.ws, targets);
    messages.extend(warnings::ignored_files(ctx.ws));
    warnings::emit(ctx.shell(), ctx.project, ctx.ws, messages);
}

pub fn build(ctx: &BuildContext, opts: &BuildOptions) -> Result<BuildResults> {
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.targets)?;
    let env = hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    warn_about_sources(ctx, &targets);

    let mut results = BuildResults::default();
    // Targets don't depend on each other, so they build at the same time.
    let built = super::parallel_targets(&targets, |target| -> Result<BuildResults> {
        let mut r = BuildResults::default();
        match target {
            TargetKind::Jvm => r.jvm = Some(build_jvm(ctx)?),
            TargetKind::Wasm => r.wasm = build_wasm(ctx)?,
            TargetKind::Native => r.native = build_native(ctx)?,
            TargetKind::Android => r.android = Some(crate::gradle::build(ctx)?),
        }
        Ok(r)
    });
    for r in super::collect_targets(ctx, "build", opts.keep_going, targets.iter().copied().zip(built))? {
        results.jvm = results.jvm.or(r.jvm);
        results.wasm = results.wasm.or(r.wasm);
        results.native.extend(r.native);
        results.android = results.android.or(r.android);
    }

    let mut env = env;
    if let Some(jvm) = &results.jvm {
        env.push(("KARGO_JVM_JAR".to_owned(), jvm.jar.to_string_lossy().into_owned()));
        let mut cp = vec![jvm.classes.clone()];
        cp.extend(jvm.runtime_classpath.iter().cloned());
        env.push(("KARGO_CLASSPATH".to_owned(), kargo_util::fs::join_paths(&cp)));
    }
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostBuild, &env)?;
    finished(ctx, start);
    Ok(results)
}

pub fn hook_env(ctx: &BuildContext) -> Vec<(String, String)> {
    let profile = match ctx.profile {
        ProfileKind::Dev => "dev",
        ProfileKind::Release => "release",
    };
    let mut env = crate::plugins::base_env(ctx.ws, profile);
    // Hooks run while this process holds the build lock; a `kargo` they call must not wait on it.
    if let Some(lock) = ctx.build_lock_path() {
        env.push((kargo_util::flock::HELD_LOCK_ENV.to_owned(), lock.to_string_lossy().into_owned()));
    }
    env
}

pub fn finished(ctx: &BuildContext, start: Instant) {
    let (name, detail) = match ctx.profile {
        ProfileKind::Dev => ("dev", "unoptimized + debuginfo"),
        ProfileKind::Release => ("release", "optimized"),
    };
    ctx.shell().status("Finished", format!("`{name}` profile [{detail}] target(s) in {:.2}s", start.elapsed().as_secs_f64()));
}

pub fn compiling(ctx: &BuildContext, target: TargetKind, what: &str) {
    announce(ctx, "Compiling", target, what);
}

/// Prints the status line shown right before a compiler runs, e.g. `Compiling app v0.1.0 (jvm)`.
pub fn announce(ctx: &BuildContext, verb: &str, target: TargetKind, what: &str) {
    let pkg = &ctx.ws.manifest().package;
    ctx.shell().status(verb, format!("{} v{} ({target}{what})", pkg.name, pkg.version));
}

/// JVM classpaths for one setup.
pub struct JvmClasspaths {
    pub compile: Resolution,
    pub runtime: Resolution,
}

/// The JVM compile classpath: path dependency jars, then the resolved Maven artifacts.
pub fn jvm_compile_classpath(ctx: &BuildContext, classpaths: &JvmClasspaths) -> Result<Vec<PathBuf>> {
    let mut files = local::artifacts(ctx, LocalTarget::Jvm, &[Scope::Compile, Scope::Provided], false)?;
    files.extend(classpaths.compile.files());
    Ok(files)
}

/// The JVM runtime classpath of dependencies: path dependency jars, then the resolved Maven artifacts.
pub fn jvm_runtime_classpath(ctx: &BuildContext, classpaths: &JvmClasspaths) -> Result<Vec<PathBuf>> {
    let mut files = local::artifacts(ctx, LocalTarget::Jvm, &[Scope::Compile, Scope::Runtime], false)?;
    files.extend(classpaths.runtime.files());
    Ok(files)
}

pub fn jvm_classpaths(ctx: &BuildContext, setup: &TargetSetup) -> Result<JvmClasspaths> {
    let (compile, runtime) = super::join(
        || ctx.resolve("jvm/compile", Platform::Jvm, &setup.requests(&[Scope::Compile, Scope::Provided], false), &setup.boms),
        || ctx.resolve("jvm/runtime", Platform::Jvm, &setup.requests(&[Scope::Compile, Scope::Runtime], false), &setup.boms),
    );
    Ok(JvmClasspaths { compile: compile?, runtime: runtime? })
}

/// The javac `-processorpath`: framework processors (e.g. Lombok) followed by
/// `scope = "processor"` dependencies; `test` adds processor dev-dependencies.
pub fn jvm_processor_path(ctx: &BuildContext, setup: &TargetSetup, test: bool) -> Result<Vec<PathBuf>> {
    let mut requests: Vec<_> = setup.contributions.annotation_processors.iter().map(super::context::to_request).collect();
    let declared = setup.requests(&[Scope::Processor], test);
    // BOMs only apply to declared processors, so framework-only setups keep their lockfile entry.
    let boms: &[_] = if declared.is_empty() { &[] } else { &setup.boms };
    for r in declared {
        if !requests.iter().any(|q| q.same_artifact(&r)) {
            requests.push(r);
        }
    }
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let dev_processors = test && setup.dev.iter().any(|d| d.scope == Scope::Processor);
    let key = if dev_processors { "jvm/test-annotation-processors" } else { "jvm/annotation-processors" };
    Ok(ctx.resolve(key, Platform::Jvm, &requests, boms)?.files())
}

/// The KSP processor path: `scope = "ksp"` dependencies; `test` adds KSP dev-dependencies.
pub fn jvm_ksp_path(ctx: &BuildContext, setup: &TargetSetup, test: bool) -> Result<Vec<PathBuf>> {
    let requests = setup.requests(&[Scope::Ksp], test);
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let dev_processors = test && setup.dev.iter().any(|d| d.scope == Scope::Ksp);
    let key = if dev_processors { "jvm/test-ksp" } else { "jvm/ksp" };
    Ok(ctx.resolve(key, Platform::Jvm, &requests, &setup.boms)?.files())
}

/// The compilation a KSP pass precedes.
pub(crate) struct JvmKspUnit<'a> {
    pub name: &'a str,
    pub test: bool,
    pub libraries: Vec<PathBuf>,
    pub friends: Vec<PathBuf>,
    pub jdk: &'a kargo_toolchain::Jdk,
    pub jvm_target: &'a str,
}

/// Runs the KSP processors over the main (or test) sources and adds what they generate to
/// `sources` and `resources`. Does nothing when no `scope = "ksp"` dependency applies.
pub(crate) fn run_jvm_ksp(
    ctx: &BuildContext,
    setup: &TargetSetup,
    unit: JvmKspUnit,
    sources: &mut SourceSet,
    resources: &mut Vec<PathBuf>,
) -> Result<()> {
    let processor_path = jvm_ksp_path(ctx, setup, unit.test)?;
    if processor_path.is_empty() {
        return Ok(());
    }
    let ws = ctx.ws;
    let kotlin = &ws.manifest().kotlin;
    let language_version = kotlin.language_version.clone().unwrap_or_else(|| kargo_compiler::ksp::language_version(&kotlin.version));
    let api_version = kotlin.api_version.clone().unwrap_or_else(|| language_version.clone());
    let (source_roots, dir, what) = if unit.test {
        (vec![ws.common_tests(), ws.target_tests(TargetKind::Jvm)], "test", ", tests, ksp")
    } else {
        (vec![ws.common_sources(), ws.target_sources(TargetKind::Jvm)], "main", ", ksp")
    };
    let tool_classpath = ctx.ksp_tool()?;
    let run = KspRun {
        name: unit.name,
        source_roots,
        libraries: unit.libraries,
        friends: unit.friends,
        processor_path: &processor_path,
        tool_classpath: &tool_classpath,
        jvm_target: unit.jvm_target,
        language_version: &language_version,
        api_version: &api_version,
        output: ws.output_dir(ctx.profile, TargetKind::Jvm).join("generated").join("ksp").join(dir),
        project_dir: ws.root(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let generated = run.run(ctx.shell(), unit.jdk, &fp_dir, &|| announce(ctx, "Processing", TargetKind::Jvm, what))?;
    // Generated files join the platform fragment, unless that would turn a single-fragment
    // compilation (only `src/` has Kotlin) into a multiplatform one.
    if sources.platform.iter().any(|p| p.extension().is_some_and(|e| e == "kt")) {
        sources.platform.extend(generated.sources);
    } else {
        sources.common.extend(generated.sources);
    }
    resources.extend(generated.resources);
    Ok(())
}

/// `javac` arguments: framework-provided ones, then `[target.jvm] javac-args`.
pub fn jvm_javac_args(setup: &TargetSetup, jvm: &kargo_manifest::JvmTarget) -> Vec<String> {
    let mut args = setup.contributions.javac_args.clone();
    args.extend(jvm.javac_args.iter().cloned());
    args
}

fn jvm_settings<'a>(ctx: &BuildContext<'a>) -> Result<&'a kargo_manifest::JvmTarget> {
    ctx.ws.manifest().targets.jvm.as_ref().context("the JVM target is not enabled")
}

/// Compiled JVM main classes, before packaging.
pub struct JvmClasses {
    pub classes: PathBuf,
    pub sources: SourceSet,
    pub classpaths: JvmClasspaths,
    pub outcome: Outcome,
}

/// Compiles the JVM main sources into `target/<profile>/jvm/classes`; `verb` is
/// the status shown when the compiler runs (`Compiling` or `Checking`).
pub fn compile_jvm(ctx: &BuildContext, verb: &str) -> Result<JvmClasses> {
    let manifest = ctx.ws.manifest();
    let jvm = jvm_settings(ctx)?;
    let setup = ctx.setup(TargetKind::Jvm)?;
    // On a cold cache the JDK, the compiler and the dependencies all download at once.
    let (toolchain, classpaths) = super::join(
        || -> Result<_> { Ok((ctx.jvm_jdk()?, ctx.kotlin_compiler()?, ctx.plugin_jars(&setup.contributions.plugins)?)) },
        || jvm_classpaths(ctx, &setup),
    );
    let (jdk, compiler, plugins) = toolchain?;
    let classpaths = classpaths?;
    let classpath = jvm_compile_classpath(ctx, &classpaths)?;
    let processor_path = jvm_processor_path(ctx, &setup, false)?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);

    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm);
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let classes = out.join("classes");
    let jvm_target = jvm.target_for(jdk.version)?;
    let mut srcs = sources(ctx, TargetKind::Jvm, false, &["kt", "java"]);
    let mut resources = vec![ctx.ws.resources()];
    let ksp = JvmKspUnit {
        name: &manifest.package.name,
        test: false,
        libraries: classpaths.compile.files(),
        friends: Vec::new(),
        jdk,
        jvm_target: &jvm_target,
    };
    run_jvm_ksp(ctx, &setup, ksp, &mut srcs, &mut resources)?;

    let unit = JvmCompilation {
        name: &manifest.package.name,
        sources: srcs.clone(),
        classpath,
        friend_paths: Vec::new(),
        output: classes.clone(),
        jvm_target: &jvm_target,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &javac_args,
        processor_path: &processor_path,
        resources,
    };
    let outcome = unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, verb, TargetKind::Jvm, ""))?;
    Ok(JvmClasses { classes, sources: srcs, classpaths, outcome })
}

pub fn build_jvm(ctx: &BuildContext) -> Result<JvmBuild> {
    let manifest = ctx.ws.manifest();
    let jvm = jvm_settings(ctx)?;
    let JvmClasses { classes, sources: srcs, classpaths, outcome } = compile_jvm(ctx, "Compiling")?;
    let changed = outcome == Outcome::Compiled;
    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm);

    // A library has no main class, so a failed detection only matters to `kargo run`.
    let (main_class, main_class_error) = match &jvm.main_class {
        Some(m) => (Some(m.clone()), None),
        None => match find_jvm_main_class(&srcs.kotlin_files(), "[target.jvm] main-class") {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(e.to_string())),
        },
    };

    // Thin jar plus dependencies in lib/.
    let runtime_classpath = jvm_runtime_classpath(ctx, &classpaths)?;
    let lib_dir = out.join("lib");
    let lib_names = sync_lib_dir(&lib_dir, &runtime_classpath)?;
    let jar = out.join(format!("{}-{}.jar", manifest.package.name, manifest.package.version));
    if changed || !jar.is_file() {
        let jar_manifest = JarManifest {
            main_class: main_class.clone(),
            class_path: lib_names.iter().map(|n| format!("lib/{n}")).collect(),
            implementation_title: Some(manifest.package.name.clone()),
            implementation_version: Some(manifest.package.version.clone()),
        };
        write_jar(&jar, &classes, &jar_manifest, &[])?;
        kargo_compiler::abi::update_stamp(&jar)?;
    } else if !kargo_compiler::abi::stamp_path(&jar).is_file() {
        kargo_compiler::abi::update_stamp(&jar)?;
    }

    Ok(JvmBuild { classes, jar, lib_dir, runtime_classpath, main_class, main_class_error, jvm_args: jvm.jvm_args.clone() })
}

/// Copies runtime dependencies into `lib/`, returning their file names in classpath order.
fn sync_lib_dir(lib_dir: &Path, jars: &[PathBuf]) -> Result<Vec<String>> {
    std::fs::create_dir_all(lib_dir)?;
    let mut names: Vec<String> = Vec::new();
    for jar in jars.iter().filter(|j| j.extension().is_some_and(|e| e == "jar")) {
        let mut name = jar.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if names.contains(&name) {
            // Same file name from different groups: disambiguate with the parent dirs.
            let group = jar.parent().and_then(|v| v.parent()).and_then(|a| a.parent()).and_then(|g| g.file_name());
            name = format!("{}-{name}", group.map(|g| g.to_string_lossy().into_owned()).unwrap_or_default());
        }
        let dest = lib_dir.join(&name);
        // A path dependency's jar can be rewritten with the same size, so its time counts too.
        let up_to_date = std::fs::metadata(&dest)
            .ok()
            .zip(std::fs::metadata(jar).ok())
            .is_some_and(|(d, s)| d.len() == s.len() && d.modified().ok().zip(s.modified().ok()).is_some_and(|(d, s)| d >= s));
        if !up_to_date {
            std::fs::copy(jar, &dest).with_context(|| format!("failed to copy {} to {}", jar.display(), dest.display()))?;
        }
        names.push(name);
    }
    // Remove stale jars from previous builds.
    for entry in std::fs::read_dir(lib_dir)?.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        if !names.contains(&fname) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(names)
}

/// The resolved Maven klibs of the Wasm target.
pub fn wasm_resolution(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Resolution> {
    let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
    let key = if include_dev { "wasm/test" } else { "wasm/main" };
    let requests = setup.requests(&ALL_SCOPES, include_dev);
    ctx.resolve(key, Platform::Wasm(env.platform().to_owned()), &requests, &setup.boms)
}

/// Every klib the Wasm sources compile against: path dependencies first, then Maven libraries.
pub fn wasm_libraries(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Vec<PathBuf>> {
    let mut libraries = local::artifacts(ctx, LocalTarget::Wasm, &ALL_SCOPES, include_dev)?;
    libraries.extend(wasm_resolution(ctx, setup, include_dev)?.files());
    Ok(libraries)
}

pub(crate) const ALL_SCOPES: [Scope; 3] = [Scope::Compile, Scope::Runtime, Scope::Provided];

/// Compiles the Wasm target and adds what surrounds the module: the page of a browser app, the
/// `package.json` of a library, and in a release build `wasm-opt` and the size report.
pub fn build_wasm(ctx: &BuildContext) -> Result<Option<WasmBuild>> {
    let Some(build) = prepare_wasm(ctx)? else { return Ok(None) };
    crate::wasm_output::finish(ctx, &build)?;
    Ok(Some(build))
}

/// Compiles the Wasm target and installs the npm packages it imports (enough to run its tests).
pub fn prepare_wasm(ctx: &BuildContext) -> Result<Option<WasmBuild>> {
    let Some(mut build) = compile_wasm(ctx, true)? else { return Ok(None) };
    // Outside the Wasm output directory, which is wiped on every recompile;
    // Node.js still finds `target/<profile>/node_modules` from the `.mjs`.
    build.node_modules = crate::npm::install(ctx, &build.libraries, &ctx.ws.profile_dir(ctx.profile))?;
    Ok(Some(build))
}

/// Compiles and links the Wasm target without installing npm packages (enough for a path dependency).
/// `main_args` passes the program arguments to `main(args)`; off for a path dependency.
pub fn compile_wasm(ctx: &BuildContext, main_args: bool) -> Result<Option<WasmBuild>> {
    let manifest = ctx.ws.manifest();
    let wasm = manifest.targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let setup = ctx.setup(TargetKind::Wasm)?;
    let (toolchain, libraries) = super::join(
        || -> Result<_> { Ok((ctx.compiler_jdk()?, ctx.kotlin_compiler()?, ctx.plugin_jars(&setup.contributions.plugins)?)) },
        || wasm_libraries(ctx, &setup, false),
    );
    let (jdk, compiler, plugins) = toolchain?;
    let libraries = libraries?;
    let profile = ctx.ws.profile(ctx.profile);
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut srcs = sources(ctx, TargetKind::Wasm, false, &["kt"]);
    add_npm_declarations(ctx, &libraries, &mut srcs, &mut kotlin_args)?;
    let unit = WasmCompilation {
        name: wasm.module_name(&manifest.package),
        environment: wasm.environment,
        sources: srcs,
        libraries,
        friends: Vec::new(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: ctx.ws.output_dir(ctx.profile, TargetKind::Wasm),
        debug: profile.debug,
        optimize: profile.optimize,
        declarations: wasm.kind == WasmKind::Library,
        main_args,
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Wasm);
    if unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| compiling(ctx, TargetKind::Wasm, ""))? == Outcome::Empty {
        return Ok(None);
    }
    Ok(Some(WasmBuild {
        module: unit.module_path(),
        klib: unit.klib_path(),
        environment: wasm.environment,
        kind: wasm.kind,
        name: unit.name.to_owned(),
        dir: unit.output.clone(),
        libraries: unit.libraries,
        node_modules: None,
    }))
}

/// Adds the Kotlin declarations generated for the project's npm packages (see
/// [`crate::npm::declarations`]) to the Wasm main `sources`, and the opt-in they need to `kotlin_args`.
pub(crate) fn add_npm_declarations(
    ctx: &BuildContext,
    libraries: &[PathBuf],
    sources: &mut SourceSet,
    kotlin_args: &mut Vec<String>,
) -> Result<()> {
    let generated = crate::npm::declarations(ctx, libraries)?;
    if generated.is_empty() {
        return Ok(());
    }
    // As with KSP: the platform fragment, unless only `src/` has Kotlin, which then stays one fragment.
    if sources.platform.iter().any(|p| p.extension().is_some_and(|e| e == "kt")) {
        sources.platform.extend(generated);
    } else {
        sources.common.extend(generated);
    }
    npm_opt_in(ctx, kotlin_args);
    Ok(())
}

/// Adds the opt-in that calls into the npm declarations need, when the project has npm dependencies.
pub(crate) fn npm_opt_in(ctx: &BuildContext, kotlin_args: &mut Vec<String>) {
    let manifest = ctx.ws.manifest();
    if manifest.targets.wasm.as_ref().is_some_and(|w| !w.npm_dependencies.is_empty()) {
        kotlin_args.extend(crate::npm::interop_opt_in(manifest));
    }
}

pub fn native_platforms(ctx: &BuildContext) -> Result<Vec<String>> {
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    let mut out = Vec::new();
    for p in &native.platforms {
        let resolved = resolve_platform(p)?;
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// The resolved Maven klibs of one Kotlin/Native platform.
pub fn native_resolution(ctx: &BuildContext, setup: &TargetSetup, konan_target: &str, include_dev: bool) -> Result<Resolution> {
    let key = format!("native-{konan_target}/{}", if include_dev { "test" } else { "main" });
    let requests = setup.requests(&ALL_SCOPES, include_dev);
    ctx.resolve(&key, Platform::Native(konan_target.to_owned()), &requests, &setup.boms)
}

/// Every klib the Native sources compile against: path dependencies first, then Maven libraries.
pub fn native_libraries(ctx: &BuildContext, setup: &TargetSetup, konan_target: &str, include_dev: bool) -> Result<Vec<PathBuf>> {
    let mut libraries = local::artifacts(ctx, LocalTarget::Native(konan_target), &ALL_SCOPES, include_dev)?;
    libraries.extend(native_resolution(ctx, setup, konan_target, include_dev)?.files());
    Ok(libraries)
}

/// Compiles the Native main sources into a klib for `konan_target`, which is what a consumer
/// of this package (as a path dependency) links. Returns `None` without Native sources.
pub fn native_klib(ctx: &BuildContext, konan_target: &str) -> Result<Option<PathBuf>> {
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let srcs = sources(ctx, TargetKind::Native, false, &["kt"]);
    if srcs.kotlin_files().is_empty() {
        return Ok(None);
    }
    let setup = ctx.setup(TargetKind::Native)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(konan_target);
    // A library's own build already produces the klib; anything else gets a separate one.
    let (output_dir, fp_dir) = match native.kind {
        NativeKind::Library => (out, ctx.fingerprint_dir(TargetKind::Native)),
        _ => (out.join("klib"), ctx.fingerprint_dir(TargetKind::Native).join("klib")),
    };
    let unit = NativeCompilation {
        name: &manifest.package.name,
        konan_target: konan_target.to_owned(),
        kind: NativeKind::Library,
        sources: srcs,
        libraries: native_libraries(ctx, &setup, konan_target, false)?,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point: None,
        linker_options: &native.linker_options,
        output_dir,
        debug: profile.debug,
        optimize: profile.optimize,
        test_runner: false,
    };
    let what = format!(", {konan_target}");
    unit.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
    Ok(Some(unit.artifact()))
}

/// The `.def` file of every `[target.native.cinterop]` entry (`None` for a headers-only one),
/// failing on a missing file before any toolchain is downloaded.
pub fn native_cinterop_defs<'a>(ctx: &BuildContext<'a>) -> Result<Vec<(&'a Cinterop, Option<PathBuf>)>> {
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    native
        .cinterop
        .iter()
        .map(|interop| {
            let def = ctx.ws.root().join(interop.def_path());
            if def.is_file() {
                Ok((interop, Some(def)))
            } else if interop.def.is_none() && !interop.headers.is_empty() {
                Ok((interop, None))
            } else {
                bail!(
                    "the definition file {} of [target.native.cinterop.{}] does not exist; create it, set `def`, or list `headers`",
                    interop.def_path().display(),
                    interop.name
                )
            }
        })
        .collect()
}

/// Runs `cinterop` for every `[target.native.cinterop]` entry and returns the klibs with the bindings.
pub fn native_cinterop(ctx: &BuildContext, konan_target: &str) -> Result<Vec<PathBuf>> {
    let defs = native_cinterop_defs(ctx)?;
    if defs.is_empty() {
        return Ok(Vec::new());
    }
    let toolchain = ctx.native_toolchain()?;
    let output_dir = ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(konan_target).join("cinterop");
    let fp_dir = ctx.fingerprint_dir(TargetKind::Native);
    defs.into_iter()
        .map(|(interop, def)| {
            let unit =
                CinteropUnit { interop, konan_target: konan_target.to_owned(), root: ctx.ws.root(), def, output_dir: output_dir.clone() };
            unit.generate(ctx.shell(), toolchain, &fp_dir, &|| {
                ctx.shell().status("Generating", format!("{} C bindings ({konan_target})", interop.name))
            })?;
            Ok(unit.artifact())
        })
        .collect()
}

/// The `-entry` of a native executable: `entry-point` from kargo.toml, or the single `main` in the sources.
pub fn native_entry_point(native: &kargo_manifest::NativeTarget, srcs: &SourceSet) -> Result<Option<String>> {
    Ok(match (&native.entry_point, native.kind) {
        (Some(e), _) => Some(e.clone()),
        (None, NativeKind::Executable) => {
            Some(find_single_entry_point(&srcs.kotlin_files(), "[target.native] entry-point")?.native_entry())
        }
        (None, _) => None,
    })
}

/// The `[[bin]]` named `name`, or an error listing the declared ones.
pub fn find_bin<'a>(manifest: &'a Manifest, name: &str) -> Result<&'a Bin> {
    if let Some(bin) = manifest.bin(name) {
        return Ok(bin);
    }
    if manifest.bins.is_empty() {
        bail!("no bin named `{name}`: kargo.toml declares no [[bin]] programs");
    }
    let names: Vec<&str> = manifest.bins.iter().map(|b| b.name.as_str()).collect();
    bail!("no bin named `{name}` in kargo.toml; available bins: {}", names.join(", "))
}

/// The JVM main class of a `[[bin]]`: its `main`, or the class of the `main` in its `path`.
pub fn bin_main_class(root: &Path, bin: &Bin) -> Result<String> {
    match (&bin.main, &bin.path) {
        (Some(main), _) => Ok(main.clone()),
        (None, Some(path)) => {
            Ok(find_entry_point_in(&root.join(path), false).with_context(|| format!("[[bin]] `{}`", bin.name))?.jvm_class)
        }
        (None, None) => bail!("[[bin]] `{}` has no JVM main class; set `main` or `path` in kargo.toml", bin.name),
    }
}

/// The Kotlin/Native entry function of a `[[bin]]`: its `entry-point`, or the top-level
/// `main` in its `path`; `None` for a JVM-only bin.
pub fn bin_entry_point(root: &Path, bin: &Bin) -> Result<Option<String>> {
    match (&bin.entry_point, &bin.path) {
        (Some(entry), _) => Ok(Some(entry.clone())),
        (None, Some(path)) => {
            Ok(Some(find_entry_point_in(&root.join(path), true).with_context(|| format!("[[bin]] `{}`", bin.name))?.native_entry()))
        }
        (None, None) => Ok(None),
    }
}

/// One Kotlin/Native artifact a build links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProgram {
    /// The `[[bin]]` it is, or `None` for the project's own artifact (named after the package).
    pub bin: Option<String>,
    pub entry_point: Option<String>,
}

/// What a native build links: the project's own artifact and, for an executable, one per
/// `[[bin]]` with a native entry point. When bins exist and the sources hold several `main`
/// functions without an `entry-point`, the project's own executable is left out: each bin picks one.
pub fn native_programs(ctx: &BuildContext, native: &kargo_manifest::NativeTarget, srcs: &SourceSet) -> Result<Vec<NativeProgram>> {
    if native.kind != NativeKind::Executable {
        return Ok(vec![NativeProgram { bin: None, entry_point: native_entry_point(native, srcs)? }]);
    }
    let mut bins = Vec::new();
    for bin in &ctx.ws.manifest().bins {
        if let Some(entry) = bin_entry_point(ctx.ws.root(), bin)? {
            bins.push(NativeProgram { bin: Some(bin.name.clone()), entry_point: Some(entry) });
        }
    }
    let mut programs = Vec::new();
    match native_entry_point(native, srcs) {
        Ok(entry_point) => programs.push(NativeProgram { bin: None, entry_point }),
        Err(e) if bins.is_empty() => return Err(e),
        Err(_) => {}
    }
    programs.extend(bins);
    Ok(programs)
}

pub fn build_native(ctx: &BuildContext) -> Result<Vec<NativeBuild>> {
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let setup = ctx.setup(TargetKind::Native)?;
    let srcs = sources(ctx, TargetKind::Native, false, &["kt"]);
    if srcs.kotlin_files().is_empty() {
        return Ok(Vec::new());
    }
    let programs = native_programs(ctx, native, &srcs)?;
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);

    let platforms = native_platforms(ctx)?;
    // Each platform is a separate Kotlin/Native compiler process.
    let built = super::parallel_map(&platforms, |konan_target| -> Result<Vec<NativeBuild>> {
        let mut libraries = native_libraries(ctx, &setup, konan_target, false)?;
        libraries.extend(native_cinterop(ctx, konan_target)?);
        let out = ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(konan_target);
        let mut builds = Vec::new();
        for program in &programs {
            // Bins link into `bin/` with their own fingerprints, so one named like the package does not clash.
            let (name, output_dir, fp_dir, what) = match &program.bin {
                None => (manifest.package.name.as_str(), out.clone(), ctx.fingerprint_dir(TargetKind::Native), format!(", {konan_target}")),
                Some(bin) => {
                    let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join("bin");
                    std::fs::create_dir_all(&fp_dir)?;
                    (bin.as_str(), out.join("bin"), fp_dir, format!(", {konan_target}, bin {bin}"))
                }
            };
            let unit = NativeCompilation {
                name,
                konan_target: konan_target.clone(),
                kind: native.kind,
                sources: srcs.clone(),
                libraries: libraries.clone(),
                plugins: &plugins,
                kotlin_args: &kotlin_args,
                entry_point: program.entry_point.clone(),
                linker_options: &native.linker_options,
                output_dir,
                debug: profile.debug,
                optimize: profile.optimize,
                test_runner: false,
            };
            unit.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            builds.push(NativeBuild {
                konan_target: konan_target.clone(),
                artifact: unit.artifact(),
                kind: native.kind,
                bin: program.bin.clone(),
            });
        }
        Ok(builds)
    });
    let mut builds = Vec::new();
    for b in built {
        builds.extend(b?);
    }
    Ok(builds)
}
