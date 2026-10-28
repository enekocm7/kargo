//! The package's own JavaScript and TypeScript modules for the Wasm and Kotlin/JS targets: the
//! files of `js/` (or of the `js-sources` directories of `[target.wasm]` / `[target.js]`).
//!
//! They land in `js/` next to the Kotlin module, keeping their paths, so `@JsModule("./js/canvas.js")`
//! resolves the same in Node.js, on the page `kargo run` serves and in `kargo test` ([`install`]).
//! JavaScript and any other file is copied; TypeScript (`.ts`, `.mts`) is compiled with the pinned
//! `tsc` after the Kotlin module is linked, so it type-checks against the module's own `.d.mts`
//! (`import { greet } from "../app.mjs"`). Its errors go through the compiler diagnostics, in
//! `--message-format json` too.
//!
//! The TypeScript modules (and the `.d.ts` files next to JavaScript ones) also get Kotlin
//! declarations ([`declarations`]), with the generator of the npm packages
//! ([`crate::dts::generate_local`]): `js/canvas.ts` is the Kotlin package `js.canvas`. Both targets
//! get the same Kotlin, in a directory of their own. The JavaScript factories of those declarations
//! and of the npm packages' (see [`crate::dts`]) land in `js/kargo-factories/` with the modules.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::dts::{self, FACTORIES_DIR, LocalGenerated, LocalModule};
use crate::ops::context::BuildContext;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::diagnostics::Relay;
use kargo_manifest::TargetKind;
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, find_on_path};

/// The TypeScript compiler that compiles the TypeScript modules.
pub const TYPESCRIPT_VERSION: &str = "5.9.3";

/// The directory next to the Kotlin module that the modules land in.
pub const OUTPUT_DIR: &str = "js";

/// The list of the files Kargo put in [`OUTPUT_DIR`], so that it removes them once their sources
/// are gone and leaves any other file alone.
const INSTALLED: &str = ".kargo-files";

/// The `js-sources` directories of `target` that exist, absolute.
pub fn dirs(ws: &Workspace, target: TargetKind) -> Vec<PathBuf> {
    let manifest = ws.manifest();
    let configured: &[PathBuf] = match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref().map_or(&[], |w| &w.js_sources),
        TargetKind::Js => manifest.targets.js.as_ref().map_or(&[], |j| &j.js_sources),
        _ => &[],
    };
    configured.iter().map(|d| ws.root().join(d)).filter(|d| d.is_dir()).collect()
}

/// A file of the modules: where it is, and its path below [`OUTPUT_DIR`] with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    rel: String,
}

/// Every file in `dirs`, outside `node_modules`. Two directories can't both have a file at one path.
fn sources(dirs: &[PathBuf]) -> Result<Vec<Source>> {
    let mut by_rel: BTreeMap<String, Source> = BTreeMap::new();
    for dir in dirs {
        let walk = walkdir::WalkDir::new(dir).follow_links(true).into_iter().filter_entry(|e| e.file_name() != "node_modules");
        for entry in walk.filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            let path = entry.into_path();
            let Ok(rel) = path.strip_prefix(dir) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if let Some(other) = by_rel.get(&rel) {
                bail!(
                    "`{}` and `{}` would both land at `{OUTPUT_DIR}/{rel}` next to the module; rename one of them",
                    other.path.display(),
                    path.display()
                );
            }
            by_rel.insert(rel.clone(), Source { path, rel });
        }
    }
    Ok(by_rel.into_values().collect())
}

/// A TypeScript declaration file (`.d.ts`, `.d.mts`, `.d.cts`).
fn is_declaration(rel: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|e| rel.ends_with(e))
}

/// A TypeScript module that `tsc` compiles (`.ts`, `.mts`, `.cts`), and the JavaScript file it
/// becomes (`.js`, `.mjs`, `.cjs`).
fn compiled_name(rel: &str) -> Option<String> {
    if is_declaration(rel) {
        return None;
    }
    [(".ts", ".js"), (".mts", ".mjs"), (".cts", ".cjs")].iter().find_map(|(ts, js)| rel.strip_suffix(ts).map(|stem| format!("{stem}{js}")))
}

