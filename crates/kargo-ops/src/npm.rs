//! npm packages for Kotlin/Wasm: libraries such as kotlinx-datetime import npm
//! packages (`@js-joda/core`) at runtime. Kargo reads them from the klibs, adds
//! the project's own `[target.wasm.npm-dependencies]` and installs them with
//! npm, pnpm or bun into a `node_modules` that Node.js finds from the compiled
//! `.mjs` files (a browser gets an import map, see `wasm_output`). The project's own
//! packages also get Kotlin declarations generated from their TypeScript ones ([`declarations`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::dts;
use crate::ops::context::BuildContext;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::wasm::npm_dependencies;
use kargo_manifest::{Manifest, PackageManager};
use kargo_maven::Version;
use kargo_util::fs::{collect_files, remove_dir_all_if_exists};
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

/// Where [`declarations`] writes the Kotlin declarations of the npm packages.
pub fn declarations_dir(ws: &Workspace, profile: ProfileKind) -> PathBuf {
    ws.profile_dir(profile).join("generated").join("npm")
}

/// The opt-in Kotlin/Wasm interop needs since Kotlin 2.2 (`JsAny` and friends are
/// `@ExperimentalWasmJsInterop`), so the generated declarations and the code calling them compile
/// without a warning at every use. `None` when the project opts in itself or its Kotlin predates it.
pub fn interop_opt_in(manifest: &Manifest) -> Option<String> {
    const MARKER: &str = "kotlin.js.ExperimentalWasmJsInterop";
    let since = Version::parse(&manifest.kotlin.version) >= Version::parse("2.2.0");
    (since && !manifest.kotlin.opt_in.iter().any(|o| o == MARKER)).then(|| format!("-opt-in={MARKER}"))
}

/// Kotlin declarations for the project's own npm packages (`[target.wasm.npm-dependencies]`),
/// generated from their TypeScript declarations (see [`crate::dts`]) into [`declarations_dir`].
/// Installs the packages first (as the build would after compiling), and `@types/<name>` for each
/// package without declarations of its own. Returns the Kotlin files: none without npm dependencies.
///
/// The files are written again only when the installed packages change.
pub fn declarations(ctx: &BuildContext, libraries: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let out = declarations_dir(ctx.ws, ctx.profile);
    let packages: Vec<String> =
        ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.npm_dependencies.keys().cloned().collect()).unwrap_or_default();
    if packages.is_empty() {
        remove_dir_all_if_exists(&out)?;
        return Ok(Vec::new());
    }
    let profile_dir = ctx.ws.profile_dir(ctx.profile);
    let Some(node_modules) = install(ctx, libraries, &profile_dir)? else { return Ok(Vec::new()) };
    let types: BTreeMap<String, String> = packages
        .iter()
        .filter(|p| !p.starts_with("@types/") && !dts::has_declarations(&node_modules, p))
        .map(|p| (format!("@types/{}", dts::types_name(p)), types_range(&node_modules, p)))
        .collect();

    let installed = std::fs::read_to_string(node_modules.join(".kargo-package.json")).unwrap_or_default();
    let stamp = format!("{}+{}\n{}\n{installed}\n{}", env!("CARGO_PKG_VERSION"), dts::FORMAT, packages.join(","), package_list(&types));
    let stamp_path = out.join(".kargo-stamp");
    if std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == stamp) {
        return Ok(collect_files(&out, &["kt"]));
    }

    let mut roots = vec![node_modules];
    if !types.is_empty() {
        // Apart from the packages the program runs with: a missing `@types` package only costs its declarations.
        let fetched = find_package_manager(ctx, &types)
            .and_then(|(manager, exe)| install_into(ctx, &types, &profile_dir.join("npm-types"), manager, exe));
        match fetched {
            Ok(dir) => roots.push(dir),
            Err(e) => ctx.shell().warn(format!(
                "no TypeScript declarations for {}, so Kotlin gets none: {e:#}",
                types.keys().map(|t| t.trim_start_matches("@types/")).collect::<Vec<_>>().join(", ")
            )),
        }
    }
    ctx.shell().status("Generating", format!("Kotlin declarations for npm packages {}", packages.join(", ")));
    let generated = dts::generate(&packages, &roots, &out)?;
    for package in &generated.untyped {
        ctx.shell().warn(format!(
            "the npm package `{package}` has no TypeScript declarations, so Kotlin gets none; declare what you use with `@JsModule(\"{package}\")` external declarations"
        ));
    }
    std::fs::create_dir_all(&out)?;
    std::fs::write(&stamp_path, stamp)?;
    Ok(generated.files)
}

/// The `@types` version for the installed `package`: the same major version (`^5`).
fn types_range(node_modules: &Path, package: &str) -> String {
    let manifest = package.split('/').fold(node_modules.to_path_buf(), |p, part| p.join(part)).join("package.json");
    let version = std::fs::read_to_string(manifest)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["version"].as_str().map(str::to_owned));
    match version.as_deref().and_then(|v| v.split('.').next()).filter(|m| m.chars().all(|c| c.is_ascii_digit()) && !m.is_empty()) {
        Some(major) => format!("^{major}"),
        None => "*".to_owned(),
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
