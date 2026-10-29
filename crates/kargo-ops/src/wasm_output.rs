//! What surrounds a compiled Kotlin/Wasm module: the HTML page of a browser
//! app and the page that runs its tests, the import map that lets a page find
//! npm packages, the `package.json` of a library, `wasm-opt` for release
//! builds and the size report.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::ops::build::WasmBuild;
use crate::ops::context::BuildContext;
use kargo_manifest::{WasmCompression, WasmEnvironment, WasmKind, WasmOpt, WasmOptLevel};
use kargo_util::process::{self, find_on_path};

/// Records which files were copied from `resources/`, so a deleted one does not linger.
const RESOURCES_MARKER: &str = ".kargo-resources";
/// Records what `wasm-opt` already did to the module, so an unchanged build is not optimized again.
const OPTIMIZED_MARKER: &str = ".kargo-wasm-opt";

/// Everything a finished Wasm build adds around the compiled module: the page of a
/// browser app, the `package.json` of a library, and for release builds `wasm-opt`
/// and the size report.
pub fn finish(ctx: &BuildContext, build: &mut WasmBuild, size_report: bool) -> Result<()> {
    if ctx.ws.profile(&ctx.profile)?.optimize {
        optimize(ctx, build, size_report)?;
        if !ctx.ws.manifest().targets.wasm.as_ref().unwrap().debug_info {
            for file in minify_glue(build)? {
                ctx.shell()
                    .verbose("Skipping", format!("the minification of {}: its syntax is not supported by the minifier", file.display()));
            }
            for file in all_files(&build.dir).into_iter().filter(|p| p.extension().is_some_and(|e| e == "map")) {
                let _ = std::fs::remove_file(file);
            }
        }
    }
    if ctx.ws.manifest().targets.wasm.as_ref().is_some_and(|w| w.content_hash) {
        hash_assets(build)?;
    }
    if build.kind == WasmKind::Library {
        let manifest = ctx.ws.manifest();
        let wasm = manifest.targets.wasm.as_ref().context("the Wasm target is not enabled")?;
        let dependencies = crate::npm::dependencies(ctx, kargo_manifest::TargetKind::Wasm, &build.libraries)?;
        let json = package_json(&manifest.package, wasm.npm_package_name(&manifest.package), &build.name, &dependencies);
        std::fs::write(build.dir.join("package.json"), json).context("failed to write package.json")?;
    }
    if build.environment == WasmEnvironment::Browser && build.kind == WasmKind::Application {
        assemble_site(ctx, build)?;
    }
    if ctx.ws.profile(&ctx.profile)?.optimize {
        compress_assets(ctx, build)?;
        report_size(ctx, build, size_report)?;
    }
    Ok(())
}

/// Minifies the `.mjs` and `.js` files of the build and returns the ones it left as they were: the minifier does not
/// parse all the syntax the Kotlin compiler writes (dynamic `import()`), and unminified glue still runs.
fn minify_glue(build: &WasmBuild) -> Result<Vec<PathBuf>> {
    let session = minify_js::Session::new();
    let mut skipped = Vec::new();
    for file in all_files(&build.dir).into_iter().filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("mjs" | "js"))) {
        let source = terminate_imports(&String::from_utf8_lossy(&std::fs::read(&file)?)).into_bytes();
        let mut minified = Vec::new();
        if minify_js::minify(&session, minify_js::TopLevelMode::Module, &source, &mut minified).is_err() {
            skipped.push(file);
            continue;
        }
        std::fs::write(file, minified)?;
    }
    Ok(skipped)
}

/// Adds the semicolon the Kotlin compiler leaves off one-line `import` statements: the minifier's parser does not
/// apply automatic semicolon insertion before a following `let`.
fn terminate_imports(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 16);
    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_end();
        let one_line_import =
            trimmed.starts_with("import ") && trimmed.contains(" from ") && (trimmed.ends_with('\'') || trimmed.ends_with('"'));
        out.push_str(trimmed);
        if one_line_import {
            out.push(';');
        }
        out.push_str(&line[body.len()..]);
    }
    out
}