/// The JavaScript file a declaration file describes (`x.d.ts` is `x.js`).
fn described_name(rel: &str) -> Option<String> {
    [(".d.ts", ".js"), (".d.mts", ".mjs")].iter().find_map(|(d, js)| rel.strip_suffix(d).map(|stem| format!("{stem}{js}")))
}

/// The modules of `target` whose types Kotlin gets: each TypeScript module, and each declaration
/// file of a JavaScript module that no TypeScript module compiles to.
pub fn modules(ws: &Workspace, target: TargetKind) -> Result<Vec<LocalModule>> {
    let sources = sources(&dirs(ws, target))?;
    let compiled: BTreeSet<String> = sources.iter().filter_map(|s| compiled_name(&s.rel)).collect();
    let mut out = Vec::new();
    for source in &sources {
        let js = match compiled_name(&source.rel) {
            // CommonJS modules aren't imported by Kotlin.
            Some(js) if !js.ends_with(".cjs") => js,
            Some(_) => continue,
            None => match described_name(&source.rel) {
                Some(js) if !compiled.contains(&js) => js,
                _ => continue,
            },
        };
        let stem = js.rsplit_once('.').map_or(js.as_str(), |(stem, _)| stem);
        let display = source.path.strip_prefix(ws.root()).unwrap_or(&source.path).to_string_lossy().replace('\\', "/");
        out.push(LocalModule {
            file: source.path.clone(),
            display,
            package: dts::kotlin_package(&format!("{OUTPUT_DIR}/{stem}")),
            spec: format!("./{OUTPUT_DIR}/{js}"),
        });
    }
    Ok(out)
}

