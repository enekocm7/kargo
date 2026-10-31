//! High-level operations behind each CLI command.

pub mod add;
pub mod add_npm;
pub mod audit;
pub mod bench;
pub mod build;
pub mod check;
pub mod ci;
pub mod classpath;
pub mod context;
pub mod dist;
pub mod doc;
pub mod doctor;
pub mod env;
pub mod fmt;
pub mod import;
pub mod inspect;
pub mod install;
pub mod lint;
pub mod local;
pub mod metadata;
pub mod new;
pub mod packaging;
pub mod preview;
pub mod publish;
pub mod publish_npm;
pub mod report;
pub mod run;
pub mod self_update;
pub mod target;
pub mod test;
pub mod toolchain;
pub mod tree;
pub mod update;
pub mod upgrade;
pub mod vendor;
pub mod watch;
pub mod why;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use self::build::{build_jvm, jvm_classpaths, native_platforms, native_resolution, select_targets, wasm_resolution};
use self::context::BuildContext;
pub use self::packaging::PackageFormat;
use crate::env::EnvOptions;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_manifest::{Hook, JdkRequest, Scope, TargetKind};
use kargo_maven::{Explanation, Platform, Resolution};
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::shell::Shell;

/// Runs `f` on every item on its own thread and returns the results in item order.
/// A single item runs inline. Used for work that waits on the network, such as fetching. The
/// threads keep the target that `--message-format json` messages are tagged with.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    parallel_limited(items, usize::MAX, f)
}

/// [`parallel_map`] on at most [`kargo_util::jobs::limit`] threads (`--jobs`), taking the items in
/// order. Used to build independent targets (and Native platforms) at the same time: their
/// compilers are separate processes or daemon requests. With one job the items run in order.
pub fn parallel_jobs<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    parallel_limited(items, kargo_util::jobs::limit(), f)
}

fn parallel_limited<T: Sync, R: Send>(items: &[T], limit: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = limit.clamp(1, items.len().max(1));
    if threads == 1 {
        return items.iter().map(&f).collect();
    }
    let f = &f;
    let target = kargo_compiler::diagnostics::current_target();
    let target = target.as_deref();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let next = &next;
    let mut results: Vec<(usize, R)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(move || {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else { return done };
                        done.push((i, kargo_compiler::diagnostics::with_target(target, || f(item))));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))).collect()
    });
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// [`parallel_jobs`] over targets, with each target's compiler messages tagged with its name.
pub fn parallel_targets<R: Send>(targets: &[TargetKind], f: impl Fn(TargetKind) -> R + Sync) -> Vec<R> {
    parallel_jobs(targets, |&target| kargo_compiler::diagnostics::with_target(Some(target.as_str()), || f(target)))
}

/// The default number of packages built at the same time: [`kargo_util::jobs::auto`], the number
/// of CPUs within the free memory.
pub fn default_jobs() -> usize {
    kargo_util::jobs::auto()
}

