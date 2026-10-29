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
        let dependencies = crate::npm_package::published_dependencies(ctx.project, ctx.ws, dependencies);
        let json =
            package_json(&manifest.package, js.npm_package_name(&manifest.package), build, &dependencies, manifest.publish.npm.as_ref());
        std::fs::write(build.dir.join("package.json"), json).context("failed to write package.json")?;
    } else {
        remove_library_package_json(&build.dir)?;
    }
    if build.environment == JsEnvironment::Browser && build.kind == JsKind::Application {
        assemble_site(ctx, build)?;
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
        default_page(&ctx.ws.manifest().package.name, &build.name, build.module_kind)
    };
    // A UMD module loads with a plain `<script>`, which cannot use an import map.
    let imports = match (&build.node_modules, build.module_kind) {
        (Some(node_modules), JsModuleKind::Es) => crate::wasm_output::import_map(node_modules),
        _ => BTreeMap::new(),
    };
    std::fs::write(&index, crate::wasm_output::inject_import_map(&html, &imports)).context("failed to write index.html")?;
    Ok(())
}

/// The page `kargo run` serves when the project has no `resources/index.html`.
fn default_page(package: &str, module: &str, kind: JsModuleKind) -> String {
    let script = match kind {
        JsModuleKind::Es => format!("<script type=\"module\" src=\"./{module}.mjs\"></script>"),
        JsModuleKind::CommonJs | JsModuleKind::Umd => format!("<script src=\"./{module}.js\"></script>"),
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
  <p>{package} is running. What it prints with <code>println</code> appears in the browser's developer console.</p>
  {script}
</body>
</html>
"#
    )
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
/// packages it imports, so `npm publish target/release/js` works as it is (`kargo publish --npm`
/// does that with the checks and credentials of `[publish.npm]`).
fn package_json(
    package: &kargo_manifest::Package,
    npm_name: String,
    build: &JsBuild,
    dependencies: &BTreeMap<String, String>,
    settings: Option<&kargo_manifest::NpmPublishSettings>,
) -> String {
    let (extension, types_extension) = match build.module_kind {
        JsModuleKind::Es => ("mjs", "d.mts"),
        JsModuleKind::CommonJs | JsModuleKind::Umd => ("js", "d.ts"),
    };
    crate::npm_package::LibraryPackage {
        name: crate::npm_package::scoped_name(npm_name, settings),
        package,
        settings,
        module: &build.name,
        extension,
        types_extension,
        es: build.module_kind == JsModuleKind::Es,
        files: vec![format!("*.{extension}"), format!("*.{extension}.map"), format!("*.{types_extension}")],
        dependencies,
    }
    .json()
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
        let json = package_json(&manifest.package, "@acme/greeter".into(), &build(JsModuleKind::Es), &deps, None);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "@acme/greeter");
        assert_eq!(value["type"], "module");
        assert_eq!(value["main"], "./greeter.mjs");
        assert_eq!(value["types"], "./greeter.d.mts");
        assert_eq!(value["dependencies"]["left-pad"], "^1.3.0");
        assert!(json.find("\"types\": \"./greeter.d.mts\",\n      \"import\"").is_some(), "types comes first:\n{json}");

        let cjs: serde_json::Value = serde_json::from_str(&package_json(
            &manifest.package,
            "greeter".into(),
            &build(JsModuleKind::CommonJs),
            &BTreeMap::new(),
            None,
        ))
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
        assert!(default_page("app", "app", JsModuleKind::Es).contains("<script type=\"module\" src=\"./app.mjs\"></script>"));
        assert!(default_page("app", "app", JsModuleKind::Umd).contains("<script src=\"./app.js\"></script>"));
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