/// Where [`declarations`] writes the Kotlin declarations of the modules of `target`:
/// `target/<profile>/generated/js-sources` for Wasm, `generated/js-sources-js` for Kotlin/JS.
pub fn declarations_dir(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> PathBuf {
    let name = if target == TargetKind::Js { "js-sources-js" } else { "js-sources" };
    ws.profile_dir(profile).join("generated").join(name)
}

/// Writes the Kotlin declarations of the modules of `target` into [`declarations_dir`], rewriting
/// only the files that change (see [`dts::generate_local`]). npm packages are read from the
/// `node_modules` the build installs. This is what `kargo lsp` runs when a TypeScript file is saved.
pub fn generate(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> Result<LocalGenerated> {
    let out = declarations_dir(ws, profile, target);
    let modules = if crate::npm::declarations_supported(ws.manifest(), target) { modules(ws, target)? } else { Vec::new() };
    if modules.is_empty() {
        let mut generated = LocalGenerated::default();
        if out.exists() {
            generated.changed.push(out.clone());
            remove_dir_all_if_exists(&out)?;
        }
        return Ok(generated);
    }
    let node_modules = crate::npm::install_dir(ws, profile, target).join("node_modules");
    let roots: Vec<PathBuf> = [node_modules].into_iter().filter(|d| d.is_dir()).collect();
    dts::generate_local(&modules, &roots, &out)
}

/// The Kotlin declarations of the modules of `target`, generated by [`generate`], for the build.
/// Says when they changed, and which exported functions lack types.
pub fn declarations(ctx: &BuildContext, target: TargetKind) -> Result<Vec<PathBuf>> {
    let generated = generate(ctx.ws, &ctx.profile, target)?;
    if !generated.changed.is_empty() && !generated.files.is_empty() {
        let out = declarations_dir(ctx.ws, &ctx.profile, target);
        ctx.shell().verbose("Generated", format!("Kotlin declarations in {}", out.display()));
        // Modules both targets share are reported once, for Wasm.
        let shared =
            target == TargetKind::Js && ctx.ws.manifest().targets.wasm.is_some() && dirs(ctx.ws, TargetKind::Wasm) == dirs(ctx.ws, target);
        for (file, name) in generated.untyped.iter().filter(|_| !shared) {
            ctx.shell().warn(format!(
                "`{name}` in {file} has a parameter or return type without an annotation, so Kotlin sees `JsAny?` there; \
                 write the types of exported functions out"
            ));
        }
    }
    Ok(generated.files)
}

/// Whether `ws` has modules with Kotlin declarations on `target`.
pub fn has_declarations(ws: &Workspace, target: TargetKind) -> bool {
    crate::npm::declarations_supported(ws.manifest(), target) && modules(ws, target).is_ok_and(|m| !m.is_empty())
}

/// Whether `target` has TypeScript modules to compile.
fn has_typescript(sources: &[Source]) -> bool {
    sources.iter().any(|s| compiled_name(&s.rel).is_some())
}

/// Puts the modules of `target` into `out/js/`: copies every file but TypeScript, and compiles
/// TypeScript with `tsc` (only when a module or what it imports changed). `module_dir` holds the
/// Kotlin module the TypeScript may import (`../app.mjs`), whose `.d.mts` it is type-checked
/// against; it is `out` itself, except for tests, which take the main module's compiled files.
/// `node_modules` is where the target's npm packages are installed.
///
/// Files Kargo put there before whose sources are gone are removed; other files are left alone.
pub fn install(ctx: &BuildContext, target: TargetKind, out: &Path, module_dir: &Path, node_modules: Option<&Path>) -> Result<()> {
    let dest = out.join(OUTPUT_DIR);
    let sources = sources(&dirs(ctx.ws, target))?;
    let mut wanted: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for source in sources.iter().filter(|s| compiled_name(&s.rel).is_none() && !is_declaration(&s.rel)) {
        let bytes = std::fs::read(&source.path).with_context(|| format!("failed to read {}", source.path.display()))?;
        wanted.insert(source.rel.clone(), bytes);
    }
    if has_typescript(&sources) {
        let compiled = typescript(ctx, target, &sources, module_dir, node_modules, false)?.join("out").join(OUTPUT_DIR);
        for source in &sources {
            let Some(js) = compiled_name(&source.rel) else { continue };
            for name in [js.clone(), format!("{js}.map")] {
                if let Ok(bytes) = std::fs::read(compiled.join(&name)) {
                    wanted.insert(name, bytes);
                }
            }
        }
    }
    // The factories of the generated declarations, which import them from `./js/kargo-factories/`.
    let generated = [crate::npm::declarations_dir(ctx.ws, &ctx.profile, target), declarations_dir(ctx.ws, &ctx.profile, target)];
    for (rel, path) in generated.iter().flat_map(|dir| dts::factory_modules(dir)) {
        if wanted.contains_key(&rel) {
            bail!(
                "`{rel}` in the JavaScript modules would replace the factories Kargo generates there; rename it or move it out of `{FACTORIES_DIR}/`"
            );
        }
        let bytes = std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        wanted.insert(rel, bytes);
    }
    if !wanted.is_empty() && !wanted.contains_key("package.json") {
        // `tsc` writes ES modules to `.js` files: Node.js reads them as such only with this.
        wanted.insert("package.json".to_owned(), b"{ \"type\": \"module\" }\n".to_vec());
    }
    sync(&dest, &wanted)
}

/// For tests: the modules of `target` that import the Kotlin module (`../app.mjs`) find the test
/// module in `out` under the main module's name, `module` (`app.mjs` re-exporting `app-test.mjs`),
/// since the test module links the main code in. Written only when `target` has modules.
pub fn alias_module(ws: &Workspace, target: TargetKind, out: &Path, module: &str, test_module: &str) -> Result<()> {
    if dirs(ws, target).is_empty() {
        return Ok(());
    }
    let alias = out.join(format!("{module}.mjs"));
    let text = format!(
        "// Written by Kargo: the test module under the main module's name, for the modules in js/.\nexport * from \"./{test_module}.mjs\";\n"
    );
    if std::fs::read_to_string(&alias).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&alias, text).with_context(|| format!("failed to write {}", alias.display()))?;
    }
    Ok(())
}

/// Makes `dest` hold the `wanted` files (by path below it), writing only those that differ, and
/// removes the files of the previous [`sync`] that are no longer wanted.
fn sync(dest: &Path, wanted: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let list = dest.join(INSTALLED);
    let before = std::fs::read_to_string(&list).unwrap_or_default();
    for stale in before.lines().filter(|rel| !rel.is_empty() && !wanted.contains_key(*rel)) {
        let _ = std::fs::remove_file(dest.join(stale));
    }
    if wanted.is_empty() {
        let _ = std::fs::remove_file(&list);
        // Gone if nothing else is left in it.
        let _ = std::fs::remove_dir(dest);
        return Ok(());
    }
    for (rel, bytes) in wanted {
        let path = dest.join(rel);
        if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            std::fs::create_dir_all(path.parent().expect("a file below the output directory"))?;
            std::fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
        }
    }
    let names: Vec<&str> = wanted.keys().map(String::as_str).collect();
    let text = format!("{}\n", names.join("\n"));
    if before != text {
        std::fs::write(&list, text)?;
    }
    Ok(())
}

