//! Importing a Kargo project for a Kotlin language server: `kargo metadata`, turned into the
//! server's own project description.
//!
//! - The JetBrains Kotlin LSP reads `workspace.json` at the project root (its format for build
//!   systems it doesn't know) and reloads it when it changes. Kargo writes one module per
//!   package for main and one for test code, with the JVM target's classpaths as libraries
//!   (with their sources jars), the JDK and the Kotlin compiler settings and plugins.
//! - fwcd's kotlin-language-server runs a `kls-classpath` script at the project root. Kargo
//!   writes one that prints the test classpath it keeps in `target/lsp/classpath.txt`.
//!
//! Generated files are only replaced while they still hold what Kargo last wrote, so a
//! hand-written `workspace.json` is never overwritten.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use kargo_ops::ops::context::BuildContext;
use kargo_ops::ops::metadata::{self, MetadataOptions};
use kargo_ops::workspace::{ProfileKind, Project};
use kargo_toolchain::ServerKind;
use kargo_util::config::Config;

/// The project's `kargo metadata` document.
pub fn metadata(config: &Config, root: &Path, no_deps: bool) -> Result<Value> {
    let project = Project::discover(root, None)?;
    let ctx = BuildContext::new(config, &project, project.members()[0], ProfileKind::Dev)?;
    metadata::metadata(&ctx, &MetadataOptions { no_deps })
}

/// The project's fingerprint (see `kargo metadata --fingerprint`).
pub fn fingerprint(root: &Path) -> Result<String> {
    Ok(metadata::fingerprint(&Project::discover(root, None)?))
}

/// The directory of the project containing `dir`.
pub fn project_root(dir: &Path) -> Option<PathBuf> {
    Project::discover(dir, None).ok().map(|p| p.root().to_path_buf())
}

/// The result of importing a project.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The project's metadata: with its dependencies, or without them if they couldn't be resolved.
    pub metadata: Option<Value>,
    /// Why the import failed or is incomplete.
    pub error: Option<String>,
    pub written: Written,
}

/// Imports the project at `root` and writes the description `kind` reads. When dependencies
/// can't be resolved (offline, a broken `kargo.toml`), the project is described without them.
pub fn import(config: &Config, root: &Path, kind: Option<ServerKind>) -> Outcome {
    let fingerprint = fingerprint(root).ok();
    let (metadata, mut error) = match metadata(config, root, false) {
        Ok(m) => (Some(m), None),
        Err(e) => (metadata(config, root, true).ok(), Some(format!("{e:#}"))),
    };
    let mut written = Written::default();
    if let (Some(kind), Some(m)) = (kind, &metadata) {
        match write(kind, m) {
            Ok(w) => written = w,
            Err(e) => {
                error.get_or_insert(format!("{e:#}"));
            }
        }
        if error.is_none()
            && let Some(fingerprint) = &fingerprint
        {
            let record = format!(
                "{} {fingerprint}
",
                kind.as_str()
            );
            let _ = kargo_util::fs::write_string_atomic(&state_dir(m).join("fingerprint"), &record);
        }
    }
    Outcome { metadata, error, written }
}

/// Whether the last complete import of the project at `root` for `kind` saw the project as it
/// is now, so its description is up to date.
pub fn is_fresh(root: &Path, kind: ServerKind) -> bool {
    let Ok(project) = Project::discover(root, None) else { return false };
    let generated = match kind {
        ServerKind::KotlinLsp => project.root().join("workspace.json"),
        ServerKind::Kls => project.root().join(if cfg!(windows) { "kls-classpath.bat" } else { "kls-classpath" }),
    };
    let stored = std::fs::read_to_string(project.target_dir().join("lsp").join("fingerprint")).unwrap_or_default();
    generated.is_file() && stored.trim() == format!("{} {}", kind.as_str(), metadata::fingerprint(&project))
}

/// What an import wrote.
#[derive(Debug, Default)]
pub struct Written {
    /// Generated files whose contents changed.
    pub changed: Vec<PathBuf>,
    /// Generated files Kargo left alone because someone else wrote them.
    pub foreign: Vec<PathBuf>,
}