fn hash_assets(build: &mut WasmBuild) -> Result<()> {
    let old_stem = build.name.clone();
    let wasm_path = build.dir.join(format!("{old_stem}.wasm"));
    let wasm = std::fs::read(&wasm_path)?;
    let mut contents = wasm;
    if let Ok(glue) = std::fs::read(build.dir.join(format!("{old_stem}.mjs"))) {
        contents.push(0);
        contents.extend(glue);
    }
    let hash = &kargo_util::hash::sha256_bytes(&contents)[..12];
    let new_stem = format!("{old_stem}.{hash}");
    let new_wasm = build.dir.join(format!("{new_stem}.wasm"));
    let old_module = build.dir.join(format!("{old_stem}.mjs"));
    let new_module = build.dir.join(format!("{new_stem}.mjs"));
    let old_declarations = build.dir.join(format!("{old_stem}.d.mts"));
    let new_declarations = build.dir.join(format!("{new_stem}.d.mts"));
    if old_module.is_file() {
        let glue = std::fs::read_to_string(&old_module)?.replace(&format!("{old_stem}.wasm"), &format!("{new_stem}.wasm"));
        std::fs::write(&new_module, glue)?;
        std::fs::remove_file(&old_module)?;
    }
    if old_declarations.is_file() {
        std::fs::rename(old_declarations, new_declarations)?;
    }
    std::fs::rename(&wasm_path, &new_wasm)?;
    build.name = new_stem;
    build.module = new_module;
    Ok(())
}

fn compress_assets(ctx: &BuildContext, build: &WasmBuild) -> Result<()> {
    let wasm = ctx.ws.manifest().targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let mut files = all_files(&build.dir)
        .into_iter()
        .filter(|file| matches!(file.extension().and_then(|e| e.to_str()), Some("wasm" | "mjs" | "js")))
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        if !file.is_file() {
            continue;
        }
        let bytes = std::fs::read(&file)?;
        for kind in &wasm.compress {
            let (suffix, compressed) = match kind {
                WasmCompression::Gzip => ("gz", gzip(&bytes)?),
                WasmCompression::Brotli => ("br", brotli(&bytes)?),
            };
            let dest = file.with_file_name(format!("{}.{}", file.file_name().unwrap().to_string_lossy(), suffix));
            std::fs::write(dest, compressed)?;
        }
    }
    Ok(())
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn brotli(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
        encoder.write_all(data)?;
    }
    Ok(out)
}

// --- The page ---------------------------------------------------------------------------------

/// Copies `resources/` and the web resources of the app's libraries next to the module and writes
/// `index.html` (unless `resources/index.html` is the page), with an import map for the npm packages
/// that are installed and the libraries' init scripts.
fn assemble_site(ctx: &BuildContext, build: &WasmBuild) -> Result<()> {
    let dir = &build.dir;
    let mut shipments = crate::web_resources::dependencies(ctx, false)?;
    // The files the `Res` class of a Compose app fetches, under `composeResources/`.
    let compose_resources = crate::ops::build::compose_resources_dir(ctx, kargo_manifest::TargetKind::Wasm);
    if crate::compose::enabled(ctx.ws.manifest()) && compose_resources.is_dir() {
        shipments.push(crate::web_resources::from_dir("the app's composeResources/", &compose_resources));
    }
    let own_page = copy_site_files(dir, Some(&ctx.ws.resources()), &shipments)?;
    let index = dir.join("index.html");
    let html = if own_page {
        std::fs::read_to_string(&index)?
    } else {
        let page =
            Page { package: &ctx.ws.manifest().package.name, module: &build.name, compose: crate::compose::enabled(ctx.ws.manifest()) };
        default_page(&page, &crate::web_resources::init_scripts(&shipments))
    };
    let imports = build.node_modules.as_deref().map(import_map).unwrap_or_default();
    std::fs::write(&index, inject_import_map(&html, &imports)).context("failed to write index.html")?;
    Ok(())
}

/// Copies the files of `resources` and what `shipments` ship into `dir`, after removing what an earlier
/// build copied there, and returns whether `resources` has its own `index.html`. A file of `resources`
/// wins over a library's; a library's file may not replace one the build wrote (the module, the page).
pub(crate) fn copy_site_files(dir: &Path, resources: Option<&Path>, shipments: &[crate::web_resources::Shipment]) -> Result<bool> {
    let marker = dir.join(RESOURCES_MARKER);
    for old in std::fs::read_to_string(&marker).unwrap_or_default().lines() {
        let _ = std::fs::remove_file(dir.join(old));
    }
    // What is left is the build's own output.
    let mut reserved: Vec<String> = all_files(dir).iter().filter_map(|f| f.strip_prefix(dir).ok()).map(process::slash).collect();
    reserved.push("index.html".to_owned());
    let mut copied = Vec::new();
    if let Some(resources) = resources {
        for file in all_files(resources) {
            let relative = file.strip_prefix(resources).unwrap_or(&file);
            let dest = dir.join(relative);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&file, &dest).with_context(|| format!("failed to copy {} to {}", file.display(), dest.display()))?;
            copied.push(process::slash(relative));
        }
    }
    let own_page = copied.iter().any(|c| c == "index.html");
    let files = crate::web_resources::plan(shipments, &copied, &reserved)?;
    copied.extend(crate::web_resources::copy(&files, dir)?);
    std::fs::write(&marker, copied.join("\n"))?;
    Ok(own_page)
}

