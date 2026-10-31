//! `kargo build`: compiles every enabled target.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::{BuildContext, TargetSetup};
use super::local::{self, LocalTarget};
use crate::env::{EnvOptions, build_config_package, build_config_source};
use crate::native_interop;
use crate::warnings;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::jar::{JarManifest, manifest_matches, write_jar};
use kargo_compiler::js::JsCompilation;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::ksp::KspRun;
use kargo_compiler::mainclass::{EntryPoint, find_entry_point_in, find_entry_points, find_jvm_main_class, find_single_entry_point};
use kargo_compiler::native::{CinteropUnit, NativeCompilation};
use kargo_compiler::wasm::WasmCompilation;
use kargo_compiler::{Outcome, SourceSet, common_kotlin_args};
use kargo_manifest::{
    Bin, Cinterop, GeneratedKind, Hook, JsEnvironment, JsKind, JsModuleKind, Manifest, NativeKind, Scope, TargetKind, WasmEnvironment,
    WasmKind,
};
use kargo_maven::{ModuleId, Platform, Resolution};
use kargo_toolchain::native::{buildable_on_host, resolve_platform};
use kargo_util::fs::collect_files;

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Targets to build; empty means every enabled target.
    pub targets: Vec<kargo_manifest::TargetKind>,
    /// The profile to build: `dev`, `release` (`--release`) or a custom one (`--profile <name>`).
    pub profile: ProfileKind,
    /// Build every target even when one fails, then report all failures (`--keep-going`).
    pub keep_going: bool,
    /// Combine the Apple frameworks into an XCFramework (`--xcframework`).
    pub xcframework: bool,
    /// Also write `Package.swift` and a podspec next to the XCFramework (`--swift-package`, `--cocoapods`).
    pub apple_files: crate::apple_package::AppleFiles,
    /// Print Binaryen's per-function Wasm metrics (release builds only).
    pub size_report: bool,
    /// The kind of targets to use instead of `targets` or the defaults (`--target-set`).
    pub target_set: TargetSet,
}

/// `--target-set`: the targets a command uses by where their code runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TargetSet {
    /// The usual choice: `-t`, else `[build] default-targets`, else every enabled target.
    #[default]
    All,
    /// Targets that build and run on this machine with no device, emulator or Apple hardware: JVM, Wasm, JS
    /// and Native when this host's platform is one of `platforms`.
    Host,
    /// Targets whose apps run on a phone or an emulator: Android.
    Device,
}

