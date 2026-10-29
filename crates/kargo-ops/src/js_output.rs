//! What surrounds a compiled Kotlin/JS module: the HTML page of a browser app, the
//! `package.json` of a library, and the runner and page of the tests (see `js_test_runner.mjs`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::ops::build::JsBuild;
use crate::ops::context::BuildContext;
use kargo_manifest::{JsEnvironment, JsKind, JsModuleKind, TargetKind};

/// The file the tests run with, next to the test module.
pub const TEST_RUNNER: &str = "kargo-test-runner.mjs";

/// Everything a finished Kotlin/JS build adds around the compiled module: the page of a browser app
/// and the `package.json` of a library.
pub fn finish(ctx: &BuildContext, build: &JsBuild) -> Result<()> {
    if build.kind == JsKind::Library {
        let manifest = ctx.ws.manifest();
        let js = manifest.targets.js.as_ref().context("the JS target is not enabled")?;
        let dependencies = crate::npm::dependencies(ctx, TargetKind::Js, &build.libraries)?;
        let json = package_json(&manifest.package, js.npm_package_name(&manifest.package), build, &dependencies);
        std::fs::write(build.dir.join("package.json"), json).context("failed to write package.json")?;
    } else {
        remove_library_package_json(&build.dir)?;
    }
    if build.environment == JsEnvironment::Browser && build.kind == JsKind::Application {
        assemble_site(ctx, build)?;
        if ctx.ws.profile(&ctx.profile)?.optimize {
            report_size(ctx, build);
        }
    } else {
        // The page of an earlier build, which linking leaves alone (unlike on Wasm, the directory holds the npm packages).
        let _ = crate::wasm_output::copy_site_files(&build.dir, None, &[]);
        let _ = std::fs::remove_file(build.dir.join("index.html"));
    }
    Ok(())
}