/// What [`default_page`] is for.
struct Page<'a> {
    package: &'a str,
    module: &'a str,
    /// A Compose app, which fills the window with its canvas (`ComposeViewport(document.body!!)`).
    compose: bool,
}

/// The page `kargo run` serves when the project has no `resources/index.html`. It imports the init
/// scripts of the app's libraries, one after the other, before the app module.
fn default_page(page: &Page, inits: &[String]) -> String {
    let Page { package, module, compose } = *page;
    let script = if inits.is_empty() {
        format!("<script type=\"module\" src=\"./{module}.mjs\"></script>")
    } else {
        let imports: String = inits
            .iter()
            .chain(std::iter::once(&format!("./{module}.mjs")))
            .map(|m| format!("    await import({});\n", serde_json::to_string(m).expect("a string serializes")))
            .collect();
        format!(
            "<script type=\"module\">\n    // Generated by Kargo: the init scripts of the app's libraries, then the app.\n{imports}  </script>"
        )
    };
    let (style, body) = if compose {
        ("\n  <style>html, body { width: 100%; height: 100%; margin: 0; padding: 0; overflow: hidden; }</style>", String::new())
    } else {
        (
            "",
            format!(
                "\n  <p>{package} is running. What it prints with <code>println</code> appears in the browser's developer console.</p>"
            ),
        )
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{package}</title>{style}
</head>
<body>{body}
  {script}
</body>
</html>
"#
    )
}

/// The page that runs the tests and reports their output to `kargo test` (see [`crate::web`]). It
/// imports the `inits` scripts (see [`crate::web_resources`]) first.
pub fn test_page(module: &str, imports: &BTreeMap<String, String>, inits: &[String]) -> String {
    let module = serde_json::to_string(&format!("./{module}.mjs")).expect("a string serializes");
    let inits = serde_json::to_string(inits).expect("strings serialize");
    let run = format!(
        "for (const init of {inits}) await import(init);\n      const tests = await import({module});\n      await tests.startUnitTests();"
    );
    reporting_page("Kotlin/Wasm", &run, imports)
}

/// A page that reports its console output to `kargo test` (see [`crate::web`]) while `run`, the body
/// of an `async` block, runs the tests of `what` (`Kotlin/Wasm`), and then says that they are done.
pub(crate) fn reporting_page(what: &str, run: &str, imports: &BTreeMap<String, String>) -> String {
    let page = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Kargo tests</title>
</head>
<body>
  <script type="module">
    // Generated by Kargo: runs the {what} tests and reports their output to `kargo test`.
    let queue = Promise.resolve();
    const report = (name, body = '') => {{
      queue = queue.then(() => fetch('/__kargo/' + name, {{ method: 'POST', body }})).catch(() => {{}});
      return queue;
    }};
    const show = (value) => (typeof value === 'string' ? value : value instanceof Error ? String(value.stack || value) : String(value));
    for (const [level, channel] of [['log', 'stdout'], ['info', 'stdout'], ['debug', 'stdout'], ['warn', 'stderr'], ['error', 'stderr']]) {{
      const original = console[level].bind(console);
      console[level] = (...args) => {{
        original(...args);
        report(channel, args.map(show).join(' ') + '\n');
      }};
    }}
    addEventListener('error', (event) => report('stderr', 'uncaught error: ' + (event.message || event) + '\n'));
    addEventListener('unhandledrejection', (event) => report('stderr', 'unhandled rejection: ' + show(event.reason) + '\n'));
    // Kargo answers this request when the tests are done: closing the window is how Firefox quits.
    fetch('/__kargo/quit').then(() => window.close(), () => {{}});
    try {{
      {run}
      await report('returned');
    }} catch (error) {{
      await report('failed', show(error));
    }}
  </script>
</body>
</html>
"#
    );
    inject_import_map(&page, imports)
}

/// Adds `<script type="importmap">` to `html` so the page can resolve bare imports of npm packages.
/// A page that has an import map of its own is left alone, as is one that needs none.
pub(crate) fn inject_import_map(html: &str, imports: &BTreeMap<String, String>) -> String {
    if imports.is_empty() || html.contains("importmap") {
        return html.to_owned();
    }
    let json = serde_json::to_string_pretty(&serde_json::json!({ "imports": imports })).expect("a JSON value serializes");
    let script = format!("<script type=\"importmap\">\n{json}\n</script>\n");
    // An import map has to come before the first module script, so the end of `<head>` is the place.
    let lower = html.to_ascii_lowercase();
    let at = lower.find("</head>").or_else(|| lower.find("<script")).unwrap_or(0);
    format!("{}{script}{}", &html[..at], &html[at..])
}