/// Writes the description `kind` reads of the project described by `metadata`.
pub fn write(kind: ServerKind, metadata: &Value) -> Result<Written> {
    let root = PathBuf::from(metadata["root"].as_str().unwrap_or_default());
    let state = state_dir(metadata);
    let mut written = Written::default();
    match kind {
        ServerKind::KotlinLsp => {
            let text = serde_json::to_string_pretty(&workspace_json(metadata))? + "\n";
            generate(&root.join("workspace.json"), &text, &state, &mut written)?;
        }
        ServerKind::Kls => {
            let classpath_file = state.join("classpath.txt");
            let classpath = kargo_util::fs::join_paths(&test_classpath(metadata));
            let changed = std::fs::read_to_string(&classpath_file).ok().as_deref() != Some(classpath.as_str());
            if changed {
                kargo_util::fs::write_string_atomic(&classpath_file, &classpath)?;
            }
            let shown = classpath_file.display();
            let (script, text) = if cfg!(windows) {
                (root.join("kls-classpath.bat"), format!("@echo off\r\nrem {MARKER}\r\ntype \"{shown}\"\r\n"))
            } else {
                (root.join("kls-classpath"), format!("#!/bin/sh\n# {MARKER}\ncat '{shown}'\n"))
            };
            generate(&script, &text, &state, &mut written)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if !written.foreign.contains(&script) {
                    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            if changed && !written.changed.contains(&script) {
                written.changed.push(script);
            }
        }
    }
    Ok(written)
}

/// Written into generated scripts.
const MARKER: &str = "Generated by `kargo lsp`; it is replaced when kargo.toml or kargo.lock change.";

/// Where `kargo lsp` keeps its state: `target/lsp`.
fn state_dir(metadata: &Value) -> PathBuf {
    PathBuf::from(metadata["target-dir"].as_str().unwrap_or("target")).join("lsp")
}

/// Writes a generated file unless someone else wrote what is there now. Kargo remembers the
/// hash of what it wrote last in the state directory.
fn generate(path: &Path, text: &str, state: &Path, written: &mut Written) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let record = state.join(format!("{name}.sha256"));
    let current = std::fs::read_to_string(path).ok();
    if current.as_deref() == Some(text) {
        return Ok(());
    }
    if let Some(current) = &current {
        let ours = std::fs::read_to_string(&record).ok().is_some_and(|h| h.trim() == kargo_util::hash::sha256_bytes(current.as_bytes()));
        if !ours {
            written.foreign.push(path.to_path_buf());
            return Ok(());
        }
    }
    kargo_util::fs::write_string_atomic(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    kargo_util::fs::write_string_atomic(&record, &kargo_util::hash::sha256_bytes(text.as_bytes()))?;
    written.changed.push(path.to_path_buf());
    Ok(())
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The JVM target of a package, if it has one.
fn jvm_target(package: &Value) -> Option<&Value> {
    package["targets"].as_array()?.iter().find(|t| t["kind"] == "jvm")
}

/// The `kotlinc` `-classpath` of a scope: Maven artifacts' paths; path dependencies are left out.
fn classpath<'a>(target: &'a Value, scope: &str) -> impl Iterator<Item = &'a Value> {
    target["dependencies"].as_array().into_iter().flatten().flat_map(move |platform| platform[scope].as_array().into_iter().flatten())
}

/// Every jar on the test classpaths of the project's JVM targets, for fwcd's server, plus the
/// output directories of path dependencies.
fn test_classpath(metadata: &Value) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let Some(jvm) = jvm_target(package) else { continue };
        for entry in classpath(jvm, "test") {
            let path = match entry["path"].as_str() {
                Some(p) => PathBuf::from(p),
                None => match package_named(metadata, str_of(entry, "package")).and_then(jvm_target) {
                    Some(dep) => PathBuf::from(str_of(dep, "classes-dir")),
                    None => continue,
                },
            };
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

fn package_named<'a>(metadata: &'a Value, name: &str) -> Option<&'a Value> {
    metadata["packages"].as_array()?.iter().find(|p| p["name"] == name)
}

fn url(path: &str) -> String {
    let path = path.replace('\\', "/");
    if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") }
}

