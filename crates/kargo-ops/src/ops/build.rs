//! `kargo build`: compiles every enabled target.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::{BuildContext, TargetSetup};
use crate::workspace::ProfileKind;
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::mainclass::{find_jvm_main_class, find_single_entry_point};
use kargo_compiler::native::{CinteropUnit, NativeCompilation};
use kargo_compiler::wasm::WasmCompilation;
use kargo_compiler::{Outcome, SourceSet, common_kotlin_args};
use kargo_manifest::{Cinterop, Hook, NativeKind, Scope, TargetKind, WasmEnvironment};
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
}

#[derive(Debug, Clone)]
pub struct NativeBuild {
    pub konan_target: String,
    pub artifact: PathBuf,
    pub kind: NativeKind,
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

pub fn build(ctx: &BuildContext, opts: &BuildOptions) -> Result<BuildResults> {
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.targets)?;
    let env = hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;

    let mut results = BuildResults::default();
    // Targets don't depend on each other, so they build at the same time.
    let built = super::parallel_map(&targets, |&target| -> Result<BuildResults> {
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
    crate::plugins::base_env(ctx.ws, profile)
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

pub fn jvm_classpaths(ctx: &BuildContext, setup: &TargetSetup) -> Result<JvmClasspaths> {
    let (compile, runtime) = super::join(
        || ctx.resolve("jvm/compile", Platform::Jvm, &setup.requests(&[Scope::Compile, Scope::Provided], false), &setup.boms),
        || ctx.resolve("jvm/runtime", Platform::Jvm, &setup.requests(&[Scope::Compile, Scope::Runtime], false), &setup.boms),
    );
    Ok(JvmClasspaths { compile: compile?, runtime: runtime? })
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
        || -> Result<_> { Ok((ctx.compiler_jdk()?, ctx.kotlin_compiler()?, ctx.plugin_jars(&setup.contributions.plugins)?)) },
        || jvm_classpaths(ctx, &setup),
    );
    let (jdk, compiler, plugins) = toolchain?;
    let classpaths = classpaths?;
    let processor_path: Vec<PathBuf> = if setup.contributions.annotation_processors.is_empty() {
        Vec::new()
    } else {
        let requests: Vec<_> = setup.contributions.annotation_processors.iter().map(super::context::to_request).collect();
        ctx.resolve("jvm/annotation-processors", Platform::Jvm, &requests, &[])?.files()
    };
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);

    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm);
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let classes = out.join("classes");
    let srcs = sources(ctx, TargetKind::Jvm, false, &["kt", "java"]);

    let unit = JvmCompilation {
        name: &manifest.package.name,
        sources: srcs.clone(),
        classpath: classpaths.compile.files(),
        friend_paths: Vec::new(),
        output: classes.clone(),
        jvm_target: &jvm.target_for(jdk.version),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &setup.contributions.javac_args,
        processor_path: &processor_path,
        resources: vec![ctx.ws.resources()],
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
    let runtime_classpath = classpaths.runtime.files();
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
        let up_to_date = std::fs::metadata(&dest).ok().zip(std::fs::metadata(jar).ok()).is_some_and(|(d, s)| d.len() == s.len());
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

pub fn wasm_libraries(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Vec<PathBuf>> {
    let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
    let key = if include_dev { "wasm/test" } else { "wasm/main" };
    let requests = setup.requests(&[Scope::Compile, Scope::Runtime, Scope::Provided], include_dev);
    Ok(ctx.resolve(key, Platform::Wasm(env.as_str().to_owned()), &requests, &setup.boms)?.files())
}

pub fn build_wasm(ctx: &BuildContext) -> Result<Option<WasmBuild>> {
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
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let unit = WasmCompilation {
        name: &manifest.package.name,
        environment: wasm.environment,
        sources: sources(ctx, TargetKind::Wasm, false, &["kt"]),
        libraries,
        friends: Vec::new(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: ctx.ws.output_dir(ctx.profile, TargetKind::Wasm),
        debug: profile.debug,
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Wasm);
    if unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| compiling(ctx, TargetKind::Wasm, ""))? == Outcome::Empty {
        return Ok(None);
    }
    // Outside the Wasm output directory, which is wiped on every recompile;
    // Node.js still finds `target/<profile>/node_modules` from the `.mjs`.
    crate::npm::install(ctx, &unit.libraries, &ctx.ws.target_dir().join(ctx.profile.dir_name()))?;
    Ok(Some(WasmBuild { module: unit.module_path(), klib: unit.klib_path(), environment: wasm.environment }))
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

pub fn native_libraries(ctx: &BuildContext, setup: &TargetSetup, konan_target: &str, include_dev: bool) -> Result<Vec<PathBuf>> {
    let key = format!("native-{konan_target}/{}", if include_dev { "test" } else { "main" });
    let requests = setup.requests(&[Scope::Compile, Scope::Runtime, Scope::Provided], include_dev);
    Ok(ctx.resolve(&key, Platform::Native(konan_target.to_owned()), &requests, &setup.boms)?.files())
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

pub fn build_native(ctx: &BuildContext) -> Result<Vec<NativeBuild>> {
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let setup = ctx.setup(TargetKind::Native)?;
    let srcs = sources(ctx, TargetKind::Native, false, &["kt"]);
    if srcs.kotlin_files().is_empty() {
        return Ok(Vec::new());
    }
    let entry_point = native_entry_point(native, &srcs)?;
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);

    let platforms = native_platforms(ctx)?;
    // Each platform is a separate Kotlin/Native compiler process.
    super::parallel_map(&platforms, |konan_target| -> Result<NativeBuild> {
        let konan_target = konan_target.clone();
        let mut libraries = native_libraries(ctx, &setup, &konan_target, false)?;
        libraries.extend(native_cinterop(ctx, &konan_target)?);
        let unit = NativeCompilation {
            name: &manifest.package.name,
            konan_target: konan_target.clone(),
            kind: native.kind,
            sources: srcs.clone(),
            libraries,
            plugins: &plugins,
            kotlin_args: &kotlin_args,
            entry_point: entry_point.clone(),
            linker_options: &native.linker_options,
            output_dir: ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(&konan_target),
            debug: profile.debug,
            optimize: profile.optimize,
            test_runner: false,
        };
        let what = format!(", {konan_target}");
        let fp_dir = ctx.fingerprint_dir(TargetKind::Native);
        unit.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
        Ok(NativeBuild { konan_target, artifact: unit.artifact(), kind: native.kind })
    })
    .into_iter()
    .collect()
}