impl BuildOptions {
    /// The targets this operation applies to: `targets`, else the `target_set`, else the defaults.
    pub fn select(&self, ctx: &BuildContext) -> Result<Vec<TargetKind>> {
        if !self.targets.is_empty() || self.target_set == TargetSet::All {
            return select_targets(ctx, &self.targets);
        }
        let enabled = ctx.ws.manifest().targets.enabled();
        let mut chosen = Vec::new();
        for target in enabled {
            let in_set = match (self.target_set, target) {
                (TargetSet::Device, target) => target == TargetKind::Android,
                (_, TargetKind::Android) => false,
                (_, TargetKind::Native) => {
                    let host = kargo_toolchain::native::Host::current()?.konan_target;
                    native_platforms(ctx)?.iter().any(|p| p == host)
                }
                _ => true,
            };
            if in_set {
                chosen.push(target);
            }
        }
        if chosen.is_empty() {
            let (name, why) = match self.target_set {
                TargetSet::Device => ("device", "Android is the only target that runs on a device; add it with `kargo target add android`"),
                _ => ("host", "Native counts only when this machine's platform is in `platforms` of [target.native]"),
            };
            bail!("no enabled target is in the {name} set: {why}");
        }
        Ok(chosen)
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
    /// JVM options for `kargo run`: `[target.jvm] jvm-args`, then the profile's `jvm-args`.
    pub jvm_args: Vec<String>,
    /// The resolved Maven modules of the runtime classpath and their versions.
    pub runtime_modules: Vec<(ModuleId, String)>,
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
    /// The directory of the bundled site, when the app was bundled (`bundler` in `kargo.toml`).
    pub bundle: Option<PathBuf>,
}

impl WasmBuild {
    /// The directory `kargo run` serves a browser app from: the bundle, else the compiler's output.
    pub fn site_dir(&self) -> &Path {
        self.bundle.as_deref().unwrap_or(&self.dir)
    }
}

#[derive(Debug, Clone)]
pub struct JsBuild {
    /// The linked module: `<name>.mjs`, or `<name>.js` for CommonJS and UMD.
    pub module: PathBuf,
    pub klib: PathBuf,
    pub environment: JsEnvironment,
    pub kind: JsKind,
    pub module_kind: JsModuleKind,
    /// The module's name: `module-name`, else the package name. It names the files in `dir`.
    pub name: String,
    /// The output directory (`target/<profile>/js`).
    pub dir: PathBuf,
    /// The klibs the module was compiled against.
    pub libraries: Vec<PathBuf>,
    /// The `node_modules` holding the npm packages the module imports, once installed.
    pub node_modules: Option<PathBuf>,
    /// The directory of the bundled site, when the app was bundled (`bundler` in `kargo.toml`).
    pub bundle: Option<PathBuf>,
}

impl JsBuild {
    /// The directory `kargo run` serves a browser app from: the bundle, else the compiler's output.
    pub fn site_dir(&self) -> &Path {
        self.bundle.as_deref().unwrap_or(&self.dir)
    }
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
    pub js: Option<JsBuild>,
    pub native: Vec<NativeBuild>,
    pub android: Option<PathBuf>,
}

/// The targets an operation uses without `-t`: `[build] default-targets` of the package, else the
/// ones of `[workspace.build]` that the package enables, else every enabled target.
pub fn default_targets(ctx: &BuildContext) -> Vec<TargetKind> {
    let manifest = ctx.ws.manifest();
    let enabled = manifest.targets.enabled();
    if let Some(own) = &manifest.build.default_targets {
        return enabled.into_iter().filter(|t| own.contains(t)).collect();
    }
    if let Some(shared) = ctx.ws.workspace().and_then(|w| w.build.default_targets.as_ref()) {
        let chosen: Vec<TargetKind> = enabled.iter().copied().filter(|t| shared.contains(t)).collect();
        if !chosen.is_empty() {
            return chosen;
        }
    }
    enabled
}

/// Resolves which targets an operation applies to.
pub fn select_targets(ctx: &BuildContext, requested: &[TargetKind]) -> Result<Vec<TargetKind>> {
    let enabled = ctx.ws.manifest().targets.enabled();
    if requested.is_empty() {
        return Ok(default_targets(ctx));
    }
    for t in requested {
        if !enabled.contains(t) {
            bail!("target `{t}` is not enabled in kargo.toml; add a [target.{t}] section");
        }
    }
    Ok(requested.to_vec())
}

/// The sources of `target` (the tests' with `tests`), after the `[[generate]]` steps it uses ran.
pub fn sources(ctx: &BuildContext, target: TargetKind, tests: bool, extensions: &[&str]) -> Result<SourceSet> {
    crate::generate::ensure(ctx, target)?;
    existing_sources(ctx, target, tests, extensions)
}

/// Like [`sources`], but runs no `[[generate]]` step: the generated files are whatever the last build wrote.
/// `kargo metadata` reads the sources this way.
pub fn existing_sources(ctx: &BuildContext, target: TargetKind, tests: bool, extensions: &[&str]) -> Result<SourceSet> {
    let ws = ctx.ws;
    let collect = |dirs: &[PathBuf]| dirs.iter().flat_map(|d| collect_files(d, extensions)).collect::<Vec<_>>();
    let (common, platform) = if tests {
        (collect(&ws.common_test_dirs()), collect(&ws.target_test_dirs(target)))
    } else {
        (collect(&ws.common_source_dirs()), collect(&ws.target_source_dirs(target)))
    };
    let mut set = SourceSet::new(common, platform, target.as_str());
    set.groups = source_groups(ws, target, tests, extensions);
    if target == TargetKind::Native {
        set.groups.extend(platform_groups(ws, tests, extensions));
    }
    add_generated_sources(&mut set, crate::generate::sources(ws, &ctx.profile, target, tests, extensions));
    if !tests {
        add_build_config(ctx, target, &mut set)?;
        add_compose_resources(ctx, target, &mut set)?;
    }
    Ok(set)
}

/// The intermediate source sets (`[source-sets.<name>]`) that `target` compiles, with their files.
pub fn source_groups(ws: &Workspace, target: TargetKind, tests: bool, extensions: &[&str]) -> Vec<kargo_compiler::SourceGroup> {
    ws.manifest()
        .source_groups_for(target)
        .into_iter()
        .map(|(group, parents)| {
            let dirs = if tests { ws.group_test_dirs(&group.name) } else { ws.group_source_dirs(&group.name) };
            let files = dirs.iter().flat_map(|d| collect_files(d, extensions)).collect::<Vec<_>>();
            kargo_compiler::SourceGroup::new(&group.name, files, &parents)
        })
        .collect()
}

/// The source sets of only some Native platforms (`platforms` of `[source-sets.<name>]`), all of them: the files of
/// the Native target's sources are in `src-native/`, the whole-target sets and these.
fn platform_groups(ws: &Workspace, tests: bool, extensions: &[&str]) -> Vec<kargo_compiler::SourceGroup> {
    ws.manifest()
        .native_platform_groups(None)
        .into_iter()
        .map(|(group, parents)| {
            let dirs = if tests { ws.group_test_dirs(&group.name) } else { ws.group_source_dirs(&group.name) };
            let files = dirs.iter().flat_map(|d| collect_files(d, extensions)).collect::<Vec<_>>();
            // A set that refines no other one is below the target's own fragment.
            let refines = if parents.is_empty() { vec![TargetKind::Native.as_str()] } else { parents };
            kargo_compiler::SourceGroup::new(&group.name, files, &refines)
        })
        .collect()
}

/// `sources` of the Native target without the source sets of some platforms: the code every platform compiles, where
/// the entry points of the programs are looked for (platform sets could hold a `main` each).
pub fn without_platform_sets(ctx: &BuildContext, mut sources: SourceSet) -> SourceSet {
    let platform_sets: Vec<&str> = ctx.ws.manifest().native_platform_groups(None).into_iter().map(|(g, _)| g.name.as_str()).collect();
    sources.groups.retain(|g| !platform_sets.contains(&g.name.as_str()));
    sources
}

/// `sources` of the Native target for one platform: without the source sets of other platforms, whose code does not
/// compile for `konan_target`.
pub fn for_platform(ctx: &BuildContext, mut sources: SourceSet, konan_target: &str) -> SourceSet {
    let manifest = ctx.ws.manifest();
    let own: Vec<&str> = manifest.native_platform_groups(Some(konan_target)).into_iter().map(|(g, _)| g.name.as_str()).collect();
    let whole: Vec<&str> = manifest.source_groups_for(TargetKind::Native).into_iter().map(|(g, _)| g.name.as_str()).collect();
    sources.groups.retain(|g| own.contains(&g.name.as_str()) || whole.contains(&g.name.as_str()));
    sources
}

/// Adds the files of `[[generate]]` steps; see [`add_generated_source`].
fn add_generated_sources(sources: &mut SourceSet, paths: Vec<PathBuf>) {
    for path in paths {
        add_generated_source(sources, path);
    }
}

/// Adds a generated source file to the platform fragment, unless that would make a `src/`-only
/// compilation multiplatform (as with KSP and npm declarations).
fn add_generated_source(sources: &mut SourceSet, path: PathBuf) {
    if sources.has_platform_kotlin() {
        sources.platform.push(path);
    } else {
        sources.common.push(path);
    }
}

/// Generates the `Res` class of `composeResources/` for the JVM, Wasm and Native targets and adds it
/// to the main sources; [`compose_resources_dir`] holds the files it reads. (Android's comes from `gradle.rs`.)
fn add_compose_resources(ctx: &BuildContext, target: TargetKind, sources: &mut SourceSet) -> Result<()> {
    if target == TargetKind::Android {
        return Ok(());
    }
    let out = compose_resources_out(ctx.ws, &ctx.profile, target);
    if let Some(generated) = crate::compose::resources::generate(ctx.ws.root(), ctx.ws.manifest(), &out)? {
        // `Res` only uses the common API of the resources runtime, and code in `src/` reads it, so it
        // goes in the common fragment even when the target has sources of its own.
        sources.common.push(generated.source);
    }
    Ok(())
}

/// The resources that the generated `Res` class of `target` reads, once [`sources`] wrote them.
pub fn compose_resources_dir(ctx: &BuildContext, target: TargetKind) -> PathBuf {
    compose_resources_out(ctx.ws, &ctx.profile, target).join("resources")
}

/// Where the `Res` class of `target` and its resources are generated: under the target's output
/// directory, except on Wasm, whose output directory is emptied before each compilation
/// (`target/<profile>/wasm-generated/compose-resources` there).
pub fn compose_resources_out(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> PathBuf {
    match target {
        TargetKind::Wasm => ws.profile_dir(profile).join("wasm-generated").join("compose-resources"),
        _ => crate::compose::resources::output_dir(&ws.output_dir(profile, target)),
    }
}

/// Writes the `BuildConfig` object of `[env] expose` into the build directory and adds it to the
/// main sources of `target`, where the program gets the environment (see `program_env_applies`).
/// The file is rewritten only when its contents change, so unchanged builds stay incremental.
fn add_build_config(ctx: &BuildContext, target: TargetKind, sources: &mut SourceSet) -> Result<()> {
    let manifest = ctx.ws.manifest();
    // A `builtin = "build-config"` step writes the object itself, with these variables in it.
    if manifest.env.expose.is_empty() || !ctx.program_env_applies(target) || crate::generate::build_config_owner(manifest, target).is_some()
    {
        return Ok(());
    }
    let env = ctx.resolve_env(&EnvOptions::default())?;
    let package = build_config_package(manifest.package.group.as_deref(), &manifest.package.name);
    let contents = build_config_source(&package, &manifest.env.expose, &env);
    let path = ctx.ws.output_dir(&ctx.profile, target).join("generated").join("env").join("BuildConfig.kt");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(contents.as_str()) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        std::fs::write(&path, &contents).with_context(|| format!("failed to write {}", path.display()))?;
    }
    add_generated_source(sources, path);
    Ok(())
}

/// Runs the `[[generate]]` steps `targets` use, if they are out of date. Call it after the pre-build
/// hooks, which may write their inputs, and before [`warn_about_sources`].
pub fn generate_sources(ctx: &BuildContext, targets: &[TargetKind]) -> Result<()> {
    crate::generate::ensure_all(ctx, targets)
}

/// Warns about sources that are missing for `targets` or that Kargo ignores. Call it after the
/// pre-build hooks, which may generate sources, and after [`generate_sources`].
pub fn warn_about_sources(ctx: &BuildContext, targets: &[TargetKind]) {
    // A target that only has generated sources has some.
    let bare: Vec<TargetKind> =
        targets.iter().copied().filter(|&t| crate::generate::sources(ctx.ws, &ctx.profile, t, false, &["kt", "java"]).is_empty()).collect();
    let mut messages = warnings::missing_sources(ctx.ws, &bare);
    messages.extend(warnings::ignored_files(ctx.ws));
    messages.extend(warnings::untyped_modules(ctx.ws, targets));
    warnings::emit(ctx.shell(), ctx.project, ctx.ws, messages);
}

/// Downloads the Kotlin/Native dependencies (LLVM, sysroots) of the platforms this host builds
/// before the pre-build hooks run, since they may use the bundled clang. Without such hooks, or
/// without the Native target among `targets`, the first link downloads them as usual.
pub fn prepare_native_hooks(ctx: &BuildContext, targets: &[TargetKind]) -> Result<()> {
    if !targets.contains(&TargetKind::Native) || !crate::plugins::has_hook(ctx.ws, Hook::PreBuild) {
        return Ok(());
    }
    let toolchain = ctx.native_toolchain()?;
    for konan_target in native_platforms(ctx)?.iter().filter(|t| buildable_on_host(t)) {
        toolchain.ensure_dependencies(ctx.config, konan_target)?;
    }
    Ok(())
}

pub fn build(ctx: &BuildContext, opts: &BuildOptions) -> Result<BuildResults> {
    let start = Instant::now();
    let targets = opts.select(ctx)?;
    prepare_native_hooks(ctx, &targets)?;
    let env = hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PreBuild, &env)?;
    generate_sources(ctx, &targets)?;
    warn_about_sources(ctx, &targets);

