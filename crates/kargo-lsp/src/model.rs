//! The project model of Kargo's own Kotlin server (`kargo lsp --server kargo`), built from
//! `kargo metadata`.
//!
//! `kargo lsp` sends it in `initializationOptions.kargo.project` of the `initialize` request and,
//! after each import, in a `kargo/projectChanged` notification. The format is versioned
//! ([`MODEL_VERSION`]) and documented for tool authors on the website's "LSP project model" page.
//!
//! - There is one module per package, source set and scope (`main`, `test`). A package with
//!   one target has one source set, named after the target, holding `src/` and `src-<target>/`.
//!   A multiplatform package has a `common` source set for `src/` (and `tests/`) and one per
//!   target for `src-<target>/`, which `dependsOn` it.
//! - Test modules see their main modules' `internal` declarations (`friends`).
//! - Path dependencies and workspace members are module dependencies, so edits in them are seen
//!   without building them.
//! - Libraries are listed once and referenced by id (`lib:<id>`; modules are `module:<id>`).
//! - Paths are absolute, with an upper-case drive letter on Windows.
//!
//! Android code is left to Android Studio. A Native source set is analyzed for the first of
//! its platforms.

use std::path::Path;

use serde_json::{Value, json};

use crate::project::native_klibs;
use crate::text;

/// The version of the model's format. It changes only when a field is removed or changes its
/// meaning; new fields can appear at any time.
pub const MODEL_VERSION: u32 = 1;

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// An absolute path with an upper-case Windows drive letter, so the same file always has the
/// same spelling.
pub fn normalize(path: &str) -> String {
    let mut chars: Vec<char> = path.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        chars[0] = chars[0].to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

fn paths(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values.into_iter().map(|p| normalize(&p)).collect()
}

/// The platform a target is analyzed for: `jvm`, `wasm-js`, `wasm-wasi` or `native:<platform>`.
fn platform(target: &Value) -> Option<String> {
    match str_of(target, "kind") {
        "jvm" => Some("jvm".to_owned()),
        "wasm" if target["environment"] == "wasi" => Some("wasm-wasi".to_owned()),
        "wasm" => Some("wasm-js".to_owned()),
        "native" => Some(format!("native:{}", target["platforms"][0].as_str().unwrap_or_default())),
        _ => None,
    }
}

/// The targets of a package the model describes: JVM, Wasm and Native.
fn analyzed_targets(package: &Value) -> Vec<&Value> {
    package["targets"].as_array().into_iter().flatten().filter(|t| platform(t).is_some()).collect()
}

/// Whether `src/` of a package is common code compiled for several targets.
fn is_multiplatform(package: &Value) -> bool {
    package["targets"].as_array().is_some_and(|t| t.len() > 1)
}

/// The target whose libraries and compiler settings the common source set of a multiplatform
/// package gets: its JVM target, or its first one. Kargo resolves libraries per platform, not
/// their common (metadata) variants.
fn primary_target(package: &Value) -> Option<&Value> {
    let targets = analyzed_targets(package);
    targets.iter().copied().find(|t| t["kind"] == "jvm").or_else(|| targets.first().copied())
}

fn module_id(package: &str, source_set: &str, scope: &str) -> String {
    format!("{package}.{source_set}.{scope}")
}

fn scope_name(test: bool) -> &'static str {
    if test { "test" } else { "main" }
}

/// The `kargo metadata` keys of a target's source and resource roots for main or test code.
fn root_keys(test: bool) -> (&'static str, &'static str) {
    if test { ("test-roots", "test-resource-roots") } else { ("source-roots", "resource-roots") }
}

/// The main modules of the package `dep` that a module of a `kind` target sees, or with `common`
/// that a common module sees.
fn dependency_modules(dep: &Value, kind: &str, common: bool) -> Vec<String> {
    let name = str_of(dep, "name");
    let analyzed = analyzed_targets(dep);
    let has_kind = analyzed.iter().any(|t| t["kind"] == kind);
    if !is_multiplatform(dep) {
        return match analyzed.as_slice() {
            [only] if common || only["kind"] == kind => vec![module_id(name, str_of(only, "kind"), "main")],
            _ => Vec::new(),
        };
    }
    if analyzed.is_empty() {
        return Vec::new();
    }
    let common_main = module_id(name, "common", "main");
    match (common, has_kind) {
        (false, true) => vec![module_id(name, kind, "main"), common_main],
        _ => vec![common_main],
    }
}

