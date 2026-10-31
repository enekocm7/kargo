//! `kargo update`: re-resolves dependencies and rewrites `kargo.lock`, either everything or only
//! selected modules (optionally to a precise version), with a dry-run preview.

use anyhow::{Result, bail};

use super::build;
use super::context::{BuildContext, COMPILER_KEY, UpdatePlan};
use super::{Configuration, resolution_for};
use crate::builtins::PluginSpec;
use crate::lockfile::{LockChange, Lockfile};
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
    let previous = ctx.lockfile();
    ctx.reset_lockfile(None);
    if ctx.update.is_some() {
        ctx.keep_npm_tools(&previous.npm_tools);
    }
    for package in 0..ctx.project.packages().len() {
        resolve_package(&ctx.for_package(package), &previous)?;
    }
    Ok(())
}

fn resolve_package(ctx: &BuildContext, previous: &Lockfile) -> Result<()> {
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
            // Test suites have their own test classpath, locked separately.
            for suite in ctx.ws.manifest().test.suites.values() {
                let suite_setup = ctx.setup_for_suite(target, Some(suite))?;
                let test_key = format!("jvm/test-{}", suite.name);
                ctx.resolve(
                    &ctx.ws.lock_key(&test_key),
                    kargo_maven::Platform::Jvm,
                    &suite_setup.requests(&build::ALL_SCOPES, true),
                    &suite_setup.boms,
                )?;
                build::jvm_processor_path_for_suite(ctx, &suite_setup, true, Some(suite))?;
                build::jvm_ksp_path_for_suite(ctx, &suite_setup, true, Some(suite))?;
            }
        }
        if target == TargetKind::Wasm {
            crate::web_resources::fetch(ctx)?;
            for suite in ctx.ws.manifest().test.suites.values() {
                let suite_setup = ctx.setup_for_suite(target, Some(suite))?;
                build::wasm_resolution_for_suite(ctx, &suite_setup, true, Some(suite))?;
            }
        }
        if target == TargetKind::Js {
            for suite in ctx.ws.manifest().test.suites.values() {
                let suite_setup = ctx.setup_for_suite(target, Some(suite))?;
                build::js_resolution_for_suite(ctx, &suite_setup, true, Some(suite))?;
            }
        }
        if target == TargetKind::Native {
            for suite in ctx.ws.manifest().test.suites.values() {
                let suite_setup = ctx.setup_for_suite(target, Some(suite))?;
                for p in build::native_platforms(ctx)? {
                    build::native_resolution_for_suite(ctx, &suite_setup, &p, true, Some(suite))?;
                }
            }
        }
        relock_tools(ctx, target, previous)?;
    }
    crate::generate::fetch_tools(ctx, &[])?;
    let had = |artifact: &str| previous.resolutions.contains_key(&ctx.tool_key(artifact));
    if had("dokka-cli") {
        super::doc::fetch_tool(ctx)?;
    }
    if had("ktlint-cli") {
        super::fmt::fetch_tool(ctx)?;
    }
    if had("detekt-cli") {
        super::lint::fetch_tool(ctx)?;
    }
    Ok(())
}

/// Locks again the build tools of `target` (see [`super::lock_tools`]) that `previous` had locked, so a `--locked`
/// build still finds them. A tool that was not locked is locked by the first build that runs it, so updating a
/// project that was never built downloads no compiler.
fn relock_tools(ctx: &BuildContext, target: TargetKind, previous: &Lockfile) -> Result<()> {
    let had = |key: &str| previous.resolutions.contains_key(key);
    let setup = ctx.setup(target)?;
    if target != TargetKind::Native && had(COMPILER_KEY) {
        ctx.kotlin_compiler()?;
    }
    let plugins: Vec<PluginSpec> = setup.contributions.plugins.iter().filter(|p| had(&ctx.tool_key(&p.artifact))).cloned().collect();
    ctx.plugin_jars(&plugins)?;
    if matches!(target, TargetKind::Wasm | TargetKind::Js) && previous.npm_tools.contains_key(&ctx.npm_tool_key("typescript")) {
        crate::js_sources::lock_typescript(ctx, target)?;
    }
    if target == TargetKind::Jvm {
        if had(&ctx.tool_key("junit-platform-console-standalone")) {
            super::test::console_launcher(ctx)?;
        }
        if had(&ctx.tool_key("ksp")) && !build::jvm_ksp_path(ctx, &setup, true)?.is_empty() {
            ctx.ksp_tool()?;
        }
        if had(&ctx.ws.lock_key("jvm/bench-tools")) {
            super::bench::fetch(ctx)?;
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