    let mut results = BuildResults::default();
    // Targets don't depend on each other, so they build at the same time.
    let built = super::parallel_targets(&targets, |target| -> Result<BuildResults> {
        let mut r = BuildResults::default();
        match target {
            TargetKind::Jvm => r.jvm = Some(build_jvm(ctx)?),
            TargetKind::Wasm => r.wasm = build_wasm(ctx, opts.size_report)?,
            TargetKind::Js => r.js = build_js(ctx)?,
            TargetKind::Native => r.native = build_native(ctx)?,
            TargetKind::Android => r.android = Some(crate::gradle::build(ctx)?),
        }
        Ok(r)
    });
    for r in super::collect_targets(ctx, "build", opts.keep_going, targets.iter().copied().zip(built))? {
        results.jvm = results.jvm.or(r.jvm);
        results.wasm = results.wasm.or(r.wasm);
        results.js = results.js.or(r.js);
        results.native.extend(r.native);
        results.android = results.android.or(r.android);
    }
    if let Some(jvm) = &results.jvm {
        announce_frameworks(ctx, |module| jvm.runtime_modules.iter().find(|(m, _)| m == module).map(|(_, version)| version.clone()));
    }
    if opts.xcframework {
        let xcframework = create_xcframework(ctx, &results.native)?;
        if opts.apple_files.swift_package || opts.apple_files.cocoapods {
            let frameworks: Vec<&str> = results
                .native
                .iter()
                .filter(|b| b.kind == NativeKind::Framework && b.bin.is_none())
                .map(|b| b.konan_target.as_str())
                .collect();
            for file in crate::apple_package::write(&xcframework, &ctx.ws.manifest().package, &frameworks, opts.apple_files)? {
                ctx.shell().status("Writing", file.display());
            }
        }
    }

    let mut env = env;
    if let Some(jvm) = &results.jvm {
        env.push(("KARGO_JVM_JAR".to_owned(), jvm.jar.to_string_lossy().into_owned()));
        let mut cp = vec![jvm.classes.clone()];
        cp.extend(jvm.runtime_classpath.iter().cloned());
        env.push(("KARGO_CLASSPATH".to_owned(), kargo_util::fs::join_paths(&cp)));
    }
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostBuild, &env)?;
    finished(ctx, start);
    Ok(results)
}