/// Runs `f` for each package of `order` (which [`Project::dependency_order`] sorted), on up to
/// `jobs` threads. A package starts once every package of `order` it reaches through path
/// dependencies (with `dev`, also those of its tests) has finished; independent packages run at
/// the same time. After a failure no new package starts, or with `keep_going` only the packages
/// that depend on a failed one are held back; they never run (their result is `None`). Results
/// come back in the order of `order`.
///
/// [`Project::dependency_order`]: crate::workspace::Project::dependency_order
pub fn build_graph<T: Send>(
    project: &crate::workspace::Project,
    order: &[usize],
    dev: bool,
    jobs: usize,
    keep_going: bool,
    f: impl Fn(usize) -> Result<T> + Sync,
) -> Vec<Option<Result<T>>> {
    use std::sync::{Condvar, Mutex};

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Waiting,
        Running,
        Done,
        Failed,
    }
    let deps: Vec<Vec<usize>> = order
        .iter()
        .map(|&i| {
            let reached = project.reachable(i, dev);
            (0..order.len()).filter(|&k| reached.contains(&order[k])).collect()
        })
        .collect();
    let jobs = jobs.clamp(1, order.len().max(1));
    if jobs == 1 {
        let mut out: Vec<Option<Result<T>>> = Vec::new();
        for (k, &package) in order.iter().enumerate() {
            let blocked = if keep_going {
                deps[k].iter().any(|&d| !matches!(out[d], Some(Ok(_))))
            } else {
                out.iter().any(|r| matches!(r, Some(Err(_))))
            };
            out.push(if blocked { None } else { Some(f(package)) });
        }
        return out;
    }

    let state = Mutex::new((vec![State::Waiting; order.len()], false));
    let wake = Condvar::new();
    let results: Mutex<Vec<Option<Result<T>>>> = Mutex::new((0..order.len()).map(|_| None).collect());
    let f = &f;
    let target = kargo_compiler::diagnostics::current_target();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let next = {
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            let (states, stop) = &mut *guard;
                            let ready = (0..order.len())
                                .find(|&k| states[k] == State::Waiting && deps[k].iter().all(|&d| states[d] == State::Done));
                            let waiting = states.contains(&State::Waiting);
                            let running = states.contains(&State::Running);
                            match ready {
                                Some(k) if !*stop => {
                                    states[k] = State::Running;
                                    break Some(k);
                                }
                                // Nothing can start: done, stopped, or blocked by a failed dependency.
                                _ if *stop || !waiting || !running => break None,
                                _ => guard = wake.wait(guard).unwrap_or_else(|e| e.into_inner()),
                            }
                        }
                    };
                    let Some(k) = next else {
                        wake.notify_all();
                        return;
                    };
                    let run = || kargo_compiler::diagnostics::with_target(target.as_deref(), || f(order[k]));
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
                    let ok = matches!(result, Ok(Ok(_)));
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.0[k] = if ok { State::Done } else { State::Failed };
                    guard.1 |= !ok && !keep_going;
                    drop(guard);
                    wake.notify_all();
                    match result {
                        Ok(result) => results.lock().unwrap_or_else(|e| e.into_inner())[k] = Some(result),
                        // The other workers stop; the scope re-raises the panic.
                        Err(panic) => std::panic::resume_unwind(panic),
                    }
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// Collects the per-target results of a multi-target operation.
///
/// Without `keep_going` the first failure (in target order) is returned as is. With it,
/// every failed target's error is printed and the operation fails with a summary naming
/// the targets that failed and those that succeeded, like `cargo build --keep-going`.
pub(crate) fn collect_targets<R>(
    ctx: &BuildContext,
    action: &str,
    keep_going: bool,
    results: impl IntoIterator<Item = (TargetKind, Result<R>)>,
) -> Result<Vec<R>> {
    let (mut ok, mut succeeded, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    // When several packages build together, a failure names the package and the target.
    let several = ctx.project.packages().len() > 1;
    let name = ctx.ws.name();
    for (target, result) in results {
        let result = match result {
            Err(e) if several => Err(e.context(format!("could not {action} `{name}` ({target})"))),
            other => other,
        };
        match result {
            Ok(r) => {
                ok.push(r);
                succeeded.push(target);
            }
            Err(e) if !keep_going => return Err(e),
            Err(e) => failed.push((target, e)),
        }
    }
    if failed.len() == 1 && succeeded.is_empty() {
        return Err(failed.remove(0).1);
    }
    if failed.is_empty() {
        return Ok(ok);
    }
    for (target, err) in &failed {
        ctx.shell().error(format!("could not {action} the {target} target: {err:#}"));
    }
    let names = |targets: &[TargetKind]| targets.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ");
    let failed: Vec<TargetKind> = failed.into_iter().map(|(t, _)| t).collect();
    let total = failed.len() + succeeded.len();
    if succeeded.is_empty() {
        bail!("could not {action} {} of {total} targets ({})", failed.len(), names(&failed));
    }
    bail!("could not {action} {} of {total} targets ({}); {} succeeded", failed.len(), names(&failed), names(&succeeded))
}

/// Runs `a` and `b` at the same time, e.g. fetching the compiler while dependencies resolve.
pub fn join<A: Send, B: Send>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> (A, B) {
    std::thread::scope(|s| {
        let handle = s.spawn(a);
        let b = b();
        (handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)), b)
    })
}

/// `kargo clean`: removes the build directory (or one profile of it).
pub fn clean(ws: &Workspace, shell: &Shell, profile: Option<crate::workspace::ProfileKind>) -> Result<PathBuf> {
    let dir = match &profile {
        Some(p) => ws.target_dir().join(p.dir_name()),
        None => ws.target_dir().to_path_buf(),
    };
    if !ws.target_dir().exists() {
        return Ok(dir);
    }
    let lock = ws.lock(shell)?;
    if let Some(profile) = &profile {
        remove_dir_all_if_exists(&dir)?;
        // The files `[[generate]]` steps wrote for the profile.
        remove_dir_all_if_exists(&ws.target_dir().join(crate::workspace::GENERATED_DIR).join(profile.dir_name()))?;
    } else {
        // Everything but the lock this process holds.
        for entry in std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
            let path = entry?.path();
            if path == lock.path() || path == kargo_util::flock::holder_path(lock.path()) {
                continue;
            }
            if path.is_dir() {
                remove_dir_all_if_exists(&path)?;
            } else {
                std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
            }
        }
    }
    Ok(dir)
}