/// `kargo check`: type-checks the TypeScript modules of `target` without writing them out, against
/// the `.d.mts` of the Kotlin module that `kargo build` linked last. When a module imports the
/// Kotlin module and no build wrote its declarations yet, the check is left to `kargo build`.
pub fn check(ctx: &BuildContext, target: TargetKind, module_name: &str, node_modules: Option<&Path>) -> Result<()> {
    let sources = sources(&dirs(ctx.ws, target))?;
    if !has_typescript(&sources) {
        return Ok(());
    }
    let module_dir = ctx.ws.output_dir(&ctx.profile, target);
    let declarations = ["d.mts", "d.ts"].iter().any(|e| module_dir.join(format!("{module_name}.{e}")).is_file());
    let imports_module = sources
        .iter()
        .filter(|s| compiled_name(&s.rel).is_some())
        .any(|s| std::fs::read_to_string(&s.path).is_ok_and(|text| text.contains(&format!("{module_name}.mjs"))));
    if imports_module && !declarations {
        ctx.shell().verbose(
            "Skipping",
            "the type check of the TypeScript modules: they import the Kotlin module, which `kargo build` has not linked yet",
        );
        return Ok(());
    }
    typescript(ctx, target, &sources, &module_dir, node_modules, true).map(|_| ())
}

/// Compiles (or with `check`, type-checks) the TypeScript modules in a staging directory,
/// `target/<profile>/ts/<target>`, and returns it: `src/js/` mirrors the modules, so that the Kotlin
/// module in `module_dir` is one level up from them as it is at runtime (`rootDirs`), and
/// `out/js/` gets what `tsc` writes. Nothing runs when the inputs are those of the last run.
fn typescript(
    ctx: &BuildContext,
    target: TargetKind,
    sources: &[Source],
    module_dir: &Path,
    node_modules: Option<&Path>,
    check: bool,
) -> Result<PathBuf> {
    let stage = ctx.ws.profile_dir(&ctx.profile).join("ts").join(target.as_str());
    let mirror = stage.join("src");
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for source in sources {
        files.insert(source.rel.clone(), std::fs::read(&source.path).with_context(|| format!("failed to read {}", source.path.display()))?);
    }
    write_mirror(&mirror, &files)?;
    std::fs::write(mirror.join("package.json"), "{ \"type\": \"module\" }\n")?;
    let debug = ctx.ws.profile(&ctx.profile)?.debug;
    let config = tsconfig(&stage, module_dir, node_modules, debug);
    std::fs::write(stage.join("tsconfig.json"), &config)?;

    let mut fp = Fingerprint::new();
    fp.str(env!("CARGO_PKG_VERSION")).str(TYPESCRIPT_VERSION).str(&config).str(if check { "check" } else { "emit" });
    for (rel, bytes) in &files {
        fp.str(rel).bytes(bytes);
    }
    // The declarations of the Kotlin module the modules may import.
    let mut declarations: Vec<PathBuf> = std::fs::read_dir(module_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_declaration(&p.to_string_lossy()))
        .collect();
    declarations.sort();
    for path in declarations {
        fp.str(&path.to_string_lossy()).bytes(&std::fs::read(&path).unwrap_or_default());
    }
    if let Some(node_modules) = node_modules {
        fp.bytes(&std::fs::read(node_modules.join(".kargo-package.json")).unwrap_or_default());
    }
    let stamp_path = stage.join(if check { ".kargo-check-stamp" } else { ".kargo-stamp" });
    let stamp = fp.finish();
    if std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == stamp) {
        return Ok(stage);
    }
    let _ = std::fs::remove_file(&stamp_path);
    if !check {
        remove_dir_all_if_exists(&stage.join("out"))?;
    }

    let tsc = tsc(ctx, target)?;
    let runtime = find_on_path("node")
        .or_else(|| find_on_path("bun"))
        .context("the TypeScript modules are compiled with tsc, which runs on Node.js or bun; install one of them and put it on PATH")?;
    let count = sources.iter().filter(|s| compiled_name(&s.rel).is_some()).count();
    let what = format!("{count} TypeScript module{} (tsc {TYPESCRIPT_VERSION})", if count == 1 { "" } else { "s" });
    ctx.shell().status(if check { "Checking" } else { "Compiling" }, &what);
    let mut cmd = Command::new(runtime);
    cmd.arg(&tsc).args(["--project", "tsconfig.json", "--pretty", "false"]).current_dir(&stage);
    if check {
        cmd.arg("--noEmit");
    }
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().context("failed to start tsc")?;
    let originals: BTreeMap<String, &Path> = sources.iter().map(|s| (s.rel.clone(), s.path.as_path())).collect();
    let mut relay = Relay::new();
    for line in String::from_utf8_lossy(&output.stdout).lines().chain(String::from_utf8_lossy(&output.stderr).lines()) {
        relay.line(&diagnostic_line(line, &stage, &originals));
    }
    relay.flush();
    if !output.status.success() {
        bail!("could not compile the TypeScript modules of the {target} target due to the errors above");
    }
    std::fs::write(&stamp_path, stamp)?;
    Ok(stage)
}

