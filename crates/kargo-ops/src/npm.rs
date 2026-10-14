//! npm packages for Kotlin/Wasm: libraries such as kotlinx-datetime import npm
//! packages (`@js-joda/core`) at runtime. Kargo reads them from the klibs, adds
//! the project's own `[target.wasm.npm-dependencies]` and installs them with
//! npm, pnpm or bun into a `node_modules` that Node.js finds from the compiled
//! `.mjs` files (a browser gets an import map, see `wasm_output`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::ops::context::BuildContext;
use kargo_compiler::wasm::npm_dependencies;
use kargo_manifest::PackageManager;
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::process::{self, find_on_path};

/// Lockfiles the package managers leave next to `package.json`.
const LOCKFILES: [&str; 4] = ["package-lock.json", "pnpm-lock.yaml", "bun.lock", "bun.lockb"];

/// The npm packages the project needs, as `name -> version`: those its Wasm
/// `libraries` ask for, and `[target.wasm.npm-dependencies]`, which wins on a conflict.
pub fn dependencies(ctx: &BuildContext, libraries: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let mut deps = npm_dependencies(libraries)?;
    if let Some(wasm) = &ctx.ws.manifest().targets.wasm {
        deps.extend(wasm.npm_dependencies.clone());
    }
    Ok(deps)
}

/// Installs the npm packages that `libraries` need into `dir/node_modules` and
/// returns that directory, or `None` when no package is needed.
///
/// Node.js resolves bare imports by walking up from the importing module, so
/// any `.mjs` below `dir` sees them. Nothing is installed when `dir` already
/// holds exactly these packages installed by the same package manager.
pub fn install(ctx: &BuildContext, libraries: &[PathBuf], dir: &Path) -> Result<Option<PathBuf>> {
    let deps = dependencies(ctx, libraries)?;
    if deps.is_empty() {
        return Ok(None);
    }
    let (manager, exe) = find_package_manager(ctx, &deps)?;
    install_into(ctx, &deps, dir, manager, exe).map(Some)
}

/// `kargo fetch`: installs the packages [`install`] would, so an offline build finds them. Without
/// npm, pnpm or bun the build could not install them either, so this warns instead of failing.
pub fn fetch(ctx: &BuildContext, libraries: &[PathBuf], dir: &Path) -> Result<()> {
    let deps = dependencies(ctx, libraries)?;
    if deps.is_empty() {
        return Ok(());
    }
    match find_package_manager(ctx, &deps) {
        Ok((manager, exe)) => install_into(ctx, &deps, dir, manager, exe).map(|_| ()),
        Err(e) => {
            ctx.shell().warn(format!("the npm packages were not fetched: {e:#}"));
            Ok(())
        }
    }
}

/// Installs `deps` with `manager` into `dir/node_modules`, unless it already holds exactly these packages.
fn install_into(ctx: &BuildContext, deps: &BTreeMap<String, String>, dir: &Path, manager: PackageManager, exe: PathBuf) -> Result<PathBuf> {
    let packages = package_list(deps);
    let manifest = package_json(deps);
    let installed = format!("{manager}\n{manifest}");
    // Written only after a successful install, so an interrupted one is retried.
    let node_modules = dir.join("node_modules");
    let marker = node_modules.join(".kargo-package.json");
    if std::fs::read_to_string(&marker).is_ok_and(|s| s == installed) {
        return Ok(node_modules);
    }
    // Start clean: package managers don't share `node_modules` layouts or lockfiles.
    remove_dir_all_if_exists(&node_modules)?;
    for lockfile in LOCKFILES {
        let _ = std::fs::remove_file(dir.join(lockfile));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("package.json"), &manifest)?;

    ctx.shell().status("Installing", format!("npm packages {packages} ({manager})"));
    let mut cmd = Command::new(exe);
    cmd.args(install_args(manager, ctx.config.offline())).current_dir(dir);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    // The package manager's own output is only shown when the install fails.
    let output = cmd.output().with_context(|| format!("failed to start {manager}"))?;
    if !output.status.success() {
        let log = format!("{}\n{}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
        bail!(
            "`{manager} install` failed with {} while installing {packages}; check your network connection and {manager} configuration\n\n{}",
            process::describe_status(output.status),
            log.trim()
        );
    }
    std::fs::write(marker, installed)?;
    Ok(node_modules)
}

/// The configured package manager, or else the first of npm, pnpm and bun on `PATH`.
fn find_package_manager(ctx: &BuildContext, deps: &BTreeMap<String, String>) -> Result<(PackageManager, PathBuf)> {
    let preferred = ctx.ws.manifest().targets.wasm.as_ref().and_then(|w| w.package_manager);
    if let Some(manager) = preferred {
        let exe = find_on_path(manager.as_str()).with_context(|| {
            format!("[target.wasm] package-manager is `{manager}`, but `{manager}` is not on PATH; install it or choose npm, pnpm or bun")
        })?;
        return Ok((manager, exe));
    }
    PackageManager::ALL.into_iter().find_map(|m| find_on_path(m.as_str()).map(|exe| (m, exe))).with_context(|| {
        format!(
            "your Wasm dependencies need the npm packages {}; install Node.js (it includes npm), pnpm or bun and put it on PATH",
            package_list(deps)
        )
    })
}

/// The packages as `name@version`, for messages.
fn package_list(deps: &BTreeMap<String, String>) -> String {
    deps.iter().map(|(name, version)| format!("{name}@{version}")).collect::<Vec<_>>().join(", ")
}

/// The arguments that install `package.json` quietly, without touching any parent workspace.
fn install_args(manager: PackageManager, offline: bool) -> Vec<&'static str> {
    let mut args = match manager {
        PackageManager::Npm => vec!["install", "--no-audit", "--no-fund", "--loglevel=error"],
        // pnpm defaults to a frozen lockfile in CI, and there is none here.
        PackageManager::Pnpm => vec!["install", "--ignore-workspace", "--no-frozen-lockfile"],
        PackageManager::Bun => vec!["install", "--no-progress"],
    };
    if offline {
        args.push("--offline");
    }
    args
}

/// The `package.json` Kargo writes to install `deps`.
fn package_json(deps: &BTreeMap<String, String>) -> String {
    let package = serde_json::json!({
        "private": true,
        "description": "Generated by Kargo: the npm packages the project's Kotlin/Wasm libraries depend on.",
        "dependencies": deps,
    });
    format!("{}\n", serde_json::to_string_pretty(&package).expect("a JSON value serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_package_json() {
        let deps = BTreeMap::from([("@js-joda/core".to_owned(), "3.2.0".to_owned())]);
        let json: serde_json::Value = serde_json::from_str(&package_json(&deps)).unwrap();
        assert_eq!(json["private"], true);
        assert_eq!(json["dependencies"]["@js-joda/core"], "3.2.0");
    }

    #[test]
    fn install_arguments() {
        assert_eq!(install_args(PackageManager::Npm, false), ["install", "--no-audit", "--no-fund", "--loglevel=error"]);
        assert_eq!(install_args(PackageManager::Pnpm, true), ["install", "--ignore-workspace", "--no-frozen-lockfile", "--offline"]);
        assert_eq!(install_args(PackageManager::Bun, true), ["install", "--no-progress", "--offline"]);
    }
}