/// Which dependency configuration `kargo tree` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Configuration {
    Compile,
    #[default]
    Runtime,
    Test,
}

/// One resolution of a target's configuration: what `kargo tree` labels it (`jvm`, `native-linux_x64`), its
/// `kargo.lock` key and its inputs.
struct ResolutionInput {
    label: String,
    key: String,
    platform: Platform,
    roots: Vec<kargo_maven::DependencyRequest>,
    boms: Vec<kargo_manifest::Dependency>,
}

/// The resolutions one configuration of one target is made of, without resolving them.
fn resolution_inputs(ctx: &BuildContext, target: TargetKind, configuration: Configuration) -> Result<Vec<ResolutionInput>> {
    let setup = ctx.setup(target)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let test = configuration == Configuration::Test;
    let input =
        |label: String, key: String, platform: Platform, roots| ResolutionInput { label, key, platform, roots, boms: setup.boms.clone() };
    match target {
        TargetKind::Jvm => {
            let (key, roots) = match configuration {
                Configuration::Compile => ("jvm/compile", setup.requests(&[Scope::Compile, Scope::Provided], false)),
                Configuration::Runtime => ("jvm/runtime", setup.requests(&[Scope::Compile, Scope::Runtime], false)),
                Configuration::Test => ("jvm/test", setup.requests(&all_scopes, true)),
            };
            Ok(vec![input("jvm".to_owned(), key.to_owned(), Platform::Jvm, roots)])
        }
        TargetKind::Wasm => {
            let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
            let key = if test { "wasm/test" } else { "wasm/main" };
            let platform = Platform::Wasm(env.platform().to_owned());
            Ok(vec![input(format!("wasm-{}", env.platform()), key.to_owned(), platform, setup.requests(&all_scopes, test))])
        }
        TargetKind::Js => {
            let key = if test { "js/test" } else { "js/main" };
            Ok(vec![input("js".to_owned(), key.to_owned(), Platform::Js, setup.requests(&all_scopes, test))])
        }
        TargetKind::Native => Ok(native_platforms(ctx)?
            .into_iter()
            .map(|p| {
                let key = format!("native-{p}/{}", if test { "test" } else { "main" });
                input(format!("native-{p}"), key, Platform::Native(p.clone()), setup.requests(&all_scopes, test))
            })
            .collect()),
        TargetKind::Android => bail!("dependency trees for Android are computed by Gradle; run `kargo android -- dependencies`"),
    }
}

/// Resolves one configuration of one target (used by `tree`, `fetch` and `audit`).
pub fn resolution_for(ctx: &BuildContext, target: TargetKind, configuration: Configuration) -> Result<Vec<(String, Resolution)>> {
    resolution_inputs(ctx, target, configuration)?
        .into_iter()
        .map(|i| Ok((i.label, ctx.resolve(&i.key, i.platform, &i.roots, &i.boms)?)))
        .collect()
}

/// Like [`resolution_for`], with the [`Explanation`] of each resolution's versions (for `kargo why`). The
/// resolution is the one `kargo.lock` holds; the explanation comes from resolving again without it.
pub fn explanation_for(
    ctx: &BuildContext,
    target: TargetKind,
    configuration: Configuration,
) -> Result<Vec<(String, Resolution, Explanation)>> {
    resolution_inputs(ctx, target, configuration)?
        .into_iter()
        .map(|i| {
            let resolution = ctx.resolve(&i.key, i.platform.clone(), &i.roots, &i.boms)?;
            let explanation = ctx.explain(&i.key, i.platform, &i.roots, &i.boms)?;
            Ok((i.label, resolution, explanation))
        })
        .collect()
}

