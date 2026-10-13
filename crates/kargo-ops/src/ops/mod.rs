//! High-level operations behind each CLI command.

pub mod add;
pub mod build;
pub mod check;
pub mod context;
pub mod doc;
pub mod fmt;
pub mod lint;
pub mod local;
pub mod metadata;
pub mod new;
pub mod publish;
pub mod run;
pub mod target;
pub mod test;
pub mod toolchain;
pub mod tree;
pub mod update;
pub mod upgrade;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use self::build::{build_jvm, jvm_classpaths, native_platforms, native_resolution, select_targets, wasm_resolution};
use self::context::BuildContext;
use crate::workspace::Workspace;
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_manifest::{Hook, Scope, TargetKind};
use kargo_maven::{Platform, Resolution};
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::shell::Shell;

/// Runs `f` on every item on its own thread and returns the results in item order.
/// A single item runs inline. Used to build independent targets (and Native platforms)
/// at the same time: their compilers are separate processes or daemon requests.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() <= 1 {
        return items.iter().map(&f).collect();
    }
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = items.iter().map(|item| s.spawn(move || f(item))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))).collect()
    })
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
    for (target, result) in results {
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
    let dir = match profile {
        Some(p) => ws.target_dir().join(p.dir_name()),
        None => ws.target_dir().to_path_buf(),
    };
    if !ws.target_dir().exists() {
        return Ok(dir);
    }
    let lock = ws.lock(shell)?;
    if profile.is_some() {
        remove_dir_all_if_exists(&dir)?;
    } else {
        // Everything but the lock this process holds.
        for entry in std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
            let path = entry?.path();
            if path == lock.path() {
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

/// Resolves one configuration of one target (used by `tree` and `fetch`).
pub fn resolution_for(ctx: &BuildContext, target: TargetKind, configuration: Configuration) -> Result<Vec<(String, Resolution)>> {
    let setup = ctx.setup(target)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let test = configuration == Configuration::Test;
    match target {
        TargetKind::Jvm => {
            let res = match configuration {
                Configuration::Compile => jvm_classpaths(ctx, &setup)?.compile,
                Configuration::Runtime => jvm_classpaths(ctx, &setup)?.runtime,
                Configuration::Test => ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?,
            };
            Ok(vec![("jvm".to_owned(), res)])
        }
        TargetKind::Wasm => {
            let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
            let key = if test { "wasm/test" } else { "wasm/main" };
            let res = ctx.resolve(key, Platform::Wasm(env.platform().to_owned()), &setup.requests(&all_scopes, test), &setup.boms)?;
            Ok(vec![(format!("wasm-{}", env.platform()), res)])
        }
        TargetKind::Native => native_platforms(ctx)?
            .into_iter()
            .map(|p| {
                let key = format!("native-{p}/{}", if test { "test" } else { "main" });
                let res = ctx.resolve(&key, Platform::Native(p.clone()), &setup.requests(&all_scopes, test), &setup.boms)?;
                Ok((format!("native-{p}"), res))
            })
            .collect(),
        TargetKind::Android => bail!("dependency trees for Android are computed by Gradle; run `kargo android -- dependencies`"),
    }
}

/// `kargo fetch`: downloads every dependency (and the compiler) so later builds work offline.
pub fn fetch(ctx: &BuildContext, targets: &[TargetKind]) -> Result<()> {
    let targets = select_targets(ctx, targets)?;
    let fetched = parallel_map(&targets, |&target| -> Result<()> {
        match target {
            TargetKind::Jvm => {
                let setup = ctx.setup(target)?;
                let (toolchain, (main, test)) = join(
                    || ctx.kotlin_compiler().and_then(|_| ctx.plugin_jars(&setup.contributions.plugins)),
                    || join(|| jvm_classpaths(ctx, &setup), || resolution_for(ctx, target, Configuration::Test)),
                );
                toolchain?;
                main?;
                test?;
                build::jvm_processor_path(ctx, &setup, false)?;
                build::jvm_processor_path(ctx, &setup, true)?;
                build::jvm_ksp_path(ctx, &setup, false)?;
                if !build::jvm_ksp_path(ctx, &setup, true)?.is_empty() {
                    ctx.ksp_tool()?;
                }
            }
            TargetKind::Wasm => {
                let setup = ctx.setup(target)?;
                let (compiler, deps) = join(
                    || ctx.kotlin_compiler().map(|_| ()),
                    || wasm_resolution(ctx, &setup, false).and(wasm_resolution(ctx, &setup, true)),
                );
                compiler?;
                deps?;
            }
            TargetKind::Native => {
                let setup = ctx.setup(target)?;
                for p in native_platforms(ctx)? {
                    native_resolution(ctx, &setup, &p, false)?;
                    native_resolution(ctx, &setup, &p, true)?;
                }
                ctx.native_toolchain()?;
            }
            TargetKind::Android => crate::gradle::fetch(ctx)?,
        }
        Ok(())
    });
    fetched.into_iter().collect()
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
    }
    ctx.save_lockfile()
}

/// `kargo package`: builds a self-contained ("fat") jar for the JVM target, launching
/// the project's main class or, with `bin`, that `[[bin]]` (`<bin>-<version>-all.jar`).
/// A library (no main class) gets a jar of its own classes and a sources jar instead,
/// unless `fat` asks for the self-contained jar.
pub fn package(ctx: &BuildContext, bin: Option<&str>, fat: bool) -> Result<PathBuf> {
    if !ctx.ws.manifest().targets.is_enabled(TargetKind::Jvm) {
        bail!("`kargo package` creates an executable jar and needs the JVM target; use `kargo build` for other targets");
    }
    let bin_main = match bin {
        Some(name) => Some(build::bin_main_class(ctx.ws.root(), build::find_bin(ctx.ws.manifest(), name)?)?),
        None => None,
    };
    let start = std::time::Instant::now();
    let env = build::hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    build::warn_about_sources(ctx, &[TargetKind::Jvm]);
    let jvm = build_jvm(ctx)?;
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostBuild, &env)?;
    let pkg = &ctx.ws.manifest().package;
    if bin.is_none() && !fat && jvm.main_class.is_none() {
        let jar = publish::package_library(ctx, &jvm)?;
        let mut env = env;
        env.push(("KARGO_PACKAGE".to_owned(), jar.to_string_lossy().into_owned()));
        crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostPackage, &env)?;
        build::finished(ctx, start);
        return Ok(jar);
    }
    let name = bin.unwrap_or(&pkg.name);
    let dest = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join(format!("{name}-{}-all.jar", pkg.version));
    ctx.shell().status("Packaging", format!("{name} v{} ({})", pkg.version, dest.display()));
    let manifest = JarManifest {
        main_class: bin_main.or(jvm.main_class),
        class_path: Vec::new(),
        implementation_title: Some(pkg.name.clone()),
        implementation_version: Some(pkg.version.clone()),
    };
    let jars: Vec<PathBuf> = jvm.runtime_classpath.iter().filter(|p| p.extension().is_some_and(|e| e == "jar")).cloned().collect();
    write_jar(&dest, &jvm.classes, &manifest, &jars).context("failed to create the fat jar")?;
    let mut env = env;
    env.push(("KARGO_PACKAGE".to_owned(), dest.to_string_lossy().into_owned()));
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostPackage, &env)?;
    build::finished(ctx, start);
    Ok(dest)
}
