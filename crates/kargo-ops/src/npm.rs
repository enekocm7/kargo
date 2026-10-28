//! npm packages for Kotlin/Wasm and Kotlin/JS: libraries such as kotlinx-datetime import npm
//! packages (`@js-joda/core`) at runtime. Kargo reads them from the klibs, adds
//! the project's own `[target.wasm.npm-dependencies]` (or `[target.js.npm-dependencies]`) and installs them with
//! npm, pnpm or bun into a `node_modules` that Node.js finds from the compiled
//! `.mjs` files (a browser gets an import map, see `wasm_output`). The project's own packages of
//! either target also get Kotlin declarations generated from their TypeScript ones ([`declarations`]).
//! The declarations use the interop types (`JsAny`, `JsString`, ...) that Kotlin/JS shares with
//! Kotlin/Wasm since Kotlin 2.2.20, so both targets read the same Kotlin.
//!
//! Packages are installed with their lifecycle scripts off (`--ignore-scripts`): the names and
//! versions come from the metadata of Maven libraries, which must not get to run programs on the
//! machine that builds. The package manager's lockfile is kept between installs, so a version range
//! stays at the version it was first installed at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::dts;
use crate::ops::context::BuildContext;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::wasm::npm_dependencies;
use kargo_manifest::{Manifest, PackageManager, TargetKind};
use kargo_maven::Version;
use kargo_util::fs::{collect_files, remove_dir_all_if_exists};
use kargo_util::process::{self, find_on_path};

/// Lockfiles the package managers leave next to `package.json`, with the manager that writes each.
const LOCKFILES: [(&str, PackageManager); 4] = [
    ("package-lock.json", PackageManager::Npm),
    ("pnpm-lock.yaml", PackageManager::Pnpm),
    ("bun.lock", PackageManager::Bun),
    ("bun.lockb", PackageManager::Bun),
];

/// The npm settings of `target` (Wasm or JS): its own `npm-dependencies`, its test-only
/// `dev-npm-dependencies` and its `package-manager`.
fn settings(manifest: &Manifest, target: TargetKind) -> (Option<&BTreeMap<String, String>>, Option<PackageManager>) {
    match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref().map_or((None, None), |w| (Some(&w.npm_dependencies), w.package_manager)),
        TargetKind::Js => manifest.targets.js.as_ref().map_or((None, None), |j| (Some(&j.npm_dependencies), j.package_manager)),
        _ => (None, None),
    }
}

/// The packages only the tests of `target` install (`dev-npm-dependencies`).
pub fn dev_dependencies(manifest: &Manifest, target: TargetKind) -> BTreeMap<String, String> {
    match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref().map(|w| w.dev_npm_dependencies.clone()),
        TargetKind::Js => manifest.targets.js.as_ref().map(|j| j.dev_npm_dependencies.clone()),
        _ => None,
    }
    .unwrap_or_default()
}

/// The npm packages the project needs on `target`, as `name -> version`: those its
/// `libraries` ask for, and `[target.<target>.npm-dependencies]`, which wins on a conflict.
pub fn dependencies(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let mut deps = npm_dependencies(libraries)?;
    if let (Some(own), _) = settings(ctx.ws.manifest(), target) {
        deps.extend(own.clone());
    }
    Ok(deps)
}

/// [`dependencies`] and the packages only the tests install (`dev-npm-dependencies`), which win on a conflict.
pub fn test_dependencies(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let mut deps = dependencies(ctx, target, libraries)?;
    deps.extend(dev_dependencies(ctx.ws.manifest(), target));
    Ok(deps)
}

/// A library that asks for an npm package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRequest {
    /// `group:artifact` of the library.
    pub module: String,
    /// The library's resolved version.
    pub version: String,
    /// The version range of the package the library's klib asks for.
    pub spec: String,
}

/// Where an npm package of a target comes from: the libraries that ask for it and the version `kargo.toml`
/// declares for it, which wins over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub name: String,
    /// What gets installed: the declared version, else the highest one the libraries ask for.
    pub version: String,
    /// The version in `[target.<target>.npm-dependencies]` (or the test-only table, then `dev` is set).
    pub declared: Option<String>,
    pub dev: bool,
    pub requested_by: Vec<LibraryRequest>,
}