/// `kargo fetch`: downloads what later builds need so they can run offline: the compiler and
/// dependencies, the JDK that `[toolchain] jdk` pins, the JUnit launcher and the libraries of `benches/` (JVM), the npm packages
/// (Wasm and JS, when npm, pnpm or bun is on `PATH`) and, with `tools`, the Dokka, ktlint, detekt
/// and R8 jars that `kargo doc`, `fmt`, `lint` and optimized packaging run.
pub fn fetch(ctx: &BuildContext, targets: &[TargetKind], tools: bool) -> Result<()> {
    let targets = select_targets(ctx, targets)?;
    let fetched = parallel_map(&targets, |&target| -> Result<()> {
        match target {
            TargetKind::Jvm => {
                let setup = ctx.setup(target)?;
                let (toolchain, (main, test)) = join(
                    || -> Result<()> {
                        ctx.kotlin_compiler()?;
                        fetch_pinned_jdk(ctx)
                    },
                    || join(|| jvm_classpaths(ctx, &setup), || resolution_for(ctx, target, Configuration::Test)),
                );
                toolchain?;
                let classpaths = main?;
                test?;
                build::announce_frameworks(ctx, |module| classpaths.runtime.node(module).map(|n| n.version.clone()));
                build::jvm_processor_path(ctx, &setup, false)?;
                build::jvm_processor_path(ctx, &setup, true)?;
                build::jvm_ksp_path(ctx, &setup, false)?;
                lock_tools(ctx, target)?;
            }
            TargetKind::Wasm => {
                let setup = ctx.setup(target)?;
                let (compiler, (main, test)) = join(
                    || -> Result<()> {
                        lock_tools(ctx, target)?;
                        fetch_pinned_jdk(ctx)
                    },
                    || join(|| wasm_resolution(ctx, &setup, false), || wasm_resolution(ctx, &setup, true)),
                );
                compiler?;
                let (main, test) = (main?.files(), test?.files());
                // Where `kargo build` and `kargo test` install them (see `npm::install`): the main packages
                // next to the build, and the test ones in the test output when they differ.
                crate::npm::fetch(ctx, target, &main, &ctx.ws.profile_dir(&ctx.profile))?;
                let test_dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Wasm).join("test");
                if crate::npm::test_dependencies(ctx, target, &test)? != crate::npm::dependencies(ctx, target, &main)? {
                    crate::npm::fetch_for_tests(ctx, target, &test, &test_dir)?;
                }
                crate::web_resources::fetch(ctx)?;
            }
            TargetKind::Js => {
                let setup = ctx.setup(target)?;
                let (compiler, (main, test)) = join(
                    || -> Result<()> {
                        lock_tools(ctx, target)?;
                        fetch_pinned_jdk(ctx)
                    },
                    || join(|| build::js_resolution(ctx, &setup, false), || build::js_resolution(ctx, &setup, true)),
                );
                compiler?;
                let (main, test) = (main?.files(), test?.files());
                // Where `kargo build` and `kargo test` install them: next to the module and the test module.
                let dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Js);
                crate::npm::fetch(ctx, target, &main, &dir)?;
                if crate::npm::test_dependencies(ctx, target, &test)? != crate::npm::dependencies(ctx, target, &main)? {
                    crate::npm::fetch_for_tests(ctx, target, &test, &dir.join("test"))?;
                }
            }
            TargetKind::Native => {
                let setup = ctx.setup(target)?;
                for p in native_platforms(ctx)? {
                    native_resolution(ctx, &setup, &p, false)?;
                    native_resolution(ctx, &setup, &p, true)?;
                }
                lock_tools(ctx, target)?;
                ctx.native_toolchain()?;
            }
            TargetKind::Android => crate::gradle::fetch(ctx)?,
        }
        Ok(())
    });
    fetched.into_iter().collect::<Result<()>>()?;
    crate::generate::fetch_tools(ctx, &targets)?;
    if tools {
        fetch_pinned_jdk(ctx)?;
        let (dokka, (ktlint, detekt)) = join(|| doc::fetch_tool(ctx), || join(|| fmt::fetch_tool(ctx), || lint::fetch_tool(ctx)));
        dokka?;
        ktlint?;
        detekt?;
        if targets.contains(&TargetKind::Jvm) {
            ctx.r8_jar(packaging::R8_VERSION)?;
        }
    }
    Ok(())
}

/// Downloads and locks in `kargo.lock` what builds of `target` run besides the project's dependencies: the
/// Kotlin compiler (JVM, Wasm and JS), the frameworks' compiler plugins and, on the JVM, the JUnit console
/// launcher, KSP when a processor needs it and what `kargo bench` runs. `kargo fetch` and `kargo update` call it.
pub(crate) fn lock_tools(ctx: &BuildContext, target: TargetKind) -> Result<()> {
    let setup = ctx.setup(target)?;
    if target != TargetKind::Native {
        ctx.kotlin_compiler()?;
    }
    ctx.plugin_jars(&setup.contributions.plugins)?;
    if matches!(target, TargetKind::Wasm | TargetKind::Js) {
        crate::js_sources::lock_typescript(ctx, target)?;
        crate::bundler::fetch(ctx, target)?;
    }
    if target == TargetKind::Jvm {
        test::console_launcher(ctx)?;
        if !build::jvm_ksp_path(ctx, &setup, true)?.is_empty() {
            ctx.ksp_tool()?;
        }
        bench::fetch(ctx)?;
    }
    Ok(())
}

/// Downloads the JDK that `[toolchain] jdk` pins. `system` is the JDK on the machine, which is left alone.
fn fetch_pinned_jdk(ctx: &BuildContext) -> Result<()> {
    if ctx.ws.manifest().toolchain.jdk != JdkRequest::System {
        ctx.jdk()?;
    }
    Ok(())
}