/// The environment of build hooks and plugins: the project's variables, then Kargo's own.
/// `opts` picks the `.env` files, as `--env-file` and `--no-env-file` do for `kargo run`.
pub fn hook_env(ctx: &BuildContext, opts: &EnvOptions) -> Result<Vec<(String, String)>> {
    let mut env = ctx.resolve_env(opts)?.for_child();
    env.extend(crate::plugins::base_env(ctx.ws, ctx.profile.name()));
    // Hooks run while this process holds the build lock; a `kargo` they call must not wait on it.
    if let Some(lock) = ctx.build_lock_path() {
        env.push((kargo_util::flock::HELD_LOCK_ENV.to_owned(), lock.to_string_lossy().into_owned()));
    }
    Ok(env)
}

pub fn finished(ctx: &BuildContext, start: Instant) {
    let detail = match ctx.profile {
        ProfileKind::Dev => "unoptimized + debuginfo".to_owned(),
        ProfileKind::Release => "optimized".to_owned(),
        ProfileKind::Custom(_) => match ctx.ws.profile(&ctx.profile) {
            Ok(p) => format!("{}{}", if p.optimize { "optimized" } else { "unoptimized" }, if p.debug { " + debuginfo" } else { "" }),
            Err(_) => "custom".to_owned(),
        },
    };
    ctx.shell()
        .status("Finished", format!("`{}` profile [{detail}] target(s) in {:.2}s", ctx.profile.name(), start.elapsed().as_secs_f64()));
}

/// Prints each enabled framework's version as a `Resolved` status line, with the versions of the modules its BOM
/// manages that `resolved` gives. `kargo fetch` and `kargo build` print them, so a migration-sensitive choice (Spring
/// Boot 3 or 4, Jackson 2 or 3) shows without opening `kargo.lock`.
pub fn announce_frameworks(ctx: &BuildContext, resolved: impl Fn(&ModuleId) -> Option<String>) {
    for framework in crate::builtins::framework_versions(ctx.ws.manifest(), resolved) {
        ctx.shell().status("Resolved", framework.describe());
    }
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
    let (mut source_roots, dir, what) = if unit.test {
        (ws.test_dirs(TargetKind::Jvm), "test", ", tests, ksp")
    } else {
        (ws.source_dirs(TargetKind::Jvm), "main", ", ksp")
    };
    // The processors see what `[[generate]]` steps wrote too, as classes in `src/` may use it.
    let generated = if unit.test { GeneratedKind::TestSources } else { GeneratedKind::Sources };
    source_roots.extend(crate::generate::dirs(ws, &ctx.profile, TargetKind::Jvm, generated));
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
        output: ws.output_dir(&ctx.profile, TargetKind::Jvm).join("generated").join("ksp").join(dir),
        project_dir: ws.root(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let generated = run.run(ctx.shell(), unit.jdk, &fp_dir, &|| announce(ctx, "Processing", TargetKind::Jvm, what))?;
    // Generated files join the platform fragment, unless that would turn a single-fragment
    // compilation (only `src/` has Kotlin) into a multiplatform one.
    if sources.has_platform_kotlin() {
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
    crate::builtins::check_compose_stability_config(manifest, ctx.ws.root())?;
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
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);

    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm);
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    let classes = out.join("classes");
    let jvm_target = jvm.target_for(jdk.version)?;
    let mut srcs = sources(ctx, TargetKind::Jvm, false, &["kt", "java"])?;
    let mut resources = ctx.ws.resource_dirs();
    resources.extend(crate::generate::resource_dirs(ctx.ws, &ctx.profile, TargetKind::Jvm));
    let compose_resources = compose_resources_dir(ctx, TargetKind::Jvm);
    if compose_resources.is_dir() {
        resources.push(compose_resources);
    }
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
    // Fail before any toolchain downloads when an explicit main class cannot exist in
    // pure Kotlin sources; Java, generated and KSP sources defer to the compiled output below.
    if let Some(main) = &jvm.main_class {
        validate_jvm_main_early(ctx, main, "`[target.jvm] main-class`")?;
    }
    for bin in &manifest.bins {
        if let Some(main) = &bin.main {
            validate_jvm_main_early(ctx, main, &format!("bin '{}'", bin.name))?;
        }
    }
    let JvmClasses { classes, sources: srcs, classpaths, outcome } = compile_jvm(ctx, "Compiling")?;
    let changed = outcome == Outcome::Compiled;
    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm);

    // A library has no main class, so a failed detection only matters to `kargo run`.
    let (main_class, main_class_error) = match &jvm.main_class {
        Some(m) => (Some(m.clone()), None),
        None => match find_jvm_main_class(&srcs.kotlin_files(), "[target.jvm] main-class") {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(e.to_string())),
        },
    };

    // An explicit main class must name a class in the compiled output (a Java main counts);
    // the Kotlin mains are listed as the "multiple `main` functions" error does.
    if let Some(main) = &jvm.main_class {
        validate_jvm_main_class(ctx, &classes, main, "`[target.jvm] main-class`")?;
    }
    for bin in &manifest.bins {
        if let Some(main) = &bin.main {
            validate_jvm_main_class(ctx, &classes, main, &format!("bin '{}'", bin.name))?;
        }
    }

    // Thin jar plus dependencies in lib/.
    let runtime_classpath = jvm_runtime_classpath(ctx, &classpaths)?;
    let lib_dir = out.join("lib");
    let lib_names = sync_lib_dir(&lib_dir, &runtime_classpath)?;
    let jar = out.join(format!("{}-{}.jar", manifest.package.name, manifest.package.version));
    let jar_manifest = JarManifest {
        main_class: main_class.clone(),
        class_path: lib_names.iter().map(|n| format!("lib/{n}")).collect(),
        implementation_title: Some(manifest.package.name.clone()),
        implementation_version: Some(manifest.package.version.clone()),
        ..Default::default()
    };
    // The compile fingerprint doesn't cover the main class or the runtime classpath, so a change to
    // either alone shows up only in the manifest.
    if changed || !manifest_matches(&jar, &jar_manifest) {
        write_jar(&jar, &classes, &jar_manifest, &[])?;
        kargo_compiler::abi::update_stamp(&jar)?;
    } else if !kargo_compiler::abi::stamp_path(&jar).is_file() {
        kargo_compiler::abi::update_stamp(&jar)?;
    }

    // The profile's options come after `[target.jvm] jvm-args`, so they override them.
    let jvm_args = jvm.jvm_args.iter().chain(&ctx.ws.profile(&ctx.profile)?.jvm_args).cloned().collect();
    let runtime_modules = classpaths.runtime.nodes.iter().map(|n| (n.module.clone(), n.version.clone())).collect();
    Ok(JvmBuild { classes, jar, lib_dir, runtime_classpath, main_class, main_class_error, jvm_args, runtime_modules })
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
/// A jar in the resolution is not a klib but files for the page, such as Skiko's runtime (see
/// [`crate::web_resources`]), so it is left out.
pub fn wasm_libraries(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Vec<PathBuf>> {
    let mut libraries = local::artifacts(ctx, LocalTarget::Wasm, &ALL_SCOPES, include_dev)?;
    libraries.extend(wasm_resolution(ctx, setup, include_dev)?.files().into_iter().filter(|f| !is_jar(f)));
    Ok(libraries)
}

fn is_jar(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar"))
}

pub(crate) const ALL_SCOPES: [Scope; 3] = [Scope::Compile, Scope::Runtime, Scope::Provided];

/// Compiles the Wasm target and adds what surrounds the module: the page of a browser app, the
/// `package.json` of a library, and in a release build `wasm-opt` and the size report.
pub fn build_wasm(ctx: &BuildContext, size_report: bool) -> Result<Option<WasmBuild>> {
    let Some(build) = prepare_wasm(ctx)? else { return Ok(None) };
    let mut build = build;
    crate::wasm_output::finish(ctx, &mut build, size_report)?;
    Ok(Some(build))
}

/// Compiles the Wasm target and installs the npm packages it imports (enough to run its tests).
pub fn prepare_wasm(ctx: &BuildContext) -> Result<Option<WasmBuild>> {
    let Some(mut build) = compile_wasm(ctx, true)? else { return Ok(None) };
    // Outside the Wasm output directory, which is wiped on every recompile;
    // Node.js still finds `target/<profile>/node_modules` from the `.mjs`.
    build.node_modules = crate::npm::install(ctx, TargetKind::Wasm, &build.libraries, &ctx.ws.profile_dir(&ctx.profile))?;
    crate::js_sources::install(ctx, TargetKind::Wasm, &build.dir, &build.dir, build.node_modules.as_deref())?;
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
    let profile = ctx.ws.profile(&ctx.profile)?;
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut srcs = sources(ctx, TargetKind::Wasm, false, &["kt"])?;
    add_npm_declarations(ctx, TargetKind::Wasm, &libraries, &mut srcs, &mut kotlin_args)?;
    let unit = WasmCompilation {
        name: wasm.module_name(&manifest.package),
        environment: wasm.environment,
        sources: srcs,
        libraries,
        friends: Vec::new(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: ctx.ws.output_dir(&ctx.profile, TargetKind::Wasm),
        debug: profile.debug,
        debug_info: wasm.debug_info,
        optimize: profile.optimize,
        array_range_checks: wasm.wasm_enable_array_range_checks,
        debugger_custom_formatters: wasm.wasm_debugger_custom_formatters,
        // An application's too, for the TypeScript modules and any other code that imports it.
        declarations: true,
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
        bundle: None,
    }))
}