/// The npm packages `resolution` (the libraries of `target`) and the manifest need, with where each comes
/// from; `tests` adds the test-only packages. Sorted by name.
pub fn origins(manifest: &Manifest, target: TargetKind, resolution: &kargo_maven::Resolution, tests: bool) -> Result<Vec<Origin>> {
    let mut requests: BTreeMap<String, Vec<LibraryRequest>> = BTreeMap::new();
    let mut files = Vec::new();
    for node in &resolution.nodes {
        for file in &node.files {
            for (name, spec) in kargo_compiler::wasm::klib_npm_dependencies(file)? {
                requests.entry(name).or_default().push(LibraryRequest {
                    module: node.module.to_string(),
                    version: node.version.clone(),
                    spec,
                });
            }
            files.push(file.clone());
        }
    }
    let mut versions = npm_dependencies(&files)?;
    let mut declared: BTreeMap<String, (String, bool)> = BTreeMap::new();
    if let (Some(own), _) = settings(manifest, target) {
        declared.extend(own.iter().map(|(n, v)| (n.clone(), (v.clone(), false))));
    }
    if tests {
        declared.extend(dev_dependencies(manifest, target).into_iter().map(|(n, v)| (n, (v, true))));
    }
    for (name, (version, _)) in &declared {
        versions.insert(name.clone(), version.clone());
    }
    Ok(versions
        .into_iter()
        .map(|(name, version)| {
            let (declared, dev) = match declared.get(&name) {
                Some((v, dev)) => (Some(v.clone()), *dev),
                None => (None, false),
            };
            let requested_by = requests.remove(&name).unwrap_or_default();
            Origin { name, version, declared, dev, requested_by }
        })
        .collect())
}

/// Installs the npm packages that `libraries` need on `target` into `dir/node_modules` and
/// returns that directory, or `None` when no package is needed.
///
/// Node.js resolves bare imports by walking up from the importing module, so
/// any `.mjs` below `dir` sees them. Nothing is installed when `dir` already
/// holds exactly these packages installed by the same package manager.
pub fn install(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf], dir: &Path) -> Result<Option<PathBuf>> {
    let deps = dependencies(ctx, target, libraries)?;
    install_deps(ctx, target, deps, dir)
}

/// [`install`] for the tests, which also get the project's `dev-npm-dependencies`.
pub fn install_for_tests(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf], dir: &Path) -> Result<Option<PathBuf>> {
    let deps = test_dependencies(ctx, target, libraries)?;
    install_deps(ctx, target, deps, dir)
}

fn install_deps(ctx: &BuildContext, target: TargetKind, deps: BTreeMap<String, String>, dir: &Path) -> Result<Option<PathBuf>> {
    if deps.is_empty() {
        return Ok(None);
    }
    let (manager, exe) = find_package_manager(ctx, target, &deps)?;
    install_into(ctx, &deps, dir, manager, exe).map(Some)
}

/// `kargo fetch`: installs the packages [`install`] would, so an offline build finds them. Without
/// npm, pnpm or bun the build could not install them either, so this warns instead of failing.
pub fn fetch(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf], dir: &Path) -> Result<()> {
    fetch_deps(ctx, target, dependencies(ctx, target, libraries)?, dir)
}

/// [`fetch`] for the tests (see [`install_for_tests`]).
pub fn fetch_for_tests(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf], dir: &Path) -> Result<()> {
    fetch_deps(ctx, target, test_dependencies(ctx, target, libraries)?, dir)
}

fn fetch_deps(ctx: &BuildContext, target: TargetKind, deps: BTreeMap<String, String>, dir: &Path) -> Result<()> {
    if deps.is_empty() {
        return Ok(());
    }
    match find_package_manager(ctx, target, &deps) {
        Ok((manager, exe)) => install_into(ctx, &deps, dir, manager, exe).map(|_| ()),
        Err(e) => {
            ctx.shell().warn(format!("the npm packages were not fetched: {e:#}"));
            Ok(())
        }
    }
}

/// Installs the npm packages of a build tool (`deps`, such as `typescript`) into `dir/node_modules`
/// with the package manager of `target`, apart from the packages the program runs with.
pub fn install_tool(ctx: &BuildContext, target: TargetKind, deps: &BTreeMap<String, String>, dir: &Path) -> Result<PathBuf> {
    let (manager, exe) = find_package_manager(ctx, target, deps)?;
    install_into(ctx, deps, dir, manager, exe)
}