/// `kargo update`: re-resolves every dependency, ignoring `kargo.lock`.
pub fn update(ctx: &BuildContext) -> Result<()> {
    ctx.reset_lockfile(None);
    let targets: Vec<TargetKind> = ctx.ws.manifest().targets.enabled().into_iter().filter(|t| *t != TargetKind::Android).collect();
    for target in targets {
        for configuration in [Configuration::Compile, Configuration::Runtime, Configuration::Test] {
            if target != TargetKind::Jvm && configuration == Configuration::Compile {
                continue;
            }
            resolution_for(ctx, target, configuration)?;
        }
        if target == TargetKind::Jvm {
            let setup = ctx.setup(target)?;
            build::jvm_processor_path(ctx, &setup, false)?;
            build::jvm_processor_path(ctx, &setup, true)?;
            build::jvm_ksp_path(ctx, &setup, false)?;
            build::jvm_ksp_path(ctx, &setup, true)?;
        }
        if target == TargetKind::Wasm {
            crate::web_resources::fetch(ctx)?;
        }
    }
    crate::generate::fetch_tools(ctx, &[])?;
    ctx.save_lockfile()
}

/// What `kargo package` builds.
#[derive(Debug, Clone, Default)]
pub struct PackageOptions {
    pub target: Option<TargetKind>,
    /// Launch this `[[bin]]` instead of the main class.
    pub bin: Option<String>,
    /// Build the self-contained jar even for a library.
    pub fat: bool,
    /// Shrink and obfuscate the resulting JVM application jar.
    pub optimize: bool,
    /// `--format`; `None` is a jar, or an installer with `desktop`.
    pub format: Option<PackageFormat>,
    /// `--desktop`: package a Compose Desktop application.
    pub desktop: bool,
    /// `--platforms`: the Compose Desktop platforms whose Skiko natives the jar carries.
    pub platforms: Vec<String>,
    /// `--dist`: build an application distribution archive instead of a jar.
    pub dist: Option<DistOptions>,
}

/// `kargo package --dist`.
#[derive(Debug, Clone)]
pub struct DistOptions {
    /// The archives to write: all of [`dist::Archive::DEFAULT`] when empty.
    pub archives: Vec<dist::Archive>,
    /// Write `bin/` start scripts (`false` for `--no-scripts`).
    pub scripts: bool,
}

/// `kargo package`: builds a self-contained ("fat") jar for the JVM target, launching
/// the project's main class or, with `bin`, that `[[bin]]` (`<bin>-<version>-all.jar`).
/// A library (no main class) gets a jar of its own classes and a sources jar instead,
/// unless `fat` asks for the self-contained jar. Other formats build a `jlink` runtime image,
/// a GraalVM native executable or a `jpackage` installer (see [`packaging`]). `desktop` packages a
/// Compose Desktop application: an installer by default, or with `platforms` a jar that runs on
/// each of them (see [`crate::compose`]).
pub fn package(ctx: &BuildContext, opts: &PackageOptions) -> Result<Vec<PathBuf>> {
    let targets = &ctx.ws.manifest().targets;
    let target = match opts.target {
        Some(t) if !targets.is_enabled(t) => bail!("target `{t}` is not enabled in kargo.toml"),
        Some(t) => t,
        None if !targets.is_enabled(TargetKind::Jvm) && targets.is_enabled(TargetKind::Android) && !opts.desktop => TargetKind::Android,
        None => TargetKind::Jvm,
    };
    if opts.dist.is_some() {
        if let Some(format) = opts.format {
            bail!("`--dist` writes an application distribution and does not combine with `--format {}`", format.as_str());
        }
        if opts.fat || opts.optimize || opts.desktop {
            bail!("`--dist` does not combine with `--fat`, `--optimize` or `--desktop`");
        }
        if target != TargetKind::Jvm {
            bail!("`--dist` packages a JVM application and needs the jvm target, not {target}");
        }
    }
    let format = opts.format.unwrap_or(if opts.desktop { PackageFormat::Installer } else { PackageFormat::Jar });
    let platforms = crate::compose::parse_platforms(&opts.platforms)?;
    if opts.desktop {
        if !crate::compose::enabled(ctx.ws.manifest()) {
            bail!("`--desktop` packages a Compose Desktop application; enable Compose with `[frameworks] compose = true`");
        }
        if target != TargetKind::Jvm {
            bail!("`--desktop` packages the jvm target, not {target}; Compose for Android is packaged with `kargo package -t android`");
        }
    }
    if !platforms.is_empty() {
        if !opts.desktop {
            bail!("`--platforms` needs `--desktop`: it adds the Compose Desktop natives of other platforms to the jar");
        }
        if format != PackageFormat::Jar {
            bail!(
                "`--platforms` builds a jar with the Skiko natives of several platforms and needs `--format jar`; `--format {}` builds for the OS it runs on",
                format.as_str()
            );
        }
    }
    if format != PackageFormat::Jar {
        if target != TargetKind::Jvm {
            bail!("`--format {}` packages a JVM application and needs the jvm target, not {target}", format.as_str());
        }
        if opts.fat {
            bail!("`--fat` builds a jar and does not apply to `--format {}`", format.as_str());
        }
    }
    if opts.optimize && (target != TargetKind::Jvm || format != PackageFormat::Jar) {
        bail!("`--optimize` shrinks JVM jars and needs `--format jar` on the jvm target");
    }
    match target {
        TargetKind::Jvm => package_jvm(
            ctx,
            opts.bin.as_deref(),
            opts.fat,
            opts.optimize || (ctx.profile == ProfileKind::Release && format == PackageFormat::Jar),
            format,
            &platforms,
            opts.dist.as_ref(),
        ),
        TargetKind::Android => {
            if opts.bin.is_some() || opts.fat {
                bail!("`--bin` and `--fat` build JVM jars and do not apply to `kargo package -t android`");
            }
            package_android(ctx)
        }
        other => bail!("`kargo package` packages the jvm and android targets, not {other}; use `kargo build` for {other}"),
    }
}