/// Maps each installed npm package to the ES module a browser should load for it:
/// `{"@js-joda/core": "/node_modules/@js-joda/core/dist/js-joda.esm.js", "@js-joda/core/": ...}`.
/// Packages that only ship CommonJS are mapped too, but a browser cannot run them.
pub fn import_map(node_modules: &Path) -> BTreeMap<String, String> {
    let mut imports = BTreeMap::new();
    for name in installed_packages(node_modules) {
        let dir = node_modules.join(&name);
        let package: serde_json::Value =
            std::fs::read_to_string(dir.join("package.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let entry = resolve_extension(&dir, esm_entry(&package).as_deref().unwrap_or("index.js"));
        imports.insert(name.clone(), format!("/node_modules/{name}/{entry}"));
        imports.insert(format!("{name}/"), format!("/node_modules/{name}/"));
        for (sub, target) in subpath_entries(&package) {
            imports.insert(format!("{name}/{sub}"), format!("/node_modules/{name}/{}", resolve_extension(&dir, &target)));
        }
    }
    imports
}

/// The subpaths of a package's `exports` map (`./alg` of `cubing`) and the files a browser loads
/// for them, since a subpath's module is rarely the file of the same name. Patterns are left out.
fn subpath_entries(package: &serde_json::Value) -> Vec<(String, String)> {
    let Some(map) = package.get("exports").and_then(|e| e.as_object()) else { return Vec::new() };
    map.iter()
        .filter_map(|(key, value)| {
            let sub = key.strip_prefix("./").filter(|s| !s.is_empty() && !s.contains('*'))?;
            let target = condition_target(value)?;
            Some((sub.to_owned(), target))
        })
        .collect()
}

/// The names of the packages in `node_modules`, scoped ones as `@scope/name`.
fn installed_packages(node_modules: &Path) -> Vec<String> {
    let dirs = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        names
    };
    let mut packages = Vec::new();
    for name in dirs(node_modules) {
        if name.starts_with('@') {
            packages.extend(dirs(&node_modules.join(&name)).into_iter().map(|inner| format!("{name}/{inner}")));
        } else {
            packages.push(name);
        }
    }
    packages
}

/// The entry point a bundler would pick for a browser: `exports`, else `module`, `browser` or `main`.
fn esm_entry(package: &serde_json::Value) -> Option<String> {
    if let Some(exports) = package.get("exports") {
        let root = match exports.as_object() {
            // Keys starting with `.` are subpaths; without them the object lists conditions for the root.
            Some(map) if map.keys().any(|k| k.starts_with('.')) => map.get("."),
            _ => Some(exports),
        };
        if let Some(entry) = root.and_then(condition_target) {
            return Some(entry);
        }
    }
    ["module", "browser", "main"].iter().find_map(|field| package.get(*field)?.as_str().map(str::to_owned))
}

/// The file an `exports` entry names, preferring the conditions a browser matches.
fn condition_target(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => items.iter().find_map(condition_target),
        serde_json::Value::Object(map) => {
            ["browser", "import", "module", "default"].iter().find_map(|key| condition_target(map.get(*key)?))
        }
        _ => None,
    }
}

/// Adds the extension Node.js would: `dist/index` is `dist/index.js` or `dist/index/index.js`.
fn resolve_extension(dir: &Path, entry: &str) -> String {
    let entry = entry.trim_start_matches("./");
    [entry.to_owned(), format!("{entry}.js"), format!("{entry}.mjs"), format!("{}/index.js", entry.trim_end_matches('/'))]
        .into_iter()
        .find(|candidate| dir.join(candidate).is_file())
        .unwrap_or_else(|| entry.to_owned())
}

// --- The library package ----------------------------------------------------------------------

