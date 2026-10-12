//! Path dependencies: builds the packages a package depends on through `path` and returns
//! the artifacts it compiles and runs against (jars for the JVM, klibs for Wasm and Native).

use std::path::PathBuf;

use anyhow::Result;

use super::build::{build_jvm, compile_wasm, native_klib};
use super::context::BuildContext;
use crate::workspace::Workspace;
use kargo_manifest::{Scope, TargetKind};

/// What a path dependency is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTarget<'a> {
    Jvm,
    Wasm,
    /// A Kotlin/Native platform such as `linux_x64`.
    Native(&'a str),
}

impl LocalTarget<'_> {
    fn kind(self) -> TargetKind {
        match self {
            LocalTarget::Jvm => TargetKind::Jvm,
            LocalTarget::Wasm => TargetKind::Wasm,
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
                LocalTarget::Wasm => Ok(compile_wasm(&child)?.map(|w| w.klib)),
                LocalTarget::Native(konan) => native_klib(&child, konan),
            }
        })?;
        out.extend(artifact);
    }
    Ok(out)
}

/// Fails if `ws` has path dependencies for `target`, which does not support them yet.
pub fn unsupported(ws: &Workspace, target: TargetKind) -> Result<()> {
    if let Some((dep, _)) = ws.path_dependencies().iter().find(|(d, _)| d.target.is_none_or(|t| t == target)) {
        anyhow::bail!(
            "`{}` has the path dependency `{}`, but path dependencies are not supported for the {target} target yet",
            ws.name(),
            dep.name
        );
    }
    Ok(())
}