/// `kargo package -t android`: signed (when configured) APKs and App Bundles, or AARs.
fn package_android(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let start = std::time::Instant::now();
    let mut env = build::hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PreBuild, &env)?;
    let files = crate::gradle::package(ctx)?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostBuild, &env)?;
    env.push(("KARGO_PACKAGE".to_owned(), files[0].to_string_lossy().into_owned()));
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostPackage, &env)?;
    build::finished(ctx, start);
    Ok(files)
}

fn package_jvm(
    ctx: &BuildContext,
    bin: Option<&str>,
    fat: bool,
    optimize: bool,
    format: PackageFormat,
    platforms: &[&str],
    dist: Option<&DistOptions>,
) -> Result<Vec<PathBuf>> {
    if !ctx.ws.manifest().targets.is_enabled(TargetKind::Jvm) {
        bail!("`kargo package` creates an executable jar and needs the JVM target; use `kargo build` for other targets");
    }
    let bin_main = match bin {
        Some(name) => Some(build::bin_main_class(ctx.ws.root(), build::find_bin(ctx.ws.manifest(), name)?)?),
        None => None,
    };
    let start = std::time::Instant::now();
    let env = build::hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PreBuild, &env)?;
    build::generate_sources(ctx, &[TargetKind::Jvm])?;
    build::warn_about_sources(ctx, &[TargetKind::Jvm]);
    let jvm = build_jvm(ctx)?;
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostBuild, &env)?;
    let pkg = &ctx.ws.manifest().package;
    let name = bin.unwrap_or(&pkg.name);
    let app = |bin_main: Option<String>| -> Result<packaging::App> {
        let Some(main_class) = bin_main.or_else(|| jvm.main_class.clone()) else {
            bail!(
                "`--format {}` packages an application, but the package has no main class: {}",
                format.as_str(),
                jvm.main_class_error.as_deref().unwrap_or("set [target.jvm] main-class")
            );
        };
        Ok(packaging::App { name, main_class, jvm: &jvm, dir: ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("package") })
    };
    let package_main_class = bin_main.clone().or_else(|| jvm.main_class.clone());
    let mut files = match format {
        _ if dist.is_some() => distribution(ctx, &jvm, bin, dist.expect("checked by the guard"))?,
        PackageFormat::Jar if bin.is_none() && !fat && platforms.is_empty() && jvm.main_class.is_none() => {
            vec![publish::package_library(ctx, &jvm)?]
        }
        PackageFormat::Jar => vec![fat_jar(ctx, &jvm, name, bin_main, platforms)?],
        PackageFormat::Jlink => vec![packaging::jlink_image(ctx, &app(bin_main)?)?],
        PackageFormat::NativeImage => vec![packaging::native_image(ctx, &app(bin_main)?)?],
        PackageFormat::Installer => packaging::installer(ctx, &app(bin_main)?)?,
    };
    if optimize {
        if format != PackageFormat::Jar {
            bail!("`--optimize` applies to JVM jars; use `--format jar`");
        }
        files[0] = packaging::optimize_jar(ctx, &files[0], name, package_main_class.as_deref())?;
    }
    let mut env = env;
    env.push(("KARGO_PACKAGE".to_owned(), files[0].to_string_lossy().into_owned()));
    crate::plugins::run_hook(ctx.config, ctx.ws, Hook::PostPackage, &env)?;
    build::finished(ctx, start);
    Ok(files)
}

