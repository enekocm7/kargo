//! `kargo update`: re-resolves dependencies and rewrites `kargo.lock`, either everything or only
//! selected modules (optionally to a precise version), with a dry-run preview.

use anyhow::{Result, bail};

use super::build;
use super::context::{BuildContext, UpdatePlan};
use super::{Configuration, resolution_for};
use crate::lockfile::LockChange;
use kargo_manifest::TargetKind;
use kargo_maven::{ModuleId, VersionReq};

#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Only update these modules; every other module keeps its locked version.
    pub modules: Vec<ModuleId>,
    /// Lock the (single) selected module at exactly this version.
    pub precise: Option<String>,
    /// Resolve and report the changes without writing `kargo.lock`.
    pub dry_run: bool,
}

/// Re-resolves dependencies (ignoring cached metadata) and returns how the locked versions changed.
pub fn update(ctx: &mut BuildContext, opts: &UpdateOptions) -> Result<Vec<LockChange>> {
    let previous = ctx.lockfile();
    if let Some(precise) = &opts.precise {
        if opts.modules.len() != 1 {
            bail!("`--precise` needs exactly one dependency to update, like `kargo update group:artifact --precise 1.2.3`");
        }
        if !matches!(precise.parse::<VersionReq>(), Ok(VersionReq::Soft(_))) {
            bail!("`--precise` takes a single version such as `1.2.3`, not `{precise}`");
        }
    }
    for module in &opts.modules {
        if !previous.contains_module(&module.to_string()) {
            bail!("`{module}` is not locked in kargo.lock; check the name with `kargo tree`, or run `kargo update` without arguments");
        }
    }
    ctx.dry_run = opts.dry_run;
    if !opts.modules.is_empty() {
        ctx.update = Some(UpdatePlan { previous: previous.clone(), modules: opts.modules.clone(), precise: opts.precise.clone() });
    }
    let result = resolve_all(ctx).and_then(|()| {
        if let Some(precise) = &opts.precise {
            let module = opts.modules[0].to_string();
            if !ctx.lockfile().module_versions().get(&module).is_some_and(|v| v.contains(precise)) {
                bail!("`{module}` is no longer in the dependency graph after the update; nothing was locked at {precise}");
            }
        }
        Ok(())
    });
    if result.is_err() {
        // Leave kargo.lock as it was rather than saving a partial resolution.
        ctx.dry_run = true;
    }
    result?;
    ctx.save_lockfile()?;
    Ok(previous.diff(&ctx.lockfile()))
}

/// Resolves every configuration of every enabled target of every package into a fresh lockfile.
fn resolve_all(ctx: &BuildContext) -> Result<()> {
    ctx.reset_lockfile(None);
    for package in 0..ctx.project.packages().len() {
        resolve_package(&ctx.for_package(package))?;
    }
    Ok(())
}

fn resolve_package(ctx: &BuildContext) -> Result<()> {
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
            for test in [false, true] {
                build::jvm_processor_path(ctx, &setup, test)?;
                build::jvm_ksp_path(ctx, &setup, test)?;
            }
        }
    }
    Ok(())
}

/// Describes a change for the `Updating` / `Adding` / `Removing` status lines.
pub fn describe(change: &LockChange) -> (&'static str, String) {
    let versions = |v: &[String]| v.iter().map(|v| format!("v{v}")).collect::<Vec<_>>().join(", ");
    match (change.before.is_empty(), change.after.is_empty()) {
        (true, _) => ("Adding", format!("{} {}", change.module, versions(&change.after))),
        (_, true) => ("Removing", format!("{} {}", change.module, versions(&change.before))),
        _ => ("Updating", format!("{} {} -> {}", change.module, versions(&change.before), versions(&change.after))),
    }
}