/// Where [`declarations`] writes the Kotlin declarations of the npm packages of `target`:
/// `target/<profile>/generated/npm` for Wasm, `generated/npm-js` for Kotlin/JS.
pub fn declarations_dir(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> PathBuf {
    let name = if target == TargetKind::Js { "npm-js" } else { "npm" };
    ws.profile_dir(profile).join("generated").join(name)
}

/// Where the npm packages of `target` are installed: `target/<profile>/node_modules` for Wasm (the
/// Wasm output directory is wiped on every recompile), next to the module for Kotlin/JS.
pub fn install_dir(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> PathBuf {
    match target {
        TargetKind::Js => ws.output_dir(profile, TargetKind::Js),
        _ => ws.profile_dir(profile),
    }
}

/// Whether Kotlin declarations can be generated for `target`: Kotlin/JS has the interop types they
/// use since Kotlin 2.2.20.
pub fn declarations_supported(manifest: &Manifest, target: TargetKind) -> bool {
    match target {
        TargetKind::Wasm => true,
        TargetKind::Js => Version::parse(&manifest.kotlin.version) >= Version::parse("2.2.20"),
        _ => false,
    }
}

/// The opt-in the interop types need since Kotlin 2.2 (`JsAny` and friends are
/// `@ExperimentalWasmJsInterop`, on Kotlin/JS too), so the generated declarations and the code calling them compile
/// without a warning at every use. `None` when the project opts in itself or its Kotlin predates it.
pub fn interop_opt_in(manifest: &Manifest) -> Option<String> {
    const MARKER: &str = "kotlin.js.ExperimentalWasmJsInterop";
    let since = Version::parse(&manifest.kotlin.version) >= Version::parse("2.2.0");
    (since && !manifest.kotlin.opt_in.iter().any(|o| o == MARKER)).then(|| format!("-opt-in={MARKER}"))
}

/// Kotlin declarations for the project's own npm packages of `target` (`[target.wasm.npm-dependencies]`
/// or `[target.js.npm-dependencies]`), generated from their TypeScript declarations (see
/// [`crate::dts`]) into [`declarations_dir`]. Installs the packages first (as the build would after
/// compiling), and `@types/<name>` for each package without declarations of its own. Returns the
/// Kotlin files: none without npm dependencies.
///
/// The files are written again only when the installed packages change.
pub fn declarations(ctx: &BuildContext, target: TargetKind, libraries: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let out = declarations_dir(ctx.ws, &ctx.profile, target);
    let packages: Vec<String> = settings(ctx.ws.manifest(), target).0.map(|deps| deps.keys().cloned().collect()).unwrap_or_default();
    if packages.is_empty() {
        remove_dir_all_if_exists(&out)?;
        return Ok(Vec::new());
    }
    if !declarations_supported(ctx.ws.manifest(), target) {
        ctx.shell().warn(format!(
            "the npm packages get no Kotlin declarations on the {target} target before Kotlin 2.2.20; set [kotlin] version to 2.2.20 or later, or declare what you use with `@JsModule` external declarations"
        ));
        remove_dir_all_if_exists(&out)?;
        return Ok(Vec::new());
    }
    let profile_dir = ctx.ws.profile_dir(&ctx.profile);
    let Some(node_modules) = install(ctx, target, libraries, &install_dir(ctx.ws, &ctx.profile, target))? else { return Ok(Vec::new()) };
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
        let types_dir = profile_dir.join(if target == TargetKind::Js { "npm-types-js" } else { "npm-types" });
        let fetched =
            find_package_manager(ctx, target, &types).and_then(|(manager, exe)| install_into(ctx, &types, &types_dir, manager, exe));
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
    // Start clean: package managers don't share `node_modules` layouts or lockfiles. The manager's own
    // lockfile stays, so the packages it installed before keep their versions.
    remove_dir_all_if_exists(&node_modules)?;
    for (lockfile, owner) in LOCKFILES {
        if owner != manager {
            let _ = std::fs::remove_file(dir.join(lockfile));
        }
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

/// The package manager configured for `target`, or else the first of npm, pnpm and bun on `PATH`.
fn find_package_manager(ctx: &BuildContext, target: TargetKind, deps: &BTreeMap<String, String>) -> Result<(PackageManager, PathBuf)> {
    if let (_, Some(manager)) = settings(ctx.ws.manifest(), target) {
        let exe = find_on_path(manager.as_str()).with_context(|| {
            format!(
                "[target.{target}] package-manager is `{manager}`, but `{manager}` is not on PATH; install it or choose npm, pnpm or bun"
            )
        })?;
        return Ok((manager, exe));
    }
    PackageManager::ALL.into_iter().find_map(|m| find_on_path(m.as_str()).map(|exe| (m, exe))).with_context(|| {
        format!(
            "your {} dependencies need the npm packages {}; install Node.js (it includes npm), pnpm or bun and put it on PATH",
            if target == TargetKind::Js { "Kotlin/JS" } else { "Wasm" },
            package_list(deps)
        )
    })
}

/// The packages as `name@version`, for messages.
fn package_list(deps: &BTreeMap<String, String>) -> String {
    deps.iter().map(|(name, version)| format!("{name}@{version}")).collect::<Vec<_>>().join(", ")
}

/// The arguments that install `package.json` quietly, without lifecycle scripts and without touching
/// any parent workspace.
fn install_args(manager: PackageManager, offline: bool) -> Vec<&'static str> {
    let mut args = match manager {
        PackageManager::Npm => vec!["install", "--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=error"],
        // pnpm defaults to a frozen lockfile in CI, and the dependencies here change with the libraries.
        PackageManager::Pnpm => vec!["install", "--ignore-scripts", "--ignore-workspace", "--no-frozen-lockfile"],
        PackageManager::Bun => vec!["install", "--ignore-scripts", "--no-progress"],
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
        "description": "Generated by Kargo: the npm packages the project's Kotlin libraries depend on.",
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
    fn origins_tell_the_libraries_from_the_manifest() {
        use kargo_maven::Resolution;
        use kargo_maven::resolver::ResolvedNode;
        let dir = tempfile::tempdir().unwrap();
        let klib = |name: &str, json: &str| {
            let path = dir.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("package.json"), json).unwrap();
            path
        };
        let node = |module: &str, version: &str, files: Vec<PathBuf>| ResolvedNode {
            module: module.parse().unwrap(),
            version: version.into(),
            files,
            dependencies: vec![],
        };
        let resolution = Resolution {
            roots: vec![],
            nodes: vec![
                node("org.acme:datetime", "0.7.1", vec![klib("a", r#"{"dependencies":{"@js-joda/core":"3.2.0","left-pad":"1.0.0"}}"#)]),
                node("org.acme:other", "1.0", vec![klib("b", r#"{"dependencies":{"@js-joda/core":"3.1.0"}}"#)]),
            ],
        };
        let manifest = Manifest::parse(
            "[package]\nname = \"web\"\nversion = \"0.1.0\"\n\n[target.wasm]\n\n[target.wasm.npm-dependencies]\nleft-pad = \"^1.3.0\"\n\n[target.wasm.dev-npm-dependencies]\nmocha = \"^10.0.0\"\n",
        )
        .unwrap();
        let main = origins(&manifest, TargetKind::Wasm, &resolution, false).unwrap();
        let summary: Vec<(&str, &str, Option<&str>, usize)> =
            main.iter().map(|o| (o.name.as_str(), o.version.as_str(), o.declared.as_deref(), o.requested_by.len())).collect();
        assert_eq!(summary, [("@js-joda/core", "3.2.0", None, 2), ("left-pad", "^1.3.0", Some("^1.3.0"), 1)]);
        assert_eq!(
            main[0].requested_by[1],
            LibraryRequest { module: "org.acme:other".into(), version: "1.0".into(), spec: "3.1.0".into() }
        );
        let test = origins(&manifest, TargetKind::Wasm, &resolution, true).unwrap();
        let mocha = test.iter().find(|o| o.name == "mocha").unwrap();
        assert!(mocha.dev && mocha.declared.as_deref() == Some("^10.0.0") && mocha.requested_by.is_empty());
        assert!(origins(&manifest, TargetKind::Js, &resolution, false).unwrap().iter().all(|o| o.declared.is_none()));
    }

    #[test]
    fn install_arguments() {
        assert_eq!(
            install_args(PackageManager::Npm, false),
            ["install", "--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=error"]
        );
        assert_eq!(
            install_args(PackageManager::Pnpm, true),
            ["install", "--ignore-scripts", "--ignore-workspace", "--no-frozen-lockfile", "--offline"]
        );
        assert_eq!(install_args(PackageManager::Bun, true), ["install", "--ignore-scripts", "--no-progress", "--offline"]);
        for manager in PackageManager::ALL {
            assert!(install_args(manager, false).contains(&"--ignore-scripts"), "{manager} runs no lifecycle scripts");
        }
    }
}