/// `--dist`: the start scripts are the `--bin` one, or every `[[bin]]` with a JVM main class plus the
/// package's own main class (unless a bin has its package's name).
fn distribution(ctx: &BuildContext, jvm: &build::JvmBuild, bin: Option<&str>, opts: &DistOptions) -> Result<Vec<PathBuf>> {
    let manifest = ctx.ws.manifest();
    let own = manifest.package.name.as_str();
    let mut programs: Vec<dist::Program> = Vec::new();
    if let Some(name) = bin {
        let main_class = build::bin_main_class(ctx.ws.root(), build::find_bin(manifest, name)?)?;
        programs.push(dist::Program { name: name.to_owned(), main_class });
    } else {
        for bin in &manifest.bins {
            if let Ok(main_class) = build::bin_main_class(ctx.ws.root(), bin) {
                programs.push(dist::Program { name: bin.name.clone(), main_class });
            }
        }
        if let Some(main_class) = &jvm.main_class
            && !programs.iter().any(|p| p.name == own)
        {
            programs.insert(0, dist::Program { name: own.to_owned(), main_class: main_class.clone() });
        }
    }
    if opts.scripts && programs.is_empty() {
        bail!(
            "`--dist` writes a start script for each program, but the package has none: {}; declare a [[bin]] or a `main` function, or pass `--no-scripts` for a distribution of jars only",
            jvm.main_class_error.as_deref().unwrap_or("it declares no [[bin]] with a main class")
        );
    }
    let archives = if opts.archives.is_empty() { dist::Archive::DEFAULT.to_vec() } else { opts.archives.clone() };
    dist::dist(ctx, jvm, &programs, &archives, opts.scripts)
}

/// The self-contained `<name>-<version>-all.jar`. With `platforms` (`--desktop --platforms`), it
/// also carries the Skiko natives of those Compose Desktop platforms besides the host's.
fn fat_jar(ctx: &BuildContext, jvm: &build::JvmBuild, name: &str, bin_main: Option<String>, platforms: &[&str]) -> Result<PathBuf> {
    let pkg = &ctx.ws.manifest().package;
    let dest = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join(format!("{name}-{}-all.jar", pkg.version));
    let manifest = JarManifest {
        main_class: bin_main.or_else(|| jvm.main_class.clone()),
        class_path: Vec::new(),
        implementation_title: Some(pkg.name.clone()),
        implementation_version: Some(pkg.version.clone()),
        ..Default::default()
    };
    let mut jars: Vec<PathBuf> = jvm.runtime_classpath.iter().filter(|p| p.extension().is_some_and(|e| e == "jar")).cloned().collect();
    if platforms.is_empty() {
        ctx.shell().status("Packaging", format!("{name} v{} ({})", pkg.version, dest.display()));
    } else {
        ctx.shell().status("Packaging", format!("{name} v{} for {} ({})", pkg.version, platforms.join(", "), dest.display()));
        jars.retain(|j| !crate::compose::is_skiko_runtime_jar(j));
        jars.extend(skiko_runtime_jars(ctx, jvm, platforms)?);
    }
    match &pkg_shade(ctx) {
        Some(shade) => {
            shade_jar(ctx, &dest, jvm, &manifest, &jars, shade).context("failed to create the fat jar")?;
        }
        None => write_jar(&dest, &jvm.classes, &manifest, &jars).context("failed to create the fat jar")?,
    }
    Ok(dest)
}

/// `[package.shade]` of the package, if it has one.
pub(crate) fn pkg_shade(ctx: &BuildContext) -> Option<kargo_manifest::ShadeSettings> {
    ctx.ws.manifest().packaging.shade.clone()
}

/// Writes `dest` with `jars` bundled and `shade` applied, and says what needs attention. Returns the jars that
/// put classes into it.
pub(crate) fn shade_jar(
    ctx: &BuildContext,
    dest: &std::path::Path,
    jvm: &build::JvmBuild,
    manifest: &JarManifest,
    jars: &[PathBuf],
    shade: &kargo_manifest::ShadeSettings,
) -> Result<Vec<PathBuf>> {
    let report = kargo_compiler::shade::write_shaded_jar(dest, &jvm.classes, manifest, jars, shade)?;
    if !report.duplicates.is_empty() {
        let shown: Vec<&str> = report.duplicates.iter().take(10).map(String::as_str).collect();
        let more = report.duplicates.len() - shown.len();
        ctx.shell().warn(format!(
            "{} files exist in more than one jar with different contents, the first one was kept: {}{}",
            report.duplicates.len(),
            shown.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() }
        ));
    }
    if shade.minimize {
        ctx.shell().note(format!("minimize removed {} classes of the dependencies", report.removed));
    }
    Ok(report.bundled)
}