/// The JetBrains Kotlin LSP's `workspace.json` for the project.
pub fn workspace_json(metadata: &Value) -> Value {
    let mut modules: Vec<Value> = Vec::new();
    let mut libraries: Vec<Value> = Vec::new();
    let mut kotlin_settings: Vec<Value> = Vec::new();
    let mut java_settings: Vec<Value> = Vec::new();
    let jdk = &metadata["toolchain"]["jdk"];
    let sdk_name = jdk["version"].as_u64().map(|v| v.to_string());
    let mut sdks: Vec<Value> = Vec::new();
    if let (Some(name), Some(home)) = (&sdk_name, jdk["home"].as_str()) {
        sdks.push(json!({ "name": name, "type": "JavaSDK", "version": format!("JDK_{name}"), "homePath": home, "additionalData": "" }));
    }
    let sdk = || match &sdk_name {
        Some(name) => json!({ "type": "sdk", "name": name, "kind": "JavaSDK" }),
        None => json!({ "type": "inheritedSdk" }),
    };
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let name = str_of(package, "name");
        let root = str_of(package, "root");
        modules.push(json!({
            "name": name,
            "dependencies": [{ "type": "moduleSource" }, sdk()],
            "contentRoots": [{ "path": root, "excludedUrls": [url(&Path::new(root).join("target").to_string_lossy())] }],
            "externalProjectPath": root,
            "externalProjectId": name,
        }));
        let Some(jvm) = jvm_target(package) else { continue };
        for test in [false, true] {
            let module = format!("{name}.{}", if test { "test" } else { "main" });
            let (sources, resources) = if test { ("test-roots", "test-resource-roots") } else { ("source-roots", "resource-roots") };
            let (source_type, resource_type) = if test { ("java-test", "java-test-resource") } else { ("java-source", "java-resource") };
            let mut roots: Vec<(String, &str)> = Vec::new();
            for (key, ty) in [(sources, source_type), (resources, resource_type)] {
                roots.extend(jvm[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(|p| (p.to_owned(), ty)));
            }
            let mut dependencies: Vec<Value> = Vec::new();
            if test {
                dependencies.push(json!({ "type": "module", "name": format!("{name}.main"), "scope": "compile" }));
            }
            let scopes: &[(&str, &str)] = if test { &[("test", "compile")] } else { &[("compile", "compile"), ("runtime", "runtime")] };
            for (scope, ide_scope) in scopes {
                for entry in classpath(jvm, scope) {
                    let dependency = match entry["path"].as_str() {
                        Some(jar) => {
                            let library = library_name(entry, &libraries, jar);
                            if !libraries.iter().any(|l| l["name"] == library.as_str()) {
                                libraries.push(library_entry(&library, entry));
                            }
                            json!({ "type": "library", "name": library, "scope": ide_scope })
                        }
                        None => json!({ "type": "module", "name": format!("{}.main", str_of(entry, "package")), "scope": ide_scope }),
                    };
                    // A library on both the compile and runtime classpath is a compile dependency.
                    if !dependencies.iter().any(|d| d["name"] == dependency["name"]) {
                        dependencies.push(dependency);
                    }
                }
            }
            dependencies.push(json!({ "type": "moduleSource" }));
            dependencies.push(sdk());
            modules.push(json!({
                "name": module,
                "dependencies": dependencies,
                "contentRoots": roots.iter().map(|(path, ty)| json!({ "path": path, "sourceRoots": [{ "path": path, "type": ty }] })).collect::<Vec<_>>(),
                "externalProjectPath": root,
                "externalProjectId": name,
            }));
            kotlin_settings.push(kotlin_settings_entry(package, jvm, &module, &roots, test));
            let jvm_target = str_of(jvm, "jvm-target");
            let language_level = format!("JDK_{}", jvm_target.replace('.', "_"));
            let classes = str_of(jvm, if test { "test-classes-dir" } else { "classes-dir" });
            let mut java = json!({
                "module": module,
                "inheritedCompilerOutput": false,
                "excludeOutput": true,
                "languageLevelId": language_level,
                "manifestAttributes": {},
                "compilerArguments": jvm["javac-args"],
            });
            java[if test { "compilerOutputsForTests" } else { "compilerOutputs" }] = json!([classes]);
            java_settings.push(java);
        }
    }
    json!({
        "modules": modules,
        "libraries": libraries,
        "sdks": sdks,
        "kotlinSettings": kotlin_settings,
        "javaSettings": java_settings,
    })
}

/// A library's name: `Kargo: group:artifact:version`, with the file name when a module has
/// several files (classifiers).
fn library_name(entry: &Value, libraries: &[Value], jar: &str) -> String {
    let base = format!("Kargo: {}:{}", str_of(entry, "module"), str_of(entry, "version"));
    let taken = libraries.iter().find(|l| l["name"] == base.as_str());
    match taken {
        Some(library) if library["roots"][0]["path"] != jar => {
            let file = Path::new(jar).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{base} ({file})")
        }
        _ => base,
    }
}