/// The model while it is being built.
struct Model<'a> {
    metadata: &'a Value,
    libraries: Vec<Value>,
    modules: Vec<Value>,
}

/// One module of the model.
struct Module<'a> {
    package: &'a Value,
    source_set: String,
    test: bool,
    /// The target whose libraries and compiler settings it gets.
    target: &'a Value,
    platforms: Vec<String>,
    source_roots: Vec<String>,
    resource_roots: Vec<String>,
    depends_on: Vec<String>,
    friends: Vec<String>,
    /// Modules it depends on before its libraries.
    modules: Vec<String>,
    /// A source set of a multiplatform package.
    multiplatform: bool,
    /// The common source set, whose path dependencies are those packages' common code.
    common: bool,
}

/// The project model Kargo's Kotlin server reads, from a `kargo metadata` document.
pub fn kargo_model(metadata: &Value) -> Value {
    let mut model = Model { metadata, libraries: Vec::new(), modules: Vec::new() };
    for package in metadata["packages"].as_array().into_iter().flatten() {
        model.package(package);
    }
    let jdk = &metadata["toolchain"]["jdk"];
    let jdk = match jdk["home"].as_str() {
        Some(home) => json!({ "home": normalize(home), "version": jdk["version"] }),
        None => Value::Null,
    };
    let root = normalize(str_of(metadata, "root"));
    json!({
        "modelVersion": MODEL_VERSION,
        "kargoVersion": metadata["kargo-version"],
        "root": text::path_to_uri(Path::new(&root)),
        "targetDir": normalize(str_of(metadata, "target-dir")),
        "resolved": metadata["resolved"] == true,
        "jdk": jdk,
        "kotlin": { "projectVersion": metadata["toolchain"]["kotlin"] },
        "libraries": model.libraries,
        "modules": model.modules,
    })
}