/// Adds the Kotlin declarations generated for the project's npm packages (see
/// [`crate::npm::declarations`]) and its own TypeScript modules (see [`crate::js_sources::declarations`])
/// to the main `sources` of `target` (Wasm or JS), and the opt-in they need to `kotlin_args`.
pub(crate) fn add_npm_declarations(
    ctx: &BuildContext,
    target: TargetKind,
    libraries: &[PathBuf],
    sources: &mut SourceSet,
    kotlin_args: &mut Vec<String>,
) -> Result<()> {
    let mut generated = crate::npm::declarations(ctx, target, libraries)?;
    generated.extend(crate::js_sources::declarations(ctx, target)?);
    if generated.is_empty() {
        return Ok(());
    }
    // As with KSP: the platform fragment, unless only `src/` has Kotlin, which then stays one fragment.
    if sources.has_platform_kotlin() {
        sources.platform.extend(generated);
    } else {
        sources.common.extend(generated);
    }
    npm_opt_in(ctx, target, kotlin_args);
    Ok(())
}

/// Adds the opt-in that calls into the npm declarations need, when the project has npm dependencies
/// or TypeScript modules with declarations on `target`.
pub(crate) fn npm_opt_in(ctx: &BuildContext, target: TargetKind, kotlin_args: &mut Vec<String>) {
    let manifest = ctx.ws.manifest();
    let npm = match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref().is_some_and(|w| !w.npm_dependencies.is_empty()),
        TargetKind::Js => manifest.targets.js.as_ref().is_some_and(|j| !j.npm_dependencies.is_empty()),
        _ => false,
    };
    if (npm && crate::npm::declarations_supported(manifest, target)) || crate::js_sources::has_declarations(ctx.ws, target) {
        kotlin_args.extend(crate::npm::interop_opt_in(manifest));
    }
}