/// The `package.json` of a library: the compiled module, its TypeScript declarations and the
/// npm packages it imports, so `npm publish target/release/wasm` works as it is.
///
/// Written by hand because the order matters: TypeScript takes the first condition of `exports`
/// that matches, so `types` has to come before `default`.
fn package_json(package: &kargo_manifest::Package, npm_name: String, module: &str, dependencies: &BTreeMap<String, String>) -> String {
    let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
    // A JSON value, indented to sit one level down.
    let nested = |value: serde_json::Value| serde_json::to_string_pretty(&value).expect("a JSON value serializes").replace('\n', "\n  ");
    let entry = quoted(&format!("./{module}.mjs"));
    let types = quoted(&format!("./{module}.d.mts"));

    let mut fields: Vec<(&str, String)> = vec![("name", quoted(&npm_name)), ("version", quoted(&package.version))];
    if let Some(description) = &package.description {
        fields.push(("description", quoted(description)));
    }
    if let Some(license) = &package.license {
        fields.push(("license", quoted(license)));
    }
    if let Some(author) = package.authors.first() {
        fields.push(("author", quoted(author)));
    }
    if package.authors.len() > 1 {
        fields.push(("contributors", nested(serde_json::json!(&package.authors[1..]))));
    }
    fields.extend([
        ("type", quoted("module")),
        ("main", entry.clone()),
        ("module", entry.clone()),
        ("types", types.clone()),
        (
            "exports",
            format!("{{\n    \".\": {{\n      \"types\": {types},\n      \"default\": {entry}\n    }},\n    \"./package.json\": \"./package.json\"\n  }}"),
        ),
        ("files", nested(serde_json::json!(["*.mjs", "*.wasm", "*.d.mts"]))),
    ]);
    if !dependencies.is_empty() {
        fields.push(("dependencies", nested(serde_json::json!(dependencies))));
    }
    let body: Vec<String> = fields.iter().map(|(key, value)| format!("  \"{key}\": {value}")).collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

// --- wasm-opt ---------------------------------------------------------------------------------

/// The proposals the Kotlin/Wasm backend emits, plus `-O3` in closed-world mode (Kotlin modules
/// export only what they say, so Binaryen may remove everything else).
const WASM_OPT_ARGS: [&str; 8] = [
    "--enable-gc",
    "--enable-reference-types",
    "--enable-exception-handling",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--closed-world",
];

/// Runs Binaryen's `wasm-opt` over the module when there is one to run.
fn optimize(ctx: &BuildContext, build: &WasmBuild, size_report: bool) -> Result<()> {
    let wasm = ctx.ws.manifest().targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let target = wasm;
    let exe = match &wasm.wasm_opt {
        WasmOpt::Off => return Ok(()),
        WasmOpt::Path(path) => {
            let exe = ctx.ws.root().join(path);
            if !exe.is_file() {
                bail!("[target.wasm] wasm-opt is `{}`, but there is no such file; fix the path or set wasm-opt = false", path.display());
            }
            exe
        }
        WasmOpt::Auto => match find_on_path("wasm-opt") {
            Some(exe) => exe,
            None => kargo_toolchain::binaryen::ensure(ctx.config)?,
        },
    };
    let file = build.dir.join(format!("{}.wasm", build.name));
    let marker = build.dir.join(OPTIMIZED_MARKER);
    let level = match target.opt_level {
        WasmOptLevel::Size => "-Oz",
        WasmOptLevel::Balanced => "-O2",
        WasmOptLevel::Speed => "-O3",
    };
    let mut flags = WASM_OPT_ARGS.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    flags.extend(target.wasm_opt_flags.iter().cloned());
    let key = format!("{}\n{}\n{}\n{}\n{}", exe.display(), flags.join(" "), level, target.debug_info, size_report || target.size_report);
    if std::fs::read_to_string(&marker).is_ok_and(|done| done == key) {
        return Ok(());
    }
    let before = std::fs::metadata(&file).with_context(|| format!("failed to read {}", file.display()))?.len();
    let optimized = build.dir.join(format!("{}.wasm.opt", build.name));
    ctx.shell().status("Optimizing", format!("{}.wasm with wasm-opt", build.name));
    let mut cmd = Command::new(&exe);
    cmd.args(&flags).arg(level);
    if size_report || target.size_report {
        cmd.arg("--metrics").arg("--func-metrics");
    }
    if !target.debug_info {
        cmd.arg("--strip-debug");
    }
    if target.debug_info {
        cmd.arg(format!("--output-source-map={}", build.dir.join(format!("{}.wasm.map", build.name)).display()));
        cmd.arg(format!("--output-source-map-url={}.wasm.map", build.name));
    }
    cmd.arg("-o").arg(&optimized).arg(&file);
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().with_context(|| format!("failed to start {}", exe.display()))?;
    if size_report || target.size_report {
        let metrics = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if !metrics.trim().is_empty() {
            ctx.shell().status("Wasm metrics", metrics.trim());
        }
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(&optimized);
        let log = format!("{}\n{}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
        bail!(
            "`wasm-opt` failed with {}; set [target.wasm] wasm-opt = false to skip it\n\n{}",
            process::describe_status(output.status),
            log.trim()
        );
    }
    std::fs::rename(&optimized, &file).with_context(|| format!("failed to replace {}", file.display()))?;
    std::fs::write(marker, key)?;
    let after = std::fs::metadata(&file)?.len();
    ctx.shell().status("Optimized", format!("{}.wasm: {} -> {}", build.name, human_size(before), human_size(after)));
    Ok(())
}

// --- Size report ------------------------------------------------------------------------------

/// Prints how big the release module and the whole output are.
fn report_size(ctx: &BuildContext, build: &WasmBuild, _size_report: bool) -> Result<()> {
    let file = build.dir.join(format!("{}.wasm", build.name));
    let Ok(wasm) = std::fs::read(&file) else { return Ok(()) };
    let gz = gzipped_size(&wasm);
    let br = brotli(&wasm)?.len() as u64;
    let options = ctx.ws.manifest().targets.wasm.as_ref().context("the Wasm target is not enabled")?;
    let raw = wasm.len() as u64;
    let files = deliverables(&build.dir);
    let total: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
    let shown = build.dir.strip_prefix(ctx.ws.root()).unwrap_or(&build.dir);
    ctx.shell().status(
        "Size",
        format!(
            "{}.wasm {} ({} gzip, {} brotli), {} files {} in {}",
            build.name,
            human_size(raw),
            human_size(gz),
            human_size(br),
            files.len(),
            human_size(total),
            shown.display()
        ),
    );
    if kargo_compiler::diagnostics::is_json() {
        kargo_compiler::diagnostics::emit(&serde_json::json!({
            "reason": "wasm-size", "target": "wasm", "path": file, "bytes": raw, "gzip_bytes": gz, "brotli_bytes": br,
            "max_size": options.max_size, "max_gzip_size": options.max_gzip_size
        }));
    }
    if let Some(limit) = options.max_size
        && raw > limit
    {
        bail!("Wasm size budget exceeded: {} is larger than max-size {}", human_size(raw), human_size(limit));
    }
    if let Some(limit) = options.max_gzip_size
        && gz > limit
    {
        bail!("gzipped Wasm size budget exceeded: {} is larger than max-gzip-size {}", human_size(gz), human_size(limit));
    }
    Ok(())
}

/// Every file below `dir`, sorted.
pub(crate) fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The files of a build that get published or served: everything but the klib, the tests and bookkeeping.
fn deliverables(dir: &Path) -> Vec<PathBuf> {
    all_files(dir)
        .into_iter()
        .filter(|file| {
            let relative = file.strip_prefix(dir).unwrap_or(file);
            let mut parts = relative.components().map(|c| c.as_os_str().to_string_lossy());
            let first = parts.next().unwrap_or_default();
            !matches!(first.as_ref(), "klib" | "test" | "node_modules")
                && !relative.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        })
        .collect()
}

fn gzipped_size(data: &[u8]) -> u64 {
    struct Counter(u64);
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len() as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoder = flate2::write::GzEncoder::new(Counter(0), flate2::Compression::best());
    let _ = encoder.write_all(data);
    encoder.finish().map(|c| c.0).unwrap_or(0)
}

/// `1.5 MiB`, `27.1 KiB` or `512 B`.
fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= KIB * KIB {
        format!("{:.1} MiB", bytes_f / (KIB * KIB))
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn package(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn picks_the_browser_entry_of_a_package() {
        assert_eq!(esm_entry(&package(r#"{"main":"dist/a.js","module":"dist/a.esm.js"}"#)).as_deref(), Some("dist/a.esm.js"));
        assert_eq!(esm_entry(&package(r#"{"main":"index.js"}"#)).as_deref(), Some("index.js"));
        assert_eq!(esm_entry(&package(r#"{"main":"x.js","browser":"b.js"}"#)).as_deref(), Some("b.js"));
        assert_eq!(esm_entry(&package(r#"{"exports":"./e.mjs","main":"x.js"}"#)).as_deref(), Some("./e.mjs"));
        assert_eq!(
            esm_entry(&package(r#"{"exports":{".":{"types":"./t.d.ts","require":"./c.cjs","import":"./m.mjs"}},"main":"c.cjs"}"#))
                .as_deref(),
            Some("./m.mjs")
        );
        assert_eq!(esm_entry(&package(r#"{"exports":{"import":{"types":"./t.d.ts","default":"./d.mjs"}}}"#)).as_deref(), Some("./d.mjs"));
        assert_eq!(esm_entry(&package(r#"{"exports":{"./sub":"./s.js"},"main":"m.js"}"#)).as_deref(), Some("m.js"));
        assert_eq!(esm_entry(&package("{}")), None);
    }

    #[test]
    fn maps_installed_packages() {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path();
        let write = |path: &str, contents: &str| {
            let file = modules.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        };
        write("@js-joda/core/package.json", r#"{"main":"dist/js-joda.js","module":"dist/js-joda.esm.js"}"#);
        write("@js-joda/core/dist/js-joda.esm.js", "export {}");
        write("left-pad/package.json", r#"{"main":"lib/index"}"#);
        write("left-pad/lib/index.js", "");
        write(".bin/left-pad", "");
        write(".kargo-package.json", "{}");
        write(
            "cubing/package.json",
            r#"{"exports":{"./alg":{"types":"./dist/alg.d.ts","import":"./dist/alg.js"},"./scramble":"./dist/scramble","./*":"./dist/*.js"}}"#,
        );
        write("cubing/dist/scramble.js", "");

        let imports = import_map(modules);
        assert_eq!(
            imports,
            map(&[
                ("@js-joda/core", "/node_modules/@js-joda/core/dist/js-joda.esm.js"),
                ("@js-joda/core/", "/node_modules/@js-joda/core/"),
                ("cubing", "/node_modules/cubing/index.js"),
                ("cubing/", "/node_modules/cubing/"),
                ("cubing/alg", "/node_modules/cubing/dist/alg.js"),
                ("cubing/scramble", "/node_modules/cubing/dist/scramble.js"),
                ("left-pad", "/node_modules/left-pad/lib/index.js"),
                ("left-pad/", "/node_modules/left-pad/"),
            ])
        );
        assert!(import_map(&modules.join("missing")).is_empty());
    }

    #[test]
    fn import_map_goes_before_the_module_script() {
        let imports = map(&[("left-pad", "/node_modules/left-pad/index.js")]);
        let page = inject_import_map(
            "<html><head><title>x</title></head><body><script type=\"module\" src=\"a.mjs\"></script></body></html>",
            &imports,
        );
        let map_at = page.find("importmap").unwrap();
        assert!(map_at < page.find("a.mjs").unwrap() && map_at > page.find("<title>").unwrap(), "{page}");
        assert!(page.contains("\"left-pad\": \"/node_modules/left-pad/index.js\""), "{page}");

        // Without a head the map still precedes the first script; with no script it opens the page.
        let no_head = inject_import_map("<body><script src=\"a.js\"></script></body>", &imports);
        assert!(no_head.find("importmap").unwrap() < no_head.find("a.js").unwrap());
        assert!(inject_import_map("<p>hi</p>", &imports).starts_with("<script type=\"importmap\">"));

        let own = "<head><script type=\"importmap\">{}</script></head>";
        assert_eq!(inject_import_map(own, &imports), own, "a page with its own import map keeps it");
        assert_eq!(inject_import_map("<head></head>", &BTreeMap::new()), "<head></head>");
    }

    #[test]
    fn generated_pages_load_the_module() {
        let page = default_page(&Page { package: "hello", module: "hello.core", compose: false }, &[]);
        assert!(page.contains("<title>hello</title>") && page.contains("src=\"./hello.core.mjs\""), "{page}");
        assert!(page.contains("hello is running") && !page.contains("<style>"), "{page}");

        // A Compose app's canvas fills the window, with nothing else on the page.
        let compose = default_page(&Page { package: "hello", module: "hello", compose: true }, &[]);
        assert!(compose.contains("margin: 0") && !compose.contains("is running"), "{compose}");

        // Init scripts run one after the other, then the app.
        let page = default_page(
            &Page { package: "hello", module: "hello", compose: false },
            &["./physics.mjs".to_owned(), "./init.mjs".to_owned()],
        );
        let order: Vec<usize> = ["await import(\"./physics.mjs\");", "await import(\"./init.mjs\");", "await import(\"./hello.mjs\");"]
            .iter()
            .map(|s| page.find(s).unwrap_or_else(|| panic!("{s} is missing:\n{page}")))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{page}");
        assert!(!page.contains("src="), "{page}");

        let tests = test_page("hello-test", &map(&[("a", "/node_modules/a/index.js")]), &[]);
        assert!(tests.contains("import(\"./hello-test.mjs\")") && tests.contains("tests.startUnitTests()"), "{tests}");
        let with_init = test_page("hello-test", &BTreeMap::new(), &["./init.mjs".to_owned()]);
        assert!(
            with_init.find("for (const init of [\"./init.mjs\"]) await import(init);").unwrap() < with_init.find("hello-test.mjs").unwrap()
        );
        assert!(
            tests.contains("/__kargo/") && tests.find("importmap").unwrap() < tests.find("<script type=\"module\">").unwrap(),
            "{tests}"
        );
        // The page asks Kargo when to quit, with the path the server answers to.
        assert!(tests.contains(&format!("fetch('{}').then(() => window.close()", crate::web::QUIT_PATH)), "{tests}");
    }

    #[test]
    fn library_package_json() {
        let package = kargo_manifest::Package {
            name: "Hello".to_owned(),
            version: "1.2.3".to_owned(),
            group: None,
            description: Some("Greets".to_owned()),
            authors: vec!["Ada".to_owned(), "Grace".to_owned()],
            license: Some("MIT".to_owned()),
            homepage: None,
            repository: None,
            publish: true,
        };
        let deps = map(&[("@js-joda/core", "3.2.0")]);
        let text = package_json(&package, "@acme/hello".to_owned(), "hello", &deps);
        assert!(text.find("\"types\": \"./hello.d.mts\",\n      \"default\"").is_some(), "`types` comes first in `exports`:\n{text}");
        assert!(text.ends_with("}\n") && text.find("\"name\"").unwrap() < text.find("\"version\"").unwrap(), "{text}");
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["name"], "@acme/hello");
        assert_eq!(json["version"], "1.2.3");
        assert_eq!(json["type"], "module");
        assert_eq!(json["main"], "./hello.mjs");
        assert_eq!(json["types"], "./hello.d.mts");
        assert_eq!(json["exports"]["."]["default"], "./hello.mjs");
        assert_eq!(json["exports"]["."]["types"], "./hello.d.mts");
        assert_eq!(json["dependencies"]["@js-joda/core"], "3.2.0");
        assert_eq!(json["description"], "Greets");
        assert_eq!(json["license"], "MIT");
        assert_eq!(json["author"], "Ada");
        assert_eq!(json["contributors"][0], "Grace");

        let bare = kargo_manifest::Package { description: None, authors: vec![], license: None, ..package };
        let json: serde_json::Value = serde_json::from_str(&package_json(&bare, "hello".to_owned(), "hello", &BTreeMap::new())).unwrap();
        assert!(json.get("dependencies").is_none() && json.get("author").is_none() && json.get("license").is_none());
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(27_879), "27.2 KiB");
        assert_eq!(human_size(1_572_864), "1.5 MiB");
        assert!(gzipped_size(&[0u8; 10_000]) < 100);
    }

    #[test]
    fn hash_renames_module_and_rewrites_the_wasm_reference() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.wasm"), b"wasm bytes").unwrap();
        std::fs::write(dir.path().join("app.mjs"), "fetch('app.wasm')").unwrap();
        let mut build = WasmBuild {
            module: dir.path().join("app.mjs"),
            klib: dir.path().join("app.klib"),
            environment: WasmEnvironment::Js,
            kind: WasmKind::Application,
            name: "app".to_owned(),
            dir: dir.path().to_path_buf(),
            libraries: vec![],
            node_modules: None,
        };
        hash_assets(&mut build).unwrap();
        let mut contents = b"wasm bytes".to_vec();
        contents.push(0);
        contents.extend(b"fetch('app.wasm')");
        assert_eq!(build.name, format!("app.{}", &kargo_util::hash::sha256_bytes(&contents)[..12]));
        assert!(build.dir.join(format!("{}.wasm", build.name)).is_file());
        assert_eq!(std::fs::read_to_string(build.module).unwrap(), format!("fetch('{}.wasm')", build.name));
    }

    #[test]
    fn gzip_and_brotli_round_trip() {
        let data = b"repeated repeated repeated wasm glue";
        let compressed_gz = gzip(data).unwrap();
        let mut gz = flate2::read::GzDecoder::new(compressed_gz.as_slice());
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut gz, &mut decoded).unwrap();
        assert_eq!(decoded, data);
        let compressed = brotli(data).unwrap();
        let mut decoder = brotli::Decompressor::new(compressed.as_slice(), 4096);
        decoded.clear();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn generated_javascript_is_minified() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.mjs"), "export function greet ( name ) { return name + '!' ; }\n").unwrap();
        let build = WasmBuild {
            module: dir.path().join("app.mjs"),
            klib: dir.path().join("app.klib"),
            environment: WasmEnvironment::Js,
            kind: WasmKind::Application,
            name: "app".to_owned(),
            dir: dir.path().to_path_buf(),
            libraries: vec![],
            node_modules: None,
        };
        minify_glue(&build).unwrap();
        let code = std::fs::read_to_string(build.module).unwrap();
        assert!(!code.contains("return name + '!' ;"), "{code}");
    }

    #[test]
    fn imports_without_a_semicolon_are_minified() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.mjs"), "import { a } from './a.mjs'\n\nlet value;\nexport const b = a;\n").unwrap();
        let build = WasmBuild {
            module: dir.path().join("app.mjs"),
            klib: dir.path().join("app.klib"),
            environment: WasmEnvironment::Js,
            kind: WasmKind::Application,
            name: "app".to_owned(),
            dir: dir.path().to_path_buf(),
            libraries: vec![],
            node_modules: None,
        };
        minify_glue(&build).unwrap();
    }

    #[test]
    fn deliverables_skip_the_klib_tests_and_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "app.mjs",
            "app.wasm",
            "index.html",
            "img/logo.png",
            "klib/app.klib",
            "test/app-test.mjs",
            ".kargo-resources",
            ".fingerprint/x",
        ] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "x").unwrap();
        }
        let names: Vec<String> = deliverables(dir.path()).iter().map(|p| process::slash(p.strip_prefix(dir.path()).unwrap())).collect();
        assert_eq!(names, ["app.mjs", "app.wasm", "img/logo.png", "index.html"]);
    }
}
