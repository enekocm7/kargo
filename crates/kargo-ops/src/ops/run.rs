//! `kargo run`: builds and runs the program.

use std::process::Command;

use anyhow::{Context, Result, bail};

use super::build::{BuildOptions, build, hook_env};
use super::context::BuildContext;
use kargo_manifest::{Hook, NativeKind, TargetKind};
use kargo_toolchain::native::Host;
use kargo_util::fs::join_paths;
use kargo_util::process;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub target: Option<TargetKind>,
    pub release: bool,
    pub args: Vec<String>,
}

/// Picks the target to run: explicit, else JVM, Native, Wasm, Android in that order.
pub fn runnable_target(enabled: &[TargetKind], requested: Option<TargetKind>) -> Result<TargetKind> {
    if let Some(t) = requested {
        if !enabled.contains(&t) {
            bail!("target `{t}` is not enabled in kargo.toml");
        }
        return Ok(t);
    }
    [TargetKind::Jvm, TargetKind::Native, TargetKind::Wasm, TargetKind::Android]
        .into_iter()
        .find(|t| enabled.contains(t))
        .context("no runnable target is enabled")
}

/// Builds and runs; returns the program's exit code.
pub fn run(ctx: &BuildContext, opts: &RunOptions) -> Result<i32> {
    let target = runnable_target(&ctx.ws.manifest().targets.enabled(), opts.target)?;
    let results = build(ctx, &BuildOptions { targets: vec![target], release: opts.release })?;
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx))?;

    let root = ctx.ws.root();
    let rel = |p: &std::path::Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut label;
    let mut cmd = match target {
        TargetKind::Jvm => {
            let jvm = results.jvm.context("JVM build produced no output")?;
            let main = jvm
                .main_class
                .clone()
                .context("could not determine the main class: add a top-level `fun main` or set `main-class` in [target.jvm]")?;
            let jdk = ctx.jdk()?;
            let mut classpath = vec![jvm.classes.clone()];
            classpath.extend(jvm.runtime_classpath.iter().cloned());
            let mut cmd = Command::new(jdk.java());
            cmd.args(&jvm.jvm_args).arg("-cp").arg(join_paths(&classpath)).arg(&main);
            label = format!("java {main}");
            cmd
        }
        TargetKind::Wasm => {
            let wasm = results.wasm.context("there are no Wasm sources to run")?;
            let mut cmd = Command::new(kargo_toolchain::node()?);
            cmd.arg(&wasm.module);
            label = format!("node {}", rel(&wasm.module));
            cmd
        }
        TargetKind::Native => {
            let host = Host::current()?.konan_target;
            let build = results
                .native
                .iter()
                .find(|b| b.konan_target == host)
                .with_context(|| format!("the native target has no build for this host ({host}); add \"host\" to `platforms`"))?;
            if build.kind != NativeKind::Executable {
                bail!("the native target builds a library, not an executable");
            }
            label = rel(&build.artifact);
            Command::new(&build.artifact)
        }
        TargetKind::Android => return crate::gradle::run(ctx),
    };
    cmd.args(&opts.args).current_dir(ctx.ws.root());
    for arg in &opts.args {
        label.push(' ');
        label.push_str(arg);
    }
    ctx.shell().status("Running", format!("`{label}`"));
    ctx.shell().verbose("Command", process::describe(&cmd));
    let status = cmd.status().with_context(|| format!("failed to start `{}`", cmd.get_program().to_string_lossy()))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_preference() {
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], None).unwrap(), TargetKind::Jvm);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Native], None).unwrap(), TargetKind::Native);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], Some(TargetKind::Wasm)).unwrap(), TargetKind::Wasm);
        assert!(runnable_target(&[TargetKind::Jvm], Some(TargetKind::Native)).is_err());
        assert!(runnable_target(&[], None).is_err());
    }
}
