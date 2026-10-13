//! `kargo classpath`: the resolved classpath (JVM) or klib path (Wasm, Native) of one target,
//! for editors, scripts and tools that need the jars on disk rather than coordinates.
//!
//! It is assembled by the same functions the build compiles with: path dependencies first
//! (built if needed), then the resolved Maven artifacts, including the Kotlin stdlib and what
//! frameworks add. The package's own classes are not part of it.

use std::path::PathBuf;

use anyhow::{Result, bail};

use super::build::{
    jvm_classpaths, jvm_compile_classpath, jvm_runtime_classpath, native_libraries, native_platforms, select_targets, wasm_libraries,
};
use super::context::BuildContext;
use super::local::{self, LocalTarget};
use kargo_manifest::{Scope, TargetKind};
use kargo_maven::Platform;

/// Which classpath `kargo classpath` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClasspathScope {
    /// What the main sources compile against.
    #[default]
    Compile,
    /// What the program needs at runtime.
    Runtime,
    /// What the tests compile and run against, dev-dependencies included.
    Test,
}

impl ClasspathScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ClasspathScope::Compile => "compile",
            ClasspathScope::Runtime => "runtime",
            ClasspathScope::Test => "test",
        }
    }
}

/// The classpath of one target.
#[derive(Debug)]
pub struct Classpath {
    pub target: TargetKind,
    /// `jvm`, `wasm-<environment>` or `native-<platform>`.
    pub platform: String,
    pub entries: Vec<PathBuf>,
}

/// The target `kargo classpath` describes when none is given: the JVM if it is enabled,
/// otherwise the package's only Wasm or Native target.
pub fn default_target(ctx: &BuildContext) -> Result<TargetKind> {
    let enabled = ctx.ws.manifest().targets.enabled();
    if enabled.contains(&TargetKind::Jvm) {
        return Ok(TargetKind::Jvm);
    }
    let candidates: Vec<TargetKind> = enabled.into_iter().filter(|t| *t != TargetKind::Android).collect();
    match candidates.as_slice() {
        [only] => Ok(*only),
        [] => bail!("`{}` has no JVM, Wasm or Native target; the Android classpath is computed by Gradle", ctx.ws.name()),
        _ => bail!("`{}` has several targets; choose one with `--target`", ctx.ws.name()),
    }
}

/// The classpath of `target` (or the [`default_target`]) for `scope`. A Native target's is that
/// of its first platform.
pub fn classpath(ctx: &BuildContext, target: Option<TargetKind>, scope: ClasspathScope) -> Result<Classpath> {
    let target = match target {
        Some(t) => select_targets(ctx, &[t])?[0],
        None => default_target(ctx)?,
    };
    let test = scope == ClasspathScope::Test;
    let classpath = match target {
        TargetKind::Jvm => {
            let setup = ctx.setup(target)?;
            let classpaths = jvm_classpaths(ctx, &setup)?;
            let entries = match scope {
                ClasspathScope::Compile => jvm_compile_classpath(ctx, &classpaths)?,
                ClasspathScope::Runtime => jvm_runtime_classpath(ctx, &classpaths)?,
                ClasspathScope::Test => {
                    // As `kargo test` compiles the tests.
                    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
                    let mut entries = local::artifacts(ctx, LocalTarget::Jvm, &all_scopes, true)?;
                    entries.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
                    entries
                }
            };
            Classpath { target, platform: "jvm".to_owned(), entries }
        }
        TargetKind::Wasm => {
            let setup = ctx.setup(target)?;
            let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
            Classpath { target, platform: format!("wasm-{}", env.platform()), entries: wasm_libraries(ctx, &setup, test)? }
        }
        TargetKind::Native => {
            let setup = ctx.setup(target)?;
            let platform = native_platforms(ctx)?.into_iter().next().unwrap_or_default();
            let entries = native_libraries(ctx, &setup, &platform, test)?;
            Classpath { target, platform: format!("native-{platform}"), entries }
        }
        TargetKind::Android => bail!("the Android classpath is computed by Gradle; run `kargo android -- dependencies`"),
    };
    ctx.save_lockfile()?;
    Ok(classpath)
}