/// Removes the `package.json` an earlier library build wrote into `dir`. The one `npm install` reads,
/// which Kargo writes there too, is private and stays.
fn remove_library_package_json(dir: &Path) -> Result<()> {
    let path = dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(()) };
    let private = serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v["private"] == true);
    if !private {
        std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

/// Copies `resources/` next to the module and writes `index.html` (unless `resources/index.html` is
/// the page), with an import map for the npm packages that are installed.
fn assemble_site(ctx: &BuildContext, build: &JsBuild) -> Result<()> {
    let own_page = crate::wasm_output::copy_site_files(&build.dir, Some(&ctx.ws.resources()), &[])?;
    let index = build.dir.join("index.html");
    let html = if own_page {
        std::fs::read_to_string(&index)?
    } else {
        let manifest = ctx.ws.manifest();
        let compose_html = manifest.frameworks.compose.as_ref().is_some_and(|c| c.html == Some(true));
        default_page(&manifest.package.name, &build.name, build.module_kind, compose_html)
    };
    // A UMD module loads with a plain `<script>`, which cannot use an import map.
    let imports = match (&build.node_modules, build.module_kind) {
        (Some(node_modules), JsModuleKind::Es) => crate::wasm_output::import_map(node_modules),
        _ => BTreeMap::new(),
    };
    std::fs::write(&index, crate::wasm_output::inject_import_map(&html, &imports)).context("failed to write index.html")?;
    Ok(())
}

/// The element Compose HTML's `renderComposable(rootElementId = "root")` renders into.
pub const COMPOSE_ROOT_ID: &str = "root";

/// The page `kargo run` serves when the project has no `resources/index.html`. With Compose HTML it
/// holds the root element the app renders into instead of the note about the console.
fn default_page(package: &str, module: &str, kind: JsModuleKind, compose_html: bool) -> String {
    let script = match kind {
        JsModuleKind::Es => format!("<script type=\"module\" src=\"./{module}.mjs\"></script>"),
        JsModuleKind::CommonJs | JsModuleKind::Umd => format!("<script src=\"./{module}.js\"></script>"),
    };
    let content = if compose_html {
        format!("<div id=\"{COMPOSE_ROOT_ID}\"></div>")
    } else {
        format!("<p>{package} is running. What it prints with <code>println</code> appears in the browser's developer console.</p>")
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{package}</title>
</head>
<body>
  {content}
  {script}
</body>
</html>
"#
    )
}

/// Prints how big the release module and the whole output are, like the Wasm build does.
fn report_size(ctx: &BuildContext, build: &JsBuild) {
    use crate::wasm_output::{brotli, deliverables, gzipped_size, human_size};
    let Ok(module) = std::fs::read(&build.module) else { return };
    let (raw, gzip) = (module.len() as u64, gzipped_size(&module));
    let brotli = brotli(&module).map(|b| b.len() as u64).unwrap_or(0);
    let files = deliverables(&build.dir);
    let total: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
    let shown = build.dir.strip_prefix(ctx.ws.root()).unwrap_or(&build.dir);
    let file = build.module.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ctx.shell().status(
        "Size",
        format!(
            "{file} {} ({} gzip, {} brotli), {} files {} in {}",
            human_size(raw),
            human_size(gzip),
            human_size(brotli),
            files.len(),
            human_size(total),
            shown.display()
        ),
    );
    if kargo_compiler::diagnostics::is_json() {
        kargo_compiler::diagnostics::emit(&serde_json::json!({
            "reason": "js-size", "target": "js", "path": build.module, "bytes": raw, "gzip_bytes": gzip, "brotli_bytes": brotli
        }));
    }
}

/// Writes the test runner next to the test module `module` (an ES module, `<module>.mjs`) in `dir`,
/// and for a browser the page that runs them (see [`crate::wasm_output::reporting_page`]).
pub fn write_test_files(dir: &Path, module: &str, browser: bool, filter: Option<&str>, imports: &BTreeMap<String, String>) -> Result<()> {
    std::fs::write(dir.join(TEST_RUNNER), include_str!("js_test_runner.mjs")).context("failed to write the test runner")?;
    if browser {
        let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
        let run = format!(
            "const {{ run }} = await import({});\n      await run(() => import({}), {{ filter: {} }});",
            quoted(&format!("./{TEST_RUNNER}")),
            quoted(&format!("./{module}.mjs")),
            quoted(filter.unwrap_or_default())
        );
        let page = crate::wasm_output::reporting_page("Kotlin/JS", &run, imports);
        std::fs::write(dir.join("index.html"), page).context("failed to write the test page")?;
    }
    Ok(())
}

/// The `package.json` of a library: the compiled module, its TypeScript declarations and the npm
/// packages it imports, so `npm publish target/release/js` works as it is.
///
/// Written by hand because the order matters: TypeScript takes the first condition of `exports`
/// that matches, so `types` has to come before `default`.
fn package_json(package: &kargo_manifest::Package, npm_name: String, build: &JsBuild, dependencies: &BTreeMap<String, String>) -> String {
    let quoted = |s: &str| serde_json::to_string(s).expect("a string serializes");
    let nested = |value: serde_json::Value| serde_json::to_string_pretty(&value).expect("a JSON value serializes").replace('\n', "\n  ");
    let module = &build.name;
    let (extension, types_extension) = match build.module_kind {
        JsModuleKind::Es => ("mjs", "d.mts"),
        JsModuleKind::CommonJs | JsModuleKind::Umd => ("js", "d.ts"),
    };
    let entry = quoted(&format!("./{module}.{extension}"));
    let types = quoted(&format!("./{module}.{types_extension}"));

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
    let module_type = if build.module_kind == JsModuleKind::Es { "module" } else { "commonjs" };
    fields.extend([("type", quoted(module_type)), ("main", entry.clone())]);
    if build.module_kind == JsModuleKind::Es {
        fields.push(("module", entry.clone()));
    }
    fields.extend([
        ("types", types.clone()),
        (
            "exports",
            format!("{{\n    \".\": {{\n      \"types\": {types},\n      \"default\": {entry}\n    }},\n    \"./package.json\": \"./package.json\"\n  }}"),
        ),
        ("files", nested(serde_json::json!([format!("*.{extension}"), format!("*.{extension}.map"), format!("*.{types_extension}")]))),
    ]);
    if !dependencies.is_empty() {
        fields.push(("dependencies", nested(serde_json::json!(dependencies))));
    }
    let body: Vec<String> = fields.iter().map(|(key, value)| format!("  \"{key}\": {value}")).collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(module_kind: JsModuleKind) -> JsBuild {
        JsBuild {
            module: "out/greeter.mjs".into(),
            klib: "work/greeter.klib".into(),
            environment: JsEnvironment::Node,
            kind: JsKind::Library,
            module_kind,
            name: "greeter".into(),
            dir: "out".into(),
            libraries: vec![],
            node_modules: None,
        }
    }

    #[test]
    fn library_package_json() {
        let manifest = kargo_manifest::Manifest::parse(
            "[package]\nname = \"greeter\"\nversion = \"1.2.0\"\ndescription = \"Says hello\"\nlicense = \"MIT\"\n[target.js]\nkind = \"library\"\n",
        )
        .unwrap();
        let deps = BTreeMap::from([("left-pad".to_owned(), "^1.3.0".to_owned())]);
        let json = package_json(&manifest.package, "@acme/greeter".into(), &build(JsModuleKind::Es), &deps);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "@acme/greeter");
        assert_eq!(value["type"], "module");
        assert_eq!(value["main"], "./greeter.mjs");
        assert_eq!(value["types"], "./greeter.d.mts");
        assert_eq!(value["dependencies"]["left-pad"], "^1.3.0");
        assert!(json.find("\"types\": \"./greeter.d.mts\",\n      \"default\"").is_some(), "types comes first:\n{json}");

        let cjs: serde_json::Value =
            serde_json::from_str(&package_json(&manifest.package, "greeter".into(), &build(JsModuleKind::CommonJs), &BTreeMap::new()))
                .unwrap();
        assert_eq!(
            (cjs["type"].as_str(), cjs["main"].as_str(), cjs["types"].as_str()),
            (Some("commonjs"), Some("./greeter.js"), Some("./greeter.d.ts"))
        );
        assert!(cjs.get("module").is_none() && cjs.get("dependencies").is_none());
        assert_eq!(cjs["files"], serde_json::json!(["*.js", "*.js.map", "*.d.ts"]));
    }

    #[test]
    fn forgets_the_package_json_of_an_earlier_library_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, "{\"private\": true, \"dependencies\": {}}").unwrap();
        remove_library_package_json(dir.path()).unwrap();
        assert!(path.is_file(), "what npm installs from stays");
        std::fs::write(&path, "{\"name\": \"greeter\", \"type\": \"module\"}").unwrap();
        remove_library_package_json(dir.path()).unwrap();
        assert!(!path.exists());
        remove_library_package_json(dir.path()).unwrap();
    }

    #[test]
    fn pages_load_the_module() {
        assert!(default_page("app", "app", JsModuleKind::Es, false).contains("<script type=\"module\" src=\"./app.mjs\"></script>"));
        assert!(default_page("app", "app", JsModuleKind::Umd, false).contains("<script src=\"./app.js\"></script>"));
        assert!(!default_page("app", "app", JsModuleKind::Es, false).contains("id=\"root\""));
        let compose = default_page("app", "app", JsModuleKind::Es, true);
        let (root, script) = (compose.find("<div id=\"root\"></div>").unwrap(), compose.find("<script").unwrap());
        assert!(root < script, "the root element comes before the script: {compose}");
        let dir = tempfile::tempdir().unwrap();
        write_test_files(dir.path(), "app-test", true, Some("MathTest"), &BTreeMap::new()).unwrap();
        let page = std::fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(page.contains("await run(() => import(\"./app-test.mjs\"), { filter: \"MathTest\" });"), "{page}");
        assert!(page.contains("Kotlin/JS tests"), "{page}");
        assert!(dir.path().join(TEST_RUNNER).is_file());
        let node = tempfile::tempdir().unwrap();
        write_test_files(node.path(), "app-test", false, None, &BTreeMap::new()).unwrap();
        assert!(!node.path().join("index.html").exists(), "Node.js needs no page");
    }
}