/// The Skiko native library jars of `platforms`, in the version the build resolved for the host.
fn skiko_runtime_jars(ctx: &BuildContext, jvm: &build::JvmBuild, platforms: &[&str]) -> Result<Vec<PathBuf>> {
    let Some(version) = crate::compose::skiko_version(&jvm.runtime_modules) else {
        bail!("`--platforms` adds Skiko's natives, but the build resolved no `org.jetbrains.skiko:skiko-awt-runtime-*` dependency");
    };
    let fetched = parallel_map(platforms, |platform| {
        let artifact = crate::compose::skiko_runtime_artifact(platform);
        ctx.tool_jar(crate::compose::SKIKO_GROUP, &artifact, version)
            .with_context(|| format!("failed to fetch {}:{artifact}:{version} for {platform}", crate::compose::SKIKO_GROUP))
    });
    fetched.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::workspace::Project;

    #[test]
    fn limited_parallel_map_keeps_the_item_order_and_the_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let items: Vec<usize> = (0..12).collect();
        for limit in [1, 3] {
            let (running, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
            let started = Mutex::new(Vec::new());
            let out = parallel_limited(&items, limit, |&i| {
                most.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                started.lock().unwrap().push(i);
                std::thread::sleep(std::time::Duration::from_millis(5));
                running.fetch_sub(1, Ordering::SeqCst);
                i * 2
            });
            assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
            assert!(most.load(Ordering::SeqCst) <= limit);
            if limit == 1 {
                assert_eq!(started.into_inner().unwrap(), items, "one job runs the items in order");
            }
        }
    }

    /// `a` <- `b` <- `d`, and `c` on its own.
    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, deps: &str| {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
            let manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n[dependencies]\n{deps}");
            std::fs::write(dir.path().join(name).join("kargo.toml"), manifest).unwrap();
        };
        std::fs::write(dir.path().join("kargo.toml"), "[workspace]\nmembers = [\"*\"]\n").unwrap();
        write("a", "");
        write("b", "a = { path = \"../a\" }\n");
        write("c", "");
        write("d", "b = { path = \"../b\" }\n");
        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        (dir, project)
    }

    #[test]
    fn build_graph_runs_dependencies_first() {
        let (_dir, project) = project();
        let order = project.dependency_order(project.members(), false);
        for jobs in [1, 4] {
            let log = Mutex::new(Vec::new());
            let results = build_graph(&project, &order, false, jobs, false, |i| {
                let name = project.package(i).name().to_owned();
                log.lock().unwrap().push(format!("start {name}"));
                std::thread::sleep(std::time::Duration::from_millis(20));
                log.lock().unwrap().push(format!("end {name}"));
                Ok(name)
            });
            let names: Vec<String> = results.into_iter().map(|r| r.unwrap().unwrap()).collect();
            assert_eq!(names, order.iter().map(|&i| project.package(i).name().to_owned()).collect::<Vec<_>>());
            let log = log.into_inner().unwrap();
            let at = |entry: &str| log.iter().position(|l| l == entry).unwrap();
            assert!(at("end a") < at("start b") && at("end b") < at("start d"), "{log:?}");
            if jobs > 1 {
                // `c` does not wait for anything.
                assert!(at("start c") < at("end a"), "{log:?}");
            }
        }
    }

    #[test]
    fn build_graph_keeps_going_past_a_failure() {
        let (_dir, project) = project();
        let order = project.dependency_order(project.members(), false);
        for jobs in [1, 3] {
            let results = build_graph(&project, &order, false, jobs, true, |i| match project.package(i).name() {
                "a" => anyhow::bail!("a broke"),
                name => Ok(name.to_owned()),
            });
            let by_name = |name: &str| &results[order.iter().position(|&i| project.package(i).name() == name).unwrap()];
            assert!(matches!(by_name("a"), Some(Err(_))));
            // `c` does not depend on `a`, so it still builds; `b` and `d` are skipped.
            assert!(matches!(by_name("c"), Some(Ok(_))));
            assert!(by_name("b").is_none() && by_name("d").is_none());
        }
    }

    #[test]
    fn build_graph_stops_after_a_failure() {
        let (_dir, project) = project();
        let order = project.dependency_order(project.members(), false);
        for jobs in [1, 3] {
            let results = build_graph(&project, &order, false, jobs, false, |i| match project.package(i).name() {
                "a" => anyhow::bail!("a broke"),
                name => Ok(name.to_owned()),
            });
            let by_name = |name: &str| &results[order.iter().position(|&i| project.package(i).name() == name).unwrap()];
            assert!(matches!(by_name("a"), Some(Err(_))));
            // `b` and `d` depend on `a` and never run.
            assert!(by_name("b").is_none() && by_name("d").is_none());
        }
    }
}