fn library_entry(name: &str, entry: &Value) -> Value {
    let mut roots = vec![json!({ "path": entry["path"] })];
    if let Some(sources) = entry["sources"].as_str() {
        roots.push(json!({ "path": sources, "type": "SOURCES" }));
    }
    let module = str_of(entry, "module");
    let (group, artifact) = module.split_once(':').unwrap_or(("", module));
    let version = str_of(entry, "version");
    json!({
        "name": name,
        "type": "COMPILE",
        "roots": roots,
        "properties": { "attributes": { "groupId": group, "artifactId": artifact, "version": version, "baseVersion": version } },
    })
}

fn kotlin_settings_entry(package: &Value, jvm: &Value, module: &str, roots: &[(String, &str)], test: bool) -> Value {
    let kotlin = &package["kotlin"];
    let plugins: Vec<&Value> = jvm["compiler-plugins"].as_array().into_iter().flatten().collect();
    let mut arguments = json!({
        "jvmTarget": jvm["jvm-target"],
        "pluginOptions": plugins.iter().flat_map(|p| p["options"].as_array().into_iter().flatten()).collect::<Vec<_>>(),
        "pluginClasspaths": plugins.iter().filter_map(|p| p["jar"].as_str()).collect::<Vec<_>>(),
    });
    for (key, field) in [("language-version", "languageVersion"), ("api-version", "apiVersion")] {
        if let Some(v) = kotlin[key].as_str() {
            arguments[field] = json!(v);
        }
    }
    let additional: Vec<String> = jvm["compiler-args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(shell_word).collect();
    let name = str_of(package, "name");
    json!({
        "name": "Kotlin",
        "sourceRoots": roots.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        "configFileItems": [],
        "module": module,
        "useProjectSettings": false,
        "implementedModuleNames": [],
        "dependsOnModuleNames": [],
        "additionalVisibleModuleNames": if test { vec![format!("{name}.main")] } else { Vec::new() },
        "productionOutputPath": null,
        "testOutputPath": null,
        "sourceSetNames": [],
        "isTestModule": test,
        "externalProjectId": module,
        "isHmppEnabled": true,
        "pureKotlinSourceFolders": [],
        "kind": "default",
        "compilerArguments": format!("J{arguments}"),
        "additionalArguments": if additional.is_empty() { Value::Null } else { json!(additional.join(" ")) },
        "scriptTemplates": null,
        "scriptTemplatesClasspath": null,
        "copyJsLibraryFiles": false,
        "outputDirectoryForJsLibraryFiles": null,
        "targetPlatform": null,
        "externalSystemRunTasks": [],
        "version": 5,
        "flushNeeded": false,
    })
}

/// An argument as one word of a command line, quoted only when it needs to be.
fn shell_word(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(|c: char| c.is_whitespace() || c == '"') {
        return arg.to_owned();
    }
    kargo_util::process::quote_arg(arg)
}

/// The `kargo/runnables` of the project: its programs, tests and tasks, each with the `kargo`
/// command line that runs it. With `file`, only those of the package containing it.
pub fn runnables(metadata: &Value, kargo: &str, file: Option<&Path>) -> Vec<Value> {
    let packages: Vec<&Value> = metadata["packages"].as_array().into_iter().flatten().filter(|p| p["member"] == true).collect();
    let selected: Vec<&Value> = match file {
        // The innermost package root containing the file.
        Some(file) => packages
            .iter()
            .filter(|p| file.starts_with(str_of(p, "root")))
            .max_by_key(|p| str_of(p, "root").len())
            .into_iter()
            .copied()
            .collect(),
        None => packages,
    };
    let mut out = Vec::new();
    for package in selected {
        let name = str_of(package, "name");
        let cwd = str_of(package, "root");
        let mut add = |label: String, kind: &str, target: Option<&str>, args: Vec<String>, extra: Value| {
            let mut runnable =
                json!({ "label": label, "kind": kind, "package": name, "target": target, "cwd": cwd, "program": kargo, "args": args });
            if let Value::Object(extra) = extra {
                runnable.as_object_mut().expect("an object").extend(extra);
            }
            out.push(runnable);
        };
        let args = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
        for target in package["targets"].as_array().into_iter().flatten() {
            let kind = str_of(target, "kind");
            let runnable = match kind {
                "jvm" => !target["main-class"].is_null(),
                "native" => !target["entry-point"].is_null(),
                "wasm" => target["wasm-kind"] == "application",
                "android" => true,
                _ => false,
            };
            if runnable {
                let mut extra = json!({ "mainClass": target["main-class"], "entryPoint": target["entry-point"] });
                if kind == "jvm" {
                    extra["debugArgs"] = json!(args(&["run", "-t", kind, "--debug"]));
                }
                add(format!("Run {name} ({kind})"), "run", Some(kind), args(&["run", "-t", kind]), extra);
            }
            let mut extra = json!({});
            if kind == "jvm" {
                extra["debugArgs"] = json!(args(&["test", "-t", kind, "--debug"]));
            }
            add(format!("Test {name} ({kind})"), "test", Some(kind), args(&["test", "-t", kind]), extra);
        }
        for bin in package["bins"].as_array().into_iter().flatten() {
            let bin_name = str_of(bin, "name");
            let extra = json!({ "mainClass": bin["main"], "entryPoint": bin["entry-point"], "path": bin["path"] });
            add(format!("Run {name} --bin {bin_name}"), "run", None, args(&["run", "--bin", bin_name]), extra);
        }
        for example in package["examples"].as_array().into_iter().flatten() {
            let example_name = str_of(example, "name");
            let extra = json!({ "path": example["path"] });
            add(format!("Run example {example_name}"), "run", None, args(&["run", "--example", example_name]), extra);
        }
        for task in package["tasks"].as_array().into_iter().flatten() {
            let task_name = str_of(task, "name");
            let extra = json!({ "description": task["description"] });
            add(format!("Task {task_name}"), "task", None, args(&["task", task_name]), extra);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "root": "/p",
            "target-dir": "/p/target",
            "toolchain": { "jdk": { "home": "/jdk", "version": 21 } },
            "packages": [{
                "name": "app",
                "member": true,
                "root": "/p",
                "kotlin": { "language-version": "2.2" },
                "bins": [{ "name": "tool", "main": "a.ToolKt" }],
                "examples": [{ "name": "demo", "path": "/p/examples/demo.kt" }],
                "tasks": [{ "name": "hello", "description": "Say hi" }],
                "targets": [{
                    "kind": "jvm",
                    "source-roots": ["/p/src", "/p/src-jvm"],
                    "test-roots": ["/p/tests"],
                    "resource-roots": ["/p/resources"],
                    "test-resource-roots": ["/p/test-resources"],
                    "classes-dir": "/p/target/dev/jvm/classes",
                    "test-classes-dir": "/p/target/dev/jvm/test-classes",
                    "jvm-target": "21",
                    "main-class": "a.MainKt",
                    "javac-args": [],
                    "compiler-args": ["-Xcontext-parameters"],
                    "compiler-plugins": [{ "options": ["plugin:x:k=v"], "jar": "/c/plugin.jar" }],
                    "dependencies": [{
                        "platform": "jvm",
                        "compile": [
                            { "package": "lib", "scope": "compile" },
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar" }
                        ],
                        "runtime": [
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar" },
                            { "module": "g:r", "version": "2", "path": "/c/r-2.jar", "sources": null }
                        ],
                        "test": [
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": null },
                            { "module": "g:t", "version": "3", "path": "/c/t-3.jar", "sources": null }
                        ]
                    }]
                }, { "kind": "wasm", "wasm-kind": "library" }]
            }, {
                "name": "lib",
                "member": false,
                "root": "/p/lib",
                "targets": [{ "kind": "jvm", "classes-dir": "/p/lib/target/dev/jvm/classes" }]
            }]
        })
    }

    #[test]
    fn describes_modules_libraries_and_settings() {
        let ws = workspace_json(&sample());
        let names: Vec<&str> = ws["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["app", "app.main", "app.test", "lib", "lib.main", "lib.test"]);
        let main = &ws["modules"][1];
        let deps: Vec<String> = main["dependencies"].as_array().unwrap().iter().map(|d| format!("{}:{}", d["type"], d["name"])).collect();
        assert_eq!(
            deps,
            [
                "\"module\":\"lib.main\"",
                "\"library\":\"Kargo: g:a:1\"",
                "\"library\":\"Kargo: g:r:2\"",
                "\"moduleSource\":null",
                "\"sdk\":\"21\""
            ]
        );
        assert_eq!(main["dependencies"][2]["scope"], "runtime");
        assert_eq!(main["contentRoots"][2]["sourceRoots"][0], json!({ "path": "/p/resources", "type": "java-resource" }));
        let test = &ws["modules"][2];
        assert_eq!(test["dependencies"][0]["name"], "app.main");
        assert_eq!(test["contentRoots"][0]["sourceRoots"][0]["type"], "java-test");
        let a = &ws["libraries"][0];
        assert_eq!(a["roots"][1], json!({ "path": "/c/a-1-sources.jar", "type": "SOURCES" }));
        assert_eq!(a["properties"]["attributes"]["artifactId"], "a");
        assert_eq!(ws["sdks"][0]["homePath"], "/jdk");
        let settings = &ws["kotlinSettings"][0];
        assert_eq!(settings["module"], "app.main");
        let args: Value = serde_json::from_str(settings["compilerArguments"].as_str().unwrap().strip_prefix('J').unwrap()).unwrap();
        assert_eq!(
            args,
            json!({ "jvmTarget": "21", "languageVersion": "2.2", "pluginOptions": ["plugin:x:k=v"], "pluginClasspaths": ["/c/plugin.jar"] })
        );
        assert_eq!(settings["additionalArguments"], "-Xcontext-parameters");
        assert_eq!(ws["javaSettings"][1]["compilerOutputsForTests"], json!(["/p/target/dev/jvm/test-classes"]));
        assert_eq!(ws["javaSettings"][0]["languageLevelId"], "JDK_21");
    }

    #[test]
    fn lists_the_test_classpath_for_kls() {
        let cp = test_classpath(&sample());
        assert_eq!(cp, [PathBuf::from("/c/a-1.jar"), PathBuf::from("/c/t-3.jar")]);
    }

    #[test]
    fn lists_runnables() {
        let all = runnables(&sample(), "kargo", None);
        let labels: Vec<&str> = all.iter().map(|r| r["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Run app (jvm)", "Test app (jvm)", "Test app (wasm)", "Run app --bin tool", "Run example demo", "Task hello"]);
        assert_eq!(all[0]["args"], json!(["run", "-t", "jvm"]));
        assert_eq!(all[0]["debugArgs"], json!(["run", "-t", "jvm", "--debug"]));
        assert_eq!(all[0]["mainClass"], "a.MainKt");
        assert_eq!(all[0]["cwd"], "/p");
        assert_eq!(all[5]["args"], json!(["task", "hello"]));
        assert_eq!(all[5]["description"], "Say hi");
        assert_eq!(runnables(&sample(), "kargo", Some(Path::new("/p/src/Main.kt"))).len(), 6);
        assert!(runnables(&sample(), "kargo", Some(Path::new("/elsewhere/x.kt"))).is_empty());
    }

    #[test]
    fn leaves_foreign_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut metadata = sample();
        metadata["root"] = json!(root.display().to_string());
        metadata["target-dir"] = json!(root.join("target").display().to_string());

        let written = write(ServerKind::KotlinLsp, &metadata).unwrap();
        assert_eq!(written.changed, [root.join("workspace.json")]);
        assert!(write(ServerKind::KotlinLsp, &metadata).unwrap().changed.is_empty(), "unchanged");
        metadata["packages"][0]["name"] = json!("renamed");
        assert_eq!(write(ServerKind::KotlinLsp, &metadata).unwrap().changed.len(), 1, "Kargo's own file is replaced");

        std::fs::write(root.join("workspace.json"), "{}").unwrap();
        let written = write(ServerKind::KotlinLsp, &metadata).unwrap();
        assert!(written.changed.is_empty());
        assert_eq!(written.foreign, [root.join("workspace.json")]);
        assert_eq!(std::fs::read_to_string(root.join("workspace.json")).unwrap(), "{}");

        let written = write(ServerKind::Kls, &metadata).unwrap();
        let script = root.join(if cfg!(windows) { "kls-classpath.bat" } else { "kls-classpath" });
        assert_eq!(written.changed, std::slice::from_ref(&script));
        assert!(std::fs::read_to_string(&script).unwrap().contains("classpath.txt"));
        let classpath = std::fs::read_to_string(root.join("target/lsp/classpath.txt")).unwrap();
        assert!(classpath.contains("t-3.jar"), "{classpath}");
    }
}