/// The resolved Maven klibs of the Kotlin/JS target.
pub fn js_resolution(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Resolution> {
    let key = if include_dev { "js/test" } else { "js/main" };
    ctx.resolve(key, Platform::Js, &setup.requests(&ALL_SCOPES, include_dev), &setup.boms)
}

/// Every klib the Kotlin/JS sources compile against: path dependencies first, then Maven libraries.
pub fn js_libraries(ctx: &BuildContext, setup: &TargetSetup, include_dev: bool) -> Result<Vec<PathBuf>> {
    let mut libraries = local::artifacts(ctx, LocalTarget::Js, &ALL_SCOPES, include_dev)?;
    libraries.extend(js_resolution(ctx, setup, include_dev)?.files());
    Ok(libraries)
}

/// Where the Kotlin/JS compilation `name` keeps its klib and links its module before copying it to the
/// output directory (see [`kargo_compiler::js`]): `target/<profile>/js-build/<name>`.
pub fn js_work_dir(ctx: &BuildContext, name: &str) -> PathBuf {
    ctx.ws.profile_dir(&ctx.profile).join("js-build").join(name)
}

/// Compiles the Kotlin/JS target and adds what surrounds the module: the page of a browser app and
/// the `package.json` of a library.
pub fn build_js(ctx: &BuildContext) -> Result<Option<JsBuild>> {
    let Some(mut build) = prepare_js(ctx)? else { return Ok(None) };
    crate::js_output::finish(ctx, &mut build)?;
    Ok(Some(build))
}

/// Compiles the Kotlin/JS target and installs the npm packages it imports (enough to run its tests).
pub fn prepare_js(ctx: &BuildContext) -> Result<Option<JsBuild>> {
    let Some(mut build) = compile_js(ctx, true)? else { return Ok(None) };
    // Next to the module, where Node.js finds them; linking leaves the directory's other files alone.
    build.node_modules = crate::npm::install(ctx, TargetKind::Js, &build.libraries, &build.dir)?;
    crate::js_sources::install(ctx, TargetKind::Js, &build.dir, &build.dir, build.node_modules.as_deref())?;
    Ok(Some(build))
}

/// Compiles and links the Kotlin/JS target without installing npm packages (enough for a path dependency).
/// `main_args` passes the program arguments to `main(args)`; off for a path dependency.
pub fn compile_js(ctx: &BuildContext, main_args: bool) -> Result<Option<JsBuild>> {
    let manifest = ctx.ws.manifest();
    let js = manifest.targets.js.as_ref().context("the JS target is not enabled")?;
    let setup = ctx.setup(TargetKind::Js)?;
    let (toolchain, libraries) = super::join(
        || -> Result<_> { Ok((ctx.compiler_jdk()?, ctx.kotlin_compiler()?, ctx.plugin_jars(&setup.contributions.plugins)?)) },
        || js_libraries(ctx, &setup, false),
    );
    let (jdk, compiler, plugins) = toolchain?;
    let libraries = libraries?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let mut kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut srcs = sources(ctx, TargetKind::Js, false, &["kt"])?;
    add_npm_declarations(ctx, TargetKind::Js, &libraries, &mut srcs, &mut kotlin_args)?;
    let name = js.module_name(&manifest.package);
    let unit = JsCompilation {
        name,
        environment: js.environment,
        module_kind: js.module_kind,
        sources: srcs,
        libraries,
        friends: Vec::new(),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        output: ctx.ws.output_dir(&ctx.profile, TargetKind::Js),
        work: js_work_dir(ctx, name),
        root: ctx.ws.root().to_path_buf(),
        debug: profile.debug,
        optimize: profile.optimize,
        // An application's too, for the TypeScript modules and any other code that imports it.
        declarations: true,
        main_args,
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Js);
    if unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| compiling(ctx, TargetKind::Js, ""))? == Outcome::Empty {
        return Ok(None);
    }
    Ok(Some(JsBuild {
        module: unit.module_path(),
        klib: unit.klib_path(),
        environment: js.environment,
        kind: js.kind,
        module_kind: js.module_kind,
        name: unit.name.to_owned(),
        dir: unit.output.clone(),
        libraries: unit.libraries,
        node_modules: None,
        bundle: None,
    }))
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

/// The reason `konan_target` cannot be built on this host, if there is one.
pub fn native_host_limit(konan_target: &str) -> Option<&'static str> {
    if buildable_on_host(konan_target) {
        None
    } else if konan_target.starts_with("macos") {
        Some("macOS binaries need macOS with Xcode")
    } else {
        Some("Apple platforms need macOS with Xcode")
    }
}

/// The platforms of the Native target this host can build. The others get a note, so a project
/// with an iOS target still builds its other targets on Windows and Linux.
pub fn buildable_native_platforms(ctx: &BuildContext) -> Result<Vec<String>> {
    let (buildable, skipped): (Vec<String>, Vec<String>) = native_platforms(ctx)?.into_iter().partition(|t| buildable_on_host(t));
    if let Some(first) = skipped.first() {
        let reason = native_host_limit(first).unwrap_or_default();
        ctx.shell().note(format!("skipping native {}: {reason}", skipped.join(", ")));
    }
    Ok(buildable)
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
    let srcs = sources(ctx, TargetKind::Native, false, &["kt"])?;
    if srcs.kotlin_files().is_empty() {
        return Ok(None);
    }
    let setup = ctx.setup(TargetKind::Native)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(konan_target);
    // A library's own build already produces the klib; anything else gets a separate one.
    let (output_dir, fp_dir) = match native.kind {
        NativeKind::Library => (out, ctx.fingerprint_dir(TargetKind::Native)),
        _ => (out.join("klib"), ctx.fingerprint_dir(TargetKind::Native).join("klib")),
    };
    let mut libraries = native_libraries(ctx, &setup, konan_target, false)?;
    libraries.extend(native_cinterop(ctx, konan_target)?);
    let unit = NativeCompilation {
        name: &manifest.package.name,
        konan_target: konan_target.to_owned(),
        kind: NativeKind::Library,
        sources: for_platform(ctx, srcs, konan_target),
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point: None,
        // The klib packs the C sources' library, so whatever links it gets their code.
        link: native_interop::native_link(ctx, konan_target)?,
        output_dir,
        debug: profile.debug,
        optimize: profile.optimize,
        test_runner: false,
        exported: Vec::new(),
        bundle_id: None,
    };
    let what = format!(", {konan_target}");
    unit.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
    Ok(Some(unit.artifact()))
}

