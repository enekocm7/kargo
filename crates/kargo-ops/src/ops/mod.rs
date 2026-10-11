//! High-level operations behind each CLI command.

pub mod add;
pub mod build;
pub mod check;
pub mod context;
pub mod doc;
pub mod fmt;
pub mod lint;
pub mod new;
pub mod run;
pub mod test;
pub mod toolchain;
pub mod tree;
pub mod update;
pub mod upgrade;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use self::build::{build_jvm, jvm_classpaths, native_libraries, native_platforms, select_targets, wasm_libraries};
use self::context::BuildContext;
use crate::workspace::Workspace;
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_manifest::{Hook, Scope, TargetKind};
use kargo_maven::{Platform, Resolution};
use kargo_util::fs::remove_dir_all_if_exists;

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

/// Runs `a` and `b` at the same time, e.g. fetching the compiler while dependencies resolve.
pub fn join<A: Send, B: Send>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> (A, B) {
    std::thread::scope(|s| {
        let handle = s.spawn(a);
        let b = b();
        (handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)), b)
    })
}

/// `kargo clean`: removes the build directory (or one profile of it).
pub fn clean(ws: &Workspace, profile: Option<crate::workspace::ProfileKind>) -> Result<PathBuf> {
    let dir = match profile {
        Some(p) => ws.target_dir().join(p.dir_name()),
        None => ws.target_dir().to_path_buf(),
    };
    remove_dir_all_if_exists(&dir)?;
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
            let res = ctx.resolve(key, Platform::Wasm(env.as_str().to_owned()), &setup.requests(&all_scopes, test), &setup.boms)?;
            Ok(vec![(format!("wasm-{}", env.as_str()), res)])
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
            }
            TargetKind::Wasm => {
                let setup = ctx.setup(target)?;
                let (compiler, deps) = join(
                    || ctx.kotlin_compiler().map(|_| ()),
                    || wasm_libraries(ctx, &setup, false).and(wasm_libraries(ctx, &setup, true)),
                );
                compiler?;
                deps?;
            }
            TargetKind::Native => {
                let setup = ctx.setup(target)?;
                for p in native_platforms(ctx)? {
                    native_libraries(ctx, &setup, &p, false)?;
                    native_libraries(ctx, &setup, &p, true)?;
                }
                ctx.native_toolchain()?;
            }
            TargetKind::Android => {
                ctx.shell().note("Android dependencies are fetched by Gradle during `kargo build --target android`");
            }
        }
        Ok(())
    });
    fetched.into_iter().collect()
}

/// `kargo package`: builds a self-contained ("fat") jar for the JVM target.
pub fn package(ctx: &BuildContext) -> Result<PathBuf> {
    if !ctx.ws.manifest().targets.is_enabled(TargetKind::Jvm) {
        bail!("`kargo package` creates an executable jar and needs the JVM target; use `kargo build` for other targets");
    }
    let start = std::time::Instant::now();
    let env = build::hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    let jvm = build_jvm(ctx)?;
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostBuild, &env)?;
    let pkg = &ctx.ws.manifest().package;
    let dest = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join(format!("{}-{}-all.jar", pkg.name, pkg.version));
    ctx.shell().status("Packaging", format!("{} v{} ({})", pkg.name, pkg.version, dest.display()));
    let manifest = JarManifest {
        main_class: jvm.main_class.clone(),
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