/// Makes `mirror/js/` hold `files`, writing only those that differ (so `tsc` sees the rest unchanged).
fn write_mirror(mirror: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let root = mirror.join(OUTPUT_DIR);
    for existing in walkdir::WalkDir::new(&root).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let rel = existing.path().strip_prefix(&root).unwrap_or(existing.path()).to_string_lossy().replace('\\', "/");
        if !files.contains_key(&rel) {
            let _ = std::fs::remove_file(existing.path());
        }
    }
    for (rel, bytes) in files {
        let path = root.join(rel);
        if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            std::fs::create_dir_all(path.parent().expect("a file below the mirror"))?;
            std::fs::write(&path, bytes)?;
        }
    }
    Ok(())
}

/// The `tsconfig.json` of the staging directory `stage`.
fn tsconfig(stage: &Path, module_dir: &Path, node_modules: Option<&Path>, debug: bool) -> String {
    let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let mut options = serde_json::json!({
        "target": "ES2022",
        "module": "NodeNext",
        "moduleResolution": "NodeNext",
        "lib": ["ES2022", "DOM", "DOM.Iterable"],
        "strict": true,
        "skipLibCheck": true,
        "allowJs": true,
        "checkJs": false,
        "noEmitOnError": true,
        "rootDir": "src",
        "outDir": "out",
        // The Kotlin module is one level up from the modules at runtime: `../app.mjs`.
        "rootDirs": ["src", slash(module_dir)],
    });
    if debug {
        options["sourceMap"] = serde_json::json!(true);
        options["inlineSources"] = serde_json::json!(true);
    }
    if let Some(node_modules) = node_modules.filter(|n| n.parent().is_none_or(|above| !stage.starts_with(above))) {
        // Found by walking up from the modules otherwise.
        let root = slash(node_modules);
        options["paths"] = serde_json::json!({ "*": [format!("{root}/*"), format!("{root}/@types/*")] });
        options["typeRoots"] = serde_json::json!([format!("{root}/@types")]);
    }
    let config = serde_json::json!({
        "compilerOptions": options,
        "include": [format!("src/{OUTPUT_DIR}/**/*")],
    });
    format!("{}\n", serde_json::to_string_pretty(&config).expect("a JSON value serializes"))
}

/// Installs the pinned `typescript` package (in `target/typescript`) and returns its `tsc` script.
fn tsc(ctx: &BuildContext, target: TargetKind) -> Result<PathBuf> {
    let deps = BTreeMap::from([("typescript".to_owned(), TYPESCRIPT_VERSION.to_owned())]);
    let node_modules = crate::npm::install_tool(ctx, target, &deps, &ctx.ws.work_dir("typescript"))
        .context("the TypeScript modules need the TypeScript compiler")?;
    Ok(node_modules.join("typescript").join("bin").join("tsc"))
}

