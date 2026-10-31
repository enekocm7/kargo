//! Path dependencies: builds the packages a package depends on through `path` and returns
//! the artifacts it compiles and runs against (jars for the JVM, klibs for Wasm, JS and Native).

use std::path::PathBuf;

use anyhow::{Context, Result};

use super::build::{build_jvm, compile_js, compile_wasm, native_klib};
use super::context::BuildContext;
use crate::workspace::Workspace;
use kargo_manifest::{Scope, TargetKind};
use kargo_util::fs::collect_files;

/// What a path dependency is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTarget<'a> {
    Jvm,
    Wasm,
    Js,
    /// A Kotlin/Native platform such as `linux_x64`.
    Native(&'a str),
}

impl LocalTarget<'_> {
    fn kind(self) -> TargetKind {
        match self {
            LocalTarget::Jvm => TargetKind::Jvm,
            LocalTarget::Wasm => TargetKind::Wasm,
            LocalTarget::Js => TargetKind::Js,
            LocalTarget::Native(_) => TargetKind::Native,
        }
    }

    fn key(self) -> String {
        match self {
            LocalTarget::Native(konan) => format!("native-{konan}"),
            other => other.kind().as_str().to_owned(),
        }
    }
}

/// Builds every package `ctx`'s package reaches through path dependencies whose scope is in
/// `scopes` (plus, with `dev`, the ones its dev-dependencies reach) and returns their artifacts,
/// nearest first. Each package is built once per invocation.
pub fn artifacts(ctx: &BuildContext, target: LocalTarget, scopes: &[Scope], dev: bool) -> Result<Vec<PathBuf>> {
    let mut deps = ctx.local_dependencies(target.kind(), false)?;
    if dev {
        deps.extend(ctx.local_dependencies(target.kind(), true)?);
    }
    let mut out = Vec::new();
    for dep in deps.iter().filter(|d| scopes.contains(&d.scope)) {
        let key = format!("{}/{}", ctx.project.package(dep.package).name(), target.key());
        let artifact = ctx.built_artifact(key, || {
            let child = ctx.for_package(dep.package);
            match target {
                LocalTarget::Jvm => Ok(Some(build_jvm(&child)?.jar)),
                LocalTarget::Wasm => Ok(compile_wasm(&child, false)?.map(|w| w.klib)),
                LocalTarget::Js => Ok(compile_js(&child, false)?.map(|j| j.klib)),
                LocalTarget::Native(konan) => native_klib(&child, konan),
            }
        })?;
        out.extend(artifact);
    }
    Ok(out)
}

/// The JVM test fixtures jars `ctx`'s tests use: its own, then those of the dev-dependencies
/// declared with `test-fixtures = true`. Each is built once per invocation.
pub fn test_fixtures(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let mut packages = vec![ctx.package];
    for (dep, index) in ctx.ws.path_dependencies() {
        if dep.test_fixtures && dep.applies_to(TargetKind::Jvm, true) && !packages.contains(index) {
            packages.push(*index);
        }
    }
    let mut out = Vec::new();
    for package in packages {
        let name = ctx.project.package(package).name();
        let jar = ctx.built_artifact(format!("{name}/jvm-test-fixtures"), || {
            super::test::build_jvm_test_fixtures(&ctx.for_package(package))
                .with_context(|| format!("could not build the test fixtures of `{name}`"))
        })?;
        out.extend(jar);
    }
    Ok(out)
}

/// Fails if `ctx`'s tests for `target` would need test fixtures, which only the JVM supports yet:
/// its own or those of a `test-fixtures = true` dev-dependency, when they have sources for `target`.
pub fn test_fixtures_unsupported(ctx: &BuildContext, target: TargetKind) -> Result<()> {
    let has_fixtures = |ws: &Workspace| {
        let mut dirs = ws.common_test_fixture_dirs();
        dirs.extend(ws.target_test_fixture_dirs(target));
        for g in ws.manifest().all_groups_for(target) {
            dirs.extend(ws.group_test_fixture_dirs(&g.name));
        }
        dirs.iter().any(|d| !collect_files(d, &["kt"]).is_empty())
    };
    let mut packages = vec![ctx.ws];
    packages.extend(
        ctx.ws
            .path_dependencies()
            .iter()
            .filter(|(d, _)| d.test_fixtures && d.applies_to(target, true))
            .map(|(_, i)| ctx.project.package(*i)),
    );
    if let Some(ws) = packages.into_iter().find(|ws| has_fixtures(ws)) {
        anyhow::bail!(
            "the {target} tests of `{}` would use the test fixtures of `{}`, but test fixtures only work for the JVM target yet; \
             keep JVM-only fixtures in `test-fixtures-jvm/`, or run `kargo test --target jvm`",
            ctx.ws.name(),
            ws.name()
        );
    }
    Ok(())
}