impl<'a> Model<'a> {
    fn package(&mut self, package: &'a Value) {
        let name = str_of(package, "name");
        let analyzed = analyzed_targets(package);
        let roots = |target: &Value, key: &str| -> Vec<String> {
            target[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
        };
        if !is_multiplatform(package) {
            let [target] = analyzed.as_slice() else { return };
            let kind = str_of(target, "kind");
            for test in [false, true] {
                let (sources, resources) = root_keys(test);
                let main = module_id(name, kind, "main");
                let own = if test { vec![main] } else { Vec::new() };
                self.add(Module {
                    package,
                    source_set: kind.to_owned(),
                    test,
                    target,
                    platforms: platform(target).into_iter().collect(),
                    source_roots: roots(target, sources),
                    resource_roots: roots(target, resources),
                    depends_on: Vec::new(),
                    friends: own.clone(),
                    modules: own,
                    multiplatform: false,
                    common: false,
                });
            }
            return;
        }
        let Some(primary) = primary_target(package) else { return };
        let common_main = module_id(name, "common", "main");
        for test in [false, true] {
            let (sources, resources) = root_keys(test);
            let common = module_id(name, "common", scope_name(test));
            let own = if test { vec![common_main.clone()] } else { Vec::new() };
            self.add(Module {
                package,
                source_set: "common".to_owned(),
                test,
                target: primary,
                platforms: analyzed.iter().filter_map(|t| platform(t)).collect(),
                // `kargo metadata` lists the common directory first.
                source_roots: roots(primary, sources).into_iter().take(1).collect(),
                resource_roots: Vec::new(),
                depends_on: Vec::new(),
                friends: own.clone(),
                modules: own,
                multiplatform: true,
                common: true,
            });
            for &target in &analyzed {
                let kind = str_of(target, "kind");
                let main = module_id(name, kind, "main");
                let own = if test { vec![main, common_main.clone()] } else { Vec::new() };
                self.add(Module {
                    package,
                    source_set: kind.to_owned(),
                    test,
                    target,
                    platforms: platform(target).into_iter().collect(),
                    source_roots: roots(target, sources).into_iter().skip(1).collect(),
                    resource_roots: roots(target, resources),
                    depends_on: vec![common.clone()],
                    friends: own.clone(),
                    modules: own,
                    multiplatform: true,
                    common: false,
                });
            }
        }
    }

    fn add(&mut self, module: Module) {
        let package_name = str_of(module.package, "name");
        let kind = str_of(module.target, "kind");
        let target_platform = platform(module.target).unwrap_or_default();
        let mut dependencies: Vec<String> = module.modules.iter().map(|m| format!("module:{m}")).collect();
        let push = |dependency: String, dependencies: &mut Vec<String>| {
            if !dependencies.contains(&dependency) {
                dependencies.push(dependency);
            }
        };
        // The compile classpath of main code, the test classpath of tests; a Native target's are
        // those of its first platform.
        let classpath = module.target["dependencies"][0][if module.test { "test" } else { "compile" }].as_array();
        for entry in classpath.into_iter().flatten() {
            match entry["path"].as_str() {
                Some(file) => {
                    let id = self.library(entry, file, &target_platform);
                    push(format!("lib:{id}"), &mut dependencies);
                }
                None => {
                    let found = self.package_named(str_of(entry, "package")).map(|dep| dependency_modules(dep, kind, module.common));
                    for id in found.into_iter().flatten() {
                        push(format!("module:{id}"), &mut dependencies);
                    }
                }
            }
        }
        if kind == "native" && !module.common {
            let konan = module.target["platforms"][0].as_str().unwrap_or_default();
            let version = str_of(&self.metadata["toolchain"]["kotlin-native"], "version").to_owned();
            for klib in native_klibs(self.metadata, konan) {
                let (id, library_platform) = match &klib.platform {
                    Some(platform) => (format!("kotlin-native:{version}:{platform}:{}", klib.name), format!("native:{platform}")),
                    None => (format!("kotlin-native:{version}:{}", klib.name), "native".to_owned()),
                };
                if !self.libraries.iter().any(|l| l["id"] == id.as_str()) {
                    self.libraries.push(json!({
                        "id": id,
                        "kind": "klib",
                        "path": normalize(&klib.path.display().to_string()),
                        "sources": null,
                        "sha256": null,
                        "platform": library_platform,
                    }));
                }
                push(format!("lib:{id}"), &mut dependencies);
            }
        }
        let kotlin = &module.package["kotlin"];
        let plugins: Vec<Value> = module.target["compiler-plugins"].as_array().into_iter().flatten().map(plugin).collect();
        let jvm_target = if kind == "jvm" && !module.common { module.target["jvm-target"].clone() } else { Value::Null };
        let scope = scope_name(module.test);
        self.modules.push(json!({
            "id": module_id(package_name, &module.source_set, scope),
            "package": package_name,
            "member": module.package["member"] == true,
            "sourceSet": module.source_set,
            "scope": scope,
            "platform": module.platforms,
            "sourceRoots": paths(module.source_roots),
            "resourceRoots": paths(module.resource_roots),
            "generatedRoots": [],
            "dependsOn": module.depends_on,
            "friends": module.friends,
            "dependencies": dependencies,
            "compiler": {
                "languageVersion": kotlin["language-version"],
                "apiVersion": kotlin["api-version"],
                "optIn": kotlin["opt-in"].as_array().cloned().unwrap_or_default(),
                "warningsAsErrors": kotlin["warnings-as-errors"] == true,
                "multiplatform": module.multiplatform,
                "jvmTarget": jvm_target,
                "freeArgs": module.target["compiler-args"].as_array().cloned().unwrap_or_default(),
                "plugins": plugins,
            },
        }));
    }

    fn package_named(&self, name: &str) -> Option<&'a Value> {
        self.metadata["packages"].as_array()?.iter().find(|p| p["name"] == name)
    }

    /// The id of the library for a classpath entry, added to the model's libraries once.
    fn library(&mut self, entry: &Value, file: &str, platform: &str) -> String {
        let path = normalize(file);
        if let Some(found) = self.libraries.iter().find(|l| l["path"] == path.as_str()) {
            return str_of(found, "id").to_owned();
        }
        let base = format!("{}:{}", str_of(entry, "module"), str_of(entry, "version"));
        // A module with several files (classifiers) gets an id per file.
        let id = if self.libraries.iter().any(|l| l["id"] == base.as_str()) {
            let stem = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{base}:{stem}")
        } else {
            base
        };
        let kind = match entry["kind"].as_str() {
            Some(kind) => kind.to_owned(),
            None => Path::new(file).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default(),
        };
        self.libraries.push(json!({
            "id": id,
            "kind": kind,
            "path": path,
            "sources": entry["sources"].as_str().map(normalize),
            "sha256": entry["sha256"],
            "platform": platform,
        }));
        id
    }
}