/// A line of `tsc` output with its location (`src/js/a.ts(3,7): error TS2322: ...`, relative to
/// `stage`) turned into the form of the Kotlin compiler's, at the original file:
/// `/p/js/a.ts:3:7: error: ... (TS2322)`. Any other line stays as it is.
fn diagnostic_line(line: &str, stage: &Path, originals: &BTreeMap<String, &Path>) -> String {
    let parsed = (|| {
        let (location, rest) = line.split_once("): ")?;
        let (file, position) = location.rsplit_once('(')?;
        let (row, column) = position.split_once(',')?;
        let (severity, rest) = rest.split_once(' ')?;
        let (code, message) = rest.split_once(": ")?;
        if !matches!(severity, "error" | "warning") || !code.starts_with("TS") {
            return None;
        }
        let rel = file.replace('\\', "/");
        let path = match rel.strip_prefix(&format!("src/{OUTPUT_DIR}/")).and_then(|r| originals.get(r)) {
            Some(original) => original.to_path_buf(),
            None => stage.join(file),
        };
        Some(format!("{}:{row}:{column}: {severity}: {message} ({code})", path.display()))
    })();
    parsed.unwrap_or_else(|| line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names() {
        assert_eq!(compiled_name("canvas.ts").as_deref(), Some("canvas.js"));
        assert_eq!(compiled_name("util/math.mts").as_deref(), Some("util/math.mjs"));
        assert_eq!(compiled_name("types.d.ts"), None);
        assert_eq!(compiled_name("plain.js"), None);
        assert_eq!(described_name("legacy.d.ts").as_deref(), Some("legacy.js"));
        assert_eq!(described_name("legacy.d.mts").as_deref(), Some("legacy.mjs"));
    }

    #[test]
    fn tsc_errors_point_at_the_original_files() {
        let original = Path::new("/p/js/main.ts");
        let originals = BTreeMap::from([("main.ts".to_owned(), original)]);
        let line = diagnostic_line(
            "src/js/main.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.",
            Path::new("/s"),
            &originals,
        );
        assert_eq!(line, format!("{}:3:7: error: Type 'string' is not assignable to type 'number'. (TS2322)", original.display()));
        let diagnostic = kargo_compiler::diagnostics::parse_header(&line).unwrap();
        assert_eq!((diagnostic.severity.as_str(), diagnostic.line, diagnostic.column), ("error", Some(3), Some(7)));
        assert_eq!(diagnostic_line("  Some context.", Path::new("/s"), &originals), "  Some context.");
        assert_eq!(diagnostic_line("error TS5023: Unknown option.", Path::new("/s"), &originals), "error TS5023: Unknown option.");
    }

    #[test]
    fn sync_writes_changes_and_removes_its_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("js");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("mine.txt"), "a file Kargo didn't put there").unwrap();
        let files =
            |names: &[&str]| -> BTreeMap<String, Vec<u8>> { names.iter().map(|n| (n.to_string(), n.as_bytes().to_vec())).collect() };
        sync(&dest, &files(&["a.js", "sub/b.mjs"])).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("sub/b.mjs")).unwrap(), "sub/b.mjs");
        sync(&dest, &files(&["a.js"])).unwrap();
        assert!(!dest.join("sub/b.mjs").exists());
        assert!(dest.join("mine.txt").exists(), "only Kargo's own files are removed");
        sync(&dest, &files(&[])).unwrap();
        assert!(!dest.join("a.js").exists());
    }

    #[test]
    fn tsconfig_finds_the_kotlin_module_and_the_npm_packages() {
        let config: serde_json::Value = serde_json::from_str(&tsconfig(
            Path::new("/p/target/debug/ts/wasm"),
            Path::new("/p/target/debug/wasm"),
            Some(Path::new("/p/target/debug/node_modules")),
            true,
        ))
        .unwrap();
        let options = &config["compilerOptions"];
        assert_eq!(options["rootDirs"], serde_json::json!(["src", "/p/target/debug/wasm"]));
        assert_eq!(options["module"], "NodeNext");
        assert_eq!(options["sourceMap"], true);
        assert!(options.get("paths").is_none(), "the packages are found above the staging directory");
        let config: serde_json::Value = serde_json::from_str(&tsconfig(
            Path::new("/p/target/debug/ts/js"),
            Path::new("/p/target/debug/js"),
            Some(Path::new("/p/target/debug/js/node_modules")),
            false,
        ))
        .unwrap();
        assert_eq!(config["compilerOptions"]["paths"]["*"][0], "/p/target/debug/js/node_modules/*");
    }
}