/// The `.def` file of every `[target.native.cinterop]` entry (`None` for one described by
/// `headers` or `modules` alone), failing on a missing file, C source or `pkg-config` package
/// before any toolchain is downloaded.
pub fn native_cinterop_defs<'a>(ctx: &BuildContext<'a>) -> Result<Vec<(&'a Cinterop, Option<PathBuf>)>> {
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    native_interop::check(ctx, &native_platforms(ctx)?)?;
    native
        .cinterop
        .iter()
        .map(|interop| {
            let def = ctx.ws.root().join(interop.def_path());
            if def.is_file() {
                Ok((interop, Some(def)))
            } else if interop.def.is_none() && interop.describes_library() {
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
    let output_dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(konan_target).join("cinterop");
    let fp_dir = ctx.fingerprint_dir(TargetKind::Native);
    defs.into_iter()
        .map(|(interop, def)| {
            let options = native_interop::cinterop_options(ctx, interop, konan_target)?;
            let unit = CinteropUnit {
                interop,
                konan_target: konan_target.to_owned(),
                root: ctx.ws.root(),
                def: native_interop::cinterop_def(interop, def, &options, konan_target, &output_dir)?,
                options,
                output_dir: output_dir.clone(),
            };
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

/// Formats entry points for `validate_jvm_main_class` and `validate_native_entry`:
/// `  <name> (<file>)` per line, as the "multiple `main` functions" error does.
fn format_mains(entries: &[EntryPoint], name: impl Fn(&EntryPoint) -> String) -> String {
    entries.iter().map(|e| format!("  {} ({})", name(e), e.file.display())).collect::<Vec<_>>().join("\n")
}

/// Whether the JVM target could have a main class that `find_entry_points` does not see:
/// a Java `main`, a `[[generate]]` step or KSP output. Callers that only have the Kotlin
/// sources defer to the compiled output then instead of failing early.
fn jvm_may_have_unseen_main(ctx: &BuildContext, srcs: &SourceSet) -> bool {
    if !srcs.java_files().is_empty() {
        return true;
    }
    if ctx.ws.manifest().generators.iter().any(|g| g.applies_to(TargetKind::Jvm)) {
        return true;
    }
    // KSP (`scope = "ksp"`) generates sources during compilation.
    if let Ok(setup) = ctx.setup(TargetKind::Jvm)
        && !setup.requests(&[Scope::Ksp], false).is_empty()
    {
        return true;
    }
    false
}

/// Checks an explicit JVM main class (`--main`, `[[bin]] main` or `[target.jvm] main-class`)
/// against the Kotlin mains in `srcs`, failing early when the project is pure Kotlin with
/// no generated sources. Returns whether the caller may continue: `true` when the class
/// matches or when only the compiled output can decide (Java, generated or KSP sources).
fn check_jvm_main_early(ctx: &BuildContext, srcs: &SourceSet, requested: &str, what: &str) -> Result<()> {
    let found = find_entry_points(&srcs.kotlin_files());
    if found.iter().any(|e| e.jvm_class == requested) {
        return Ok(());
    }
    if jvm_may_have_unseen_main(ctx, srcs) {
        return Ok(());
    }
    if found.is_empty() {
        bail!("{what} points to {requested}, which is not in the compiled output");
    }
    let list = format_mains(&found, |e| e.jvm_class.clone());
    bail!("{what} points to {requested}, which is not in the compiled output; available main classes:\n{list}")
}

/// Validates an explicit JVM main class before anything compiles, when the sources already
/// prove it wrong. Defers to [`validate_jvm_main_class`] after compilation otherwise (Java,
/// generated or KSP sources). `what` is `` `--main` ``, `bin 'name'` or `` `[target.jvm] main-class` ``.
pub fn validate_jvm_main_early(ctx: &BuildContext, requested: &str, what: &str) -> Result<()> {
    let srcs = match sources(ctx, TargetKind::Jvm, false, &["kt", "java"]) {
        Ok(srcs) => srcs,
        Err(_) => return Ok(()),
    };
    check_jvm_main_early(ctx, &srcs, requested, what)
}

/// Validates an explicit JVM main class against the compiled output: the class file must exist
/// or a Kotlin `main` must declare it. Lists the Kotlin mains when it does not, as the
/// "multiple `main` functions" error does. `what` is `` `--main` ``, `bin 'name'` or
/// `` `[target.jvm] main-class` ``.
pub fn validate_jvm_main_class(ctx: &BuildContext, classes_dir: &Path, requested: &str, what: &str) -> Result<()> {
    let rel = format!("{}.class", requested.replace('.', "/"));
    if classes_dir.join(&rel).is_file() {
        return Ok(());
    }
    let srcs = sources(ctx, TargetKind::Jvm, false, &["kt"])?;
    let found = find_entry_points(&srcs.kotlin_files());
    if found.iter().any(|e| e.jvm_class == requested) {
        return Ok(());
    }
    if found.is_empty() {
        bail!("{what} points to {requested}, which is not in the compiled output");
    }
    let list = format_mains(&found, |e| e.jvm_class.clone());
    bail!("{what} points to {requested}, which is not in the compiled output; available main classes:\n{list}")
}

/// Validates an explicit Kotlin/Native entry function (`--main`/`--entry-point`, `[[bin]]`
/// `entry-point` or `[target.native] entry-point`) against the top-level `main` functions in
/// `srcs`, listing them as the "multiple `main` functions" error does. `what` is `` `--main` ``,
/// `bin 'name'` or `` `[target.native] entry-point` ``.
pub fn validate_native_entry(srcs: &SourceSet, requested: &str, what: &str) -> Result<()> {
    let found: Vec<EntryPoint> =
        find_entry_points(&srcs.kotlin_files()).into_iter().filter(|e| e.kind == kargo_compiler::mainclass::EntryKind::TopLevel).collect();
    if found.iter().any(|e| e.native_entry() == requested) {
        return Ok(());
    }
    if found.is_empty() {
        bail!("{what} points to {requested}, which is not in the compiled output: no top-level `fun main` found in the sources");
    }
    let list = format_mains(&found, |e| e.native_entry());
    bail!("{what} points to {requested}, which is not in the compiled output; available entry functions:\n{list}")
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
        let entry = native_entry_point(native, srcs)?;
        if let (Some(explicit), Some(_)) = (&native.entry_point, &entry) {
            validate_native_entry(srcs, explicit, "`[target.native] entry-point`")?;
        }
        return Ok(vec![NativeProgram { bin: None, entry_point: entry }]);
    }
    let mut bins = Vec::new();
    for bin in &ctx.ws.manifest().bins {
        if let Some(entry) = bin_entry_point(ctx.ws.root(), bin)? {
            if let Some(explicit) = &bin.entry_point {
                validate_native_entry(srcs, explicit, &format!("bin '{}'", bin.name))?;
            }
            bins.push(NativeProgram { bin: Some(bin.name.clone()), entry_point: Some(entry) });
        }
    }
    if let Some(explicit) = &native.entry_point {
        validate_native_entry(srcs, explicit, "`[target.native] entry-point`")?;
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
    let srcs = sources(ctx, TargetKind::Native, false, &["kt"])?;
    if srcs.kotlin_files().is_empty() {
        return Ok(Vec::new());
    }
    let platforms = buildable_native_platforms(ctx)?;
    if platforms.is_empty() {
        return Ok(Vec::new());
    }
    let programs = native_programs(ctx, native, &without_platform_sets(ctx, srcs.clone()))?;
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);

    // A framework's name is a Swift module name, which cannot have dashes.
    let own_name = match native.kind {
        NativeKind::Framework => manifest.package.name.replace('-', "_"),
        _ => manifest.package.name.clone(),
    };
    let bundle_id = native.bundle_id.clone().or_else(|| manifest.package.group.as_ref().map(|g| format!("{g}.{own_name}")));
    // Each platform is a separate Kotlin/Native compiler process.
    let built = super::parallel_jobs(&platforms, |konan_target| -> Result<Vec<NativeBuild>> {
        let mut libraries = native_libraries(ctx, &setup, konan_target, false)?;
        libraries.extend(native_cinterop(ctx, konan_target)?);
        let link = native_interop::native_link(ctx, konan_target)?;
        let exported = native_interop::exported_klibs(ctx, &setup, konan_target)?;
        let platform_srcs = for_platform(ctx, srcs.clone(), konan_target);
        let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(konan_target);
        let mut builds = Vec::new();
        for program in &programs {
            // Bins link into `bin/` with their own fingerprints, so one named like the package does not clash.
            let (name, output_dir, fp_dir, what) = match &program.bin {
                None => (own_name.as_str(), out.clone(), ctx.fingerprint_dir(TargetKind::Native), format!(", {konan_target}")),
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
                sources: platform_srcs.clone(),
                libraries: libraries.clone(),
                plugins: &plugins,
                kotlin_args: &kotlin_args,
                entry_point: program.entry_point.clone(),
                link: link.clone(),
                output_dir,
                debug: profile.debug,
                optimize: profile.optimize,
                test_runner: false,
                exported: exported.clone(),
                bundle_id: bundle_id.clone(),
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
    install_ios_compose_resources(ctx)?;
    Ok(builds)
}

/// Copies the processed `composeResources/` of a Compose iOS framework to
/// `target/<profile>/native/compose-resources`, the folder the app bundle needs under that name:
/// Compose for iOS reads its resources from `compose-resources/` in the app's main bundle.
fn install_ios_compose_resources(ctx: &BuildContext) -> Result<()> {
    let dest = ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join("compose-resources");
    let generated = compose_resources_dir(ctx, TargetKind::Native);
    kargo_util::fs::remove_dir_all_if_exists(&dest)?;
    if crate::compose::enabled(ctx.ws.manifest()) && generated.is_dir() {
        kargo_util::fs::copy_dir(&generated, &dest)?;
    }
    Ok(())
}

/// The slice of an XCFramework a Konan target's framework goes into: the devices or the
/// simulators of one Apple OS. `None` for a platform that is not Apple's.
pub fn xcframework_slice(konan_target: &str) -> Option<&'static str> {
    Some(match konan_target {
        "ios_arm64" => "ios",
        "ios_x64" | "ios_simulator_arm64" => "ios-simulator",
        t if t.starts_with("macos_") => "macos",
        "tvos_arm64" => "tvos",
        "tvos_x64" | "tvos_simulator_arm64" => "tvos-simulator",
        "watchos_arm32" | "watchos_arm64" | "watchos_device_arm64" => "watchos",
        "watchos_x64" | "watchos_simulator_arm64" => "watchos-simulator",
        _ => return None,
    })
}

/// Combines the frameworks of a build into `target/<profile>/native/<name>.xcframework` with
/// `xcodebuild -create-xcframework`. Frameworks for the same slice (the iOS simulators on arm64 and
/// x64) are merged into one with `lipo` first, since a slice holds a single framework.
pub fn create_xcframework(ctx: &BuildContext, builds: &[NativeBuild]) -> Result<PathBuf> {
    let frameworks: Vec<&NativeBuild> = builds.iter().filter(|b| b.kind == NativeKind::Framework && b.bin.is_none()).collect();
    let Some(first) = frameworks.first() else {
        bail!("`--xcframework` combines frameworks; set kind = \"framework\" in [target.native] and list Apple `platforms`");
    };
    if !cfg!(target_os = "macos") {
        bail!("`--xcframework` runs xcodebuild, which needs macOS with Xcode; build the frameworks here and combine them on a Mac");
    }
    let name = first.artifact.file_stem().context("a framework has a name")?.to_string_lossy().into_owned();
    let mut slices: std::collections::BTreeMap<&str, Vec<&Path>> = std::collections::BTreeMap::new();
    for build in &frameworks {
        let slice = xcframework_slice(&build.konan_target).with_context(|| format!("{} is not an Apple platform", build.konan_target))?;
        slices.entry(slice).or_default().push(&build.artifact);
    }
    let native_dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Native);
    let binary = |framework: &Path| {
        let versioned = framework.join("Versions").join("A").join(&name);
        if versioned.is_file() { versioned } else { framework.join(&name) }
    };
    let mut parts = Vec::new();
    for (slice, frameworks) in slices {
        if let [only] = frameworks.as_slice() {
            parts.push(only.to_path_buf());
            continue;
        }
        let merged = native_dir.join("xcframework").join(slice).join(format!("{name}.framework"));
        kargo_util::fs::remove_dir_all_if_exists(&merged)?;
        std::fs::create_dir_all(merged.parent().expect("a framework has a parent"))?;
        // `cp -R` keeps the symbolic links of a versioned macOS framework.
        let mut copy = std::process::Command::new("cp");
        copy.arg("-R").arg(frameworks[0]).arg(&merged);
        kargo_util::process::run(ctx.shell(), &mut copy, "cp")?;
        let mut lipo = std::process::Command::new("lipo");
        lipo.arg("-create").args(frameworks.iter().map(|f| binary(f))).arg("-output").arg(binary(&merged));
        kargo_util::process::run(ctx.shell(), &mut lipo, "lipo")?;
        parts.push(merged);
    }
    let output = native_dir.join(format!("{name}.xcframework"));
    kargo_util::fs::remove_dir_all_if_exists(&output)?;
    ctx.shell().status("Packaging", format!("{name}.xcframework"));
    let mut xcodebuild = std::process::Command::new("xcodebuild");
    xcodebuild.arg("-create-xcframework");
    for part in &parts {
        xcodebuild.arg("-framework").arg(part);
    }
    xcodebuild.arg("-output").arg(&output);
    kargo_util::process::run(ctx.shell(), &mut xcodebuild, "xcodebuild")?;
    Ok(output)
}