/// A compiler plugin: its id, Maven module and options (`-P plugin:<id>:<name>=<value>`).
fn plugin(plugin: &Value) -> Value {
    let id = str_of(plugin, "id");
    let prefix = format!("plugin:{id}:");
    let options: Vec<Value> = plugin["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|o| o.strip_prefix(&prefix)?.split_once('='))
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    json!({ "id": plugin["id"], "module": plugin["module"], "options": options })
}

/// The ids of the modules that differ between two models: changed, added or removed.
pub fn changed_modules(old: &Value, new: &Value) -> Vec<String> {
    let by_id = |model: &Value| -> Vec<(String, Value)> {
        model["modules"].as_array().into_iter().flatten().map(|m| (str_of(m, "id").to_owned(), m.clone())).collect()
    };
    let (old, new) = (by_id(old), by_id(new));
    let mut changed: Vec<String> = Vec::new();
    for (id, module) in &new {
        if !old.iter().any(|(i, m)| i == id && m == module) {
            changed.push(id.clone());
        }
    }
    for (id, _) in &old {
        if !new.iter().any(|(i, _)| i == id) {
            changed.push(id.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JVM application depending on a library and on `lib`, a JVM + Wasm path dependency.
    fn sample() -> Value {
        json!({
            "kargo-version": "0.9.0",
            "root": "c:\\p",
            "target-dir": "c:\\p\\target",
            "resolved": true,
            "toolchain": { "kotlin": "2.4.21", "jdk": { "home": "c:\\jdk", "version": 21 } },
            "packages": [{
                "name": "app",
                "member": true,
                "root": "c:\\p",
                "kotlin": { "language-version": "2.2", "opt-in": ["kotlin.RequiresOptIn"] },
                "targets": [{
                    "kind": "jvm",
                    "source-roots": ["c:\\p\\src", "c:\\p\\src-jvm"],
                    "test-roots": ["c:\\p\\tests", "c:\\p\\tests-jvm"],
                    "resource-roots": ["c:\\p\\resources"],
                    "test-resource-roots": ["c:\\p\\test-resources"],
                    "jvm-target": "21",
                    "compiler-args": ["-Xcontext-parameters"],
                    "compiler-plugins": [{
                        "module": "org.jetbrains.kotlin:kotlin-serialization-compiler-plugin",
                        "id": "org.jetbrains.kotlinx.serialization",
                        "options": ["plugin:org.jetbrains.kotlinx.serialization:k=v"],
                        "jar": "/c/plugin.jar"
                    }],
                    "dependencies": [{
                        "platform": "jvm",
                        "compile": [
                            { "package": "lib", "scope": "compile" },
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar", "kind": "jar", "sha256": "aa" }
                        ],
                        "runtime": [{ "module": "g:r", "version": "2", "path": "/c/r-2.jar", "sources": null }],
                        "test": [
                            { "package": "lib", "scope": "compile" },
                            { "module": "g:a", "version": "1", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar", "kind": "jar", "sha256": "aa" },
                            { "module": "g:a", "version": "1", "path": "/c/a-1-tests.jar", "sources": null, "kind": "jar", "sha256": "ab" },
                            { "module": "g:t", "version": "3", "path": "/c/t-3.jar", "sources": null, "kind": "jar", "sha256": "tt" }
                        ]
                    }]
                }]
            }, {
                "name": "lib",
                "member": false,
                "root": "c:\\p\\lib",
                "kotlin": {},
                "targets": [{
                    "kind": "jvm",
                    "source-roots": ["c:\\p\\lib\\src", "c:\\p\\lib\\src-jvm"],
                    "test-roots": ["c:\\p\\lib\\tests", "c:\\p\\lib\\tests-jvm"],
                    "jvm-target": "17",
                    "compiler-args": [],
                    "dependencies": [{ "platform": "jvm", "compile": [], "runtime": [], "test": [] }]
                }, {
                    "kind": "wasm",
                    "environment": "browser",
                    "source-roots": ["c:\\p\\lib\\src", "c:\\p\\lib\\src-wasm"],
                    "test-roots": ["c:\\p\\lib\\tests", "c:\\p\\lib\\tests-wasm"],
                    "compiler-args": [],
                    "dependencies": [{
                        "platform": "wasm-js",
                        "compile": [{ "module": "g:w-wasm-js", "version": "1", "path": "/c/w-wasm-js-1.klib", "sources": null, "kind": "klib", "sha256": "ww" }],
                        "runtime": [],
                        "test": []
                    }]
                }, { "kind": "android" }]
            }]
        })
    }

    fn module<'a>(model: &'a Value, id: &str) -> &'a Value {
        model["modules"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no module {id}"))
    }

    fn ids(model: &Value) -> Vec<&str> {
        model["modules"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect()
    }

    #[test]
    fn describes_a_single_target_package() {
        let model = kargo_model(&sample());
        assert_eq!(model["modelVersion"], MODEL_VERSION);
        assert_eq!(model["root"], "file:///C:/p", "an upper-case drive letter");
        assert_eq!(model["jdk"], json!({ "home": "C:\\jdk", "version": 21 }));
        assert_eq!(model["kotlin"]["projectVersion"], "2.4.21");
        assert_eq!(
            ids(&model),
            [
                "app.jvm.main",
                "app.jvm.test",
                "lib.common.main",
                "lib.jvm.main",
                "lib.wasm.main",
                "lib.common.test",
                "lib.jvm.test",
                "lib.wasm.test"
            ]
        );

        let main = module(&model, "app.jvm.main");
        assert_eq!(main["sourceSet"], "jvm");
        assert_eq!(main["platform"], json!(["jvm"]));
        assert_eq!(main["sourceRoots"], json!(["C:\\p\\src", "C:\\p\\src-jvm"]));
        assert_eq!(main["resourceRoots"], json!(["C:\\p\\resources"]));
        assert_eq!(main["dependsOn"], json!([]));
        // The JVM code of the path dependency, its common code, then the libraries; runtime-only
        // libraries aren't visible to the code.
        assert_eq!(main["dependencies"], json!(["module:lib.jvm.main", "module:lib.common.main", "lib:g:a:1"]));
        let compiler = &main["compiler"];
        assert_eq!(compiler["languageVersion"], "2.2");
        assert_eq!(compiler["optIn"], json!(["kotlin.RequiresOptIn"]));
        assert_eq!(compiler["jvmTarget"], "21");
        assert_eq!(compiler["freeArgs"], json!(["-Xcontext-parameters"]));
        assert_eq!(compiler["multiplatform"], false);
        assert_eq!(
            compiler["plugins"],
            json!([{
                "id": "org.jetbrains.kotlinx.serialization",
                "module": "org.jetbrains.kotlin:kotlin-serialization-compiler-plugin",
                "options": [{ "name": "k", "value": "v" }]
            }])
        );

        let test = module(&model, "app.jvm.test");
        assert_eq!(test["friends"], json!(["app.jvm.main"]));
        assert_eq!(
            test["dependencies"],
            json!([
                "module:app.jvm.main",
                "module:lib.jvm.main",
                "module:lib.common.main",
                "lib:g:a:1",
                "lib:g:a:1:a-1-tests",
                "lib:g:t:3"
            ])
        );
        assert_eq!(test["resourceRoots"], json!(["C:\\p\\test-resources"]));
    }

    #[test]
    fn lists_each_library_once() {
        let model = kargo_model(&sample());
        let libraries = model["libraries"].as_array().unwrap();
        let ids: Vec<&str> = libraries.iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["g:a:1", "g:a:1:a-1-tests", "g:t:3", "g:w-wasm-js:1"]);
        assert_eq!(
            libraries[0],
            json!({ "id": "g:a:1", "kind": "jar", "path": "/c/a-1.jar", "sources": "/c/a-1-sources.jar", "sha256": "aa", "platform": "jvm" })
        );
        assert_eq!(libraries[3]["platform"], "wasm-js");
        assert_eq!(libraries[3]["kind"], "klib");
    }

    #[test]
    fn describes_the_source_sets_of_a_multiplatform_package() {
        let model = kargo_model(&sample());
        let common = module(&model, "lib.common.main");
        assert_eq!(common["platform"], json!(["jvm", "wasm-js"]), "the union of its targets'");
        assert_eq!(common["sourceRoots"], json!(["C:\\p\\lib\\src"]));
        assert_eq!(common["compiler"]["multiplatform"], true);
        assert_eq!(common["compiler"]["jvmTarget"], Value::Null);
        assert_eq!(common["member"], false);

        let wasm = module(&model, "lib.wasm.main");
        assert_eq!(wasm["sourceRoots"], json!(["C:\\p\\lib\\src-wasm"]));
        assert_eq!(wasm["dependsOn"], json!(["lib.common.main"]));
        assert_eq!(wasm["dependencies"], json!(["lib:g:w-wasm-js:1"]));
        assert_eq!(module(&model, "lib.jvm.main")["compiler"]["jvmTarget"], "17");

        let test = module(&model, "lib.wasm.test");
        assert_eq!(test["dependsOn"], json!(["lib.common.test"]));
        assert_eq!(test["friends"], json!(["lib.wasm.main", "lib.common.main"]));
        assert_eq!(test["dependencies"], json!(["module:lib.wasm.main", "module:lib.common.main"]));
        assert_eq!(module(&model, "lib.common.test")["friends"], json!(["lib.common.main"]));
    }

    #[test]
    fn a_common_module_sees_the_common_code_of_its_path_dependencies() {
        let mut metadata = sample();
        let mut wasm = metadata["packages"][1]["targets"][1].clone();
        wasm["source-roots"] = json!(["c:\\p\\src", "c:\\p\\src-wasm"]);
        wasm["dependencies"][0]["compile"] = json!([{ "package": "lib", "scope": "compile" }]);
        metadata["packages"][0]["targets"].as_array_mut().unwrap().push(wasm);
        let model = kargo_model(&metadata);
        assert_eq!(module(&model, "app.common.main")["dependencies"], json!(["module:lib.common.main", "lib:g:a:1"]));
        assert_eq!(module(&model, "app.wasm.main")["dependencies"], json!(["module:lib.wasm.main", "module:lib.common.main"]));
    }

    #[test]
    fn native_source_sets_get_the_distribution_klibs() {
        let home = tempfile::tempdir().unwrap();
        for dir in ["klib/common/stdlib", "klib/platform/linux_x64/posix"] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        let metadata = json!({
            "root": "/p",
            "toolchain": { "kotlin": "2.4.21", "jdk": {}, "kotlin-native": { "home": home.path().display().to_string(), "version": "2.4.21" } },
            "packages": [{
                "name": "app",
                "kotlin": {},
                "targets": [{
                    "kind": "native",
                    "platforms": ["linux_x64", "mingw_x64"],
                    "source-roots": ["/p/src", "/p/src-native"],
                    "test-roots": ["/p/tests"],
                    "dependencies": [{ "platform": "native-linux_x64", "compile": [], "runtime": [], "test": [] }]
                }]
            }]
        });
        let model = kargo_model(&metadata);
        assert_eq!(model["jdk"], Value::Null);
        let main = module(&model, "app.native.main");
        assert_eq!(main["platform"], json!(["native:linux_x64"]));
        assert_eq!(main["dependencies"], json!(["lib:kotlin-native:2.4.21:stdlib", "lib:kotlin-native:2.4.21:linux_x64:posix"]));
        let posix = model["libraries"].as_array().unwrap().iter().find(|l| l["id"] == "kotlin-native:2.4.21:linux_x64:posix").unwrap();
        assert_eq!(posix["platform"], "native:linux_x64");
        assert_eq!(posix["kind"], "klib");
    }

    #[test]
    fn lists_changed_modules() {
        let old = kargo_model(&sample());
        assert!(changed_modules(&old, &old).is_empty());
        let mut metadata = sample();
        metadata["packages"][0]["kotlin"]["language-version"] = json!("2.3");
        metadata["packages"][1]["targets"].as_array_mut().unwrap().remove(1);
        let new = kargo_model(&metadata);
        let changed = changed_modules(&old, &new);
        assert_eq!(changed[..2], ["app.jvm.main", "app.jvm.test"]);
        assert!(changed.contains(&"lib.wasm.main".to_owned()), "removed: {changed:?}");
    }

    #[test]
    fn normalizes_drive_letters() {
        assert_eq!(normalize("c:\\x"), "C:\\x");
        assert_eq!(normalize("/home/x"), "/home/x");
    }
}
