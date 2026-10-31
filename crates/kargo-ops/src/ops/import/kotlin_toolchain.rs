//! Reading a Kotlin Toolchain (formerly Amper) project: `project.yaml`, `module.yaml`,
//! `libs.versions.toml` and the `src@<platform>` source directories.
//!
//! The configuration is YAML, but declarative: like the Gradle reader, this reads what the files say
//! and lists the keys that Kargo has no equivalent for instead of running the build.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_yaml_ng::{Mapping, Value as Yaml};
use walkdir::WalkDir;

use kargo_manifest::{DEFAULT_KOTLIN_VERSION, TargetKind};

use super::frameworks::{self, Hints};
use super::gradle::catalog::{Catalog, Library};
use super::gradle::dsl::normalize_alias;
use super::gradle::{is_standard_repository, package_name, repository_name};
use super::maven::java_target;
use super::model::{CompilerPlugin, Dependency, DirMove, KotlinSettings, LocalPackage, Package, Repository, Scope, TargetSettings, Value};
use super::{Imported, relative_dir};

/// The documentation page about moving from the Kotlin Toolchain.
const MIGRATION: &str = "migration/from-kotlin-toolchain/";

pub(super) fn read(root: &Path) -> Result<Imported> {
    let mut build = Build::load(root)?;
    build.run()?;
    Ok(build.imported)
}

/// The kind of product a module builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Product {
    JvmApp,
    JvmLib,
    AndroidApp,
    IosApp,
    LinuxApp,
    MacosApp,
    WindowsApp,
    WasmJsApp,
    WasmWasiApp,
    JsApp,
    KmpLib,
    Plugin,
}

/// `product` of a `module.yaml`, or the defaults of its type.
#[derive(Debug, Clone)]
struct ProductDef {
    product: Product,
    platforms: Vec<String>,
}

struct Build {
    root: PathBuf,
    catalog: Option<Catalog>,
    project: Option<Yaml>,
    imported: Imported,
}

/// Everything `settings` says, in Kargo's model.
#[derive(Debug, Default)]
struct SettingsRead {
    kotlin: KotlinSettings,
    jvm_target: Option<String>,
    jdk: Option<String>,
    main_class: Option<String>,
    javac_args: Vec<String>,
    group: Option<String>,
    version: Option<String>,
    android: Vec<(String, Value)>,
    native_entry: Option<String>,
    /// The version of `settings.kotlin.serialization`, if it sets one.
    serialization_version: Option<String>,
    /// The formats `settings.kotlin.serialization` adds.
    serialization_formats: Vec<String>,
    hints: Hints,
    extra_dependencies: Vec<Dependency>,
    plugins: Vec<CompilerPlugin>,
    notes: Vec<String>,
}

/// The versions the toolchain catalogs (`$compose`, `$kotlin`, ...) take.
#[derive(Debug, Default)]
struct CatalogVersions {
    compose: Option<String>,
    /// Compose is a framework, which adds the catalog's libraries itself.
    compose_enabled: bool,
    ktor: Option<String>,
    spring_boot: Option<String>,
    serialization: Option<String>,
}

impl Build {
    fn load(root: &Path) -> Result<Self> {
        let project = read_yaml(&root.join("project.yaml"))?;
        let catalog_path = if root.join("libs.versions.toml").is_file() {
            Some(root.join("libs.versions.toml"))
        } else if root.join("gradle/libs.versions.toml").is_file() {
            Some(root.join("gradle/libs.versions.toml"))
        } else {
            None
        };
        let catalog = match catalog_path {
            Some(path) => {
                let text = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
                Catalog::parse("libs", &text).with_context(|| format!("failed to read the version catalog {}", path.display())).ok()
            }
            None => None,
        };
        Ok(Self { root: root.to_path_buf(), catalog, project, imported: Imported::default() })
    }

    fn manual(&mut self, message: impl Into<String>) {
        self.imported.manual(message, MIGRATION);
    }

    fn run(&mut self) -> Result<()> {
        let (mut dirs, notes) = module_dirs(&self.root, self.project.as_ref());
        for note in notes {
            self.manual(note);
        }
        if dirs.is_empty() {
            bail!(
                "there is no module.yaml in {} or in the directories of project.yaml; `kargo import` needs a Kotlin Toolchain module",
                self.root.display()
            );
        }
        dirs.sort();
        dirs.dedup();
        dirs.retain(|dir| {
            if self.root.join(dir).join("module.yaml").is_file() {
                true
            } else {
                self.manual(format!("the module `{}` is listed, but has no module.yaml", display_dir(dir)));
                false
            }
        });
        if dirs.is_empty() {
            bail!("none of the modules of project.yaml has a module.yaml");
        }

        if self.project.is_some()
            && dirs.len() > 1
            && let Some(root_module) = dirs.iter().position(|dir| dir.as_os_str().is_empty())
        {
            dirs.remove(root_module);
            self.manual(
                "the project has a root module and other modules; Kargo's workspace root has no package, so the root module's code was not imported",
            );
        } else if self.project.is_none() && dirs.len() > 1 {
            bail!("a Kotlin Toolchain project with several modules needs a project.yaml");
        }
        let workspace = dirs.len() > 1;

        if let Some(project) = self.project.as_ref() {
            for note in project_notes(project) {
                self.manual(note);
            }
        }

        // The names of the packages, and what a `//path` dependency refers to.
        let mut names: HashMap<PathBuf, String> = HashMap::new();
        let mut used: BTreeSet<String> = BTreeSet::new();
        for dir in &dirs {
            let last = if dir.as_os_str().is_empty() {
                self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "app".to_owned())
            } else {
                dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "app".to_owned())
            };
            let (mut name, renamed) = package_name(&last);
            if renamed {
                self.manual(format!("the module name `{last}` is not a valid package name; the package is called `{name}`"));
            }
            // Two directories called `lib` in different places get distinct names.
            if used.contains(&name) {
                let mut candidate = format!("{}-{}", display_dir(dir).replace('/', "-"), name);
                let mut n = 2;
                while used.contains(&candidate) {
                    candidate = format!("{}-{}", display_dir(dir).replace('/', "-"), n);
                    n += 1;
                }
                name = candidate;
            }
            used.insert(name.clone());
            names.insert(dir.clone(), name);
        }
        let locals: HashMap<PathBuf, LocalPackage> =
            dirs.iter().map(|dir| (dir.clone(), LocalPackage { name: names[dir].clone(), dir: dir.clone() })).collect();

        let mut packages = Vec::new();
        for dir in &dirs {
            let path = self.root.join(dir).join("module.yaml");
            let yaml = match read_yaml(&path)? {
                Some(yaml) => yaml,
                None => continue,
            };
            let Some(package) = self.translate(dir, &yaml, &names[dir], &locals)? else { continue };
            packages.push(package);
        }
        if packages.is_empty() {
            bail!("none of the modules of this Kotlin Toolchain project could be imported");
        }
        self.imported.workspace = workspace;
        if workspace {
            let list: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
            self.imported.translated(format!("project.yaml modules -> workspace members ({})", list.join(", ")));
        }
        self.imported.packages = packages;
        Ok(())
    }

    fn translate(&mut self, dir: &Path, yaml: &Yaml, name: &str, locals: &HashMap<PathBuf, LocalPackage>) -> Result<Option<Package>> {
        let module = yaml.as_mapping().ok_or_else(|| anyhow::anyhow!("{} is not a YAML mapping", display_dir(dir)))?;
        for key in unknown_keys(module) {
            self.manual(format!("`{}`: the key `{key}` has no Kargo equivalent", display_dir(dir)));
        }
        let Some(product) = product_of(module) else {
            if let Some(kind) = at(module, "product").and_then(|product| match product {
                Yaml::String(kind) => Some(kind.clone()),
                Yaml::Mapping(map) => at(map, "type").and_then(text),
                _ => None,
            }) {
                self.manual(format!("`{}`: the product type `{kind}` has no Kargo equivalent", display_dir(dir)));
            } else {
                self.manual(format!(
                    "`{}` has no product (a template may define it); write its `product` and run `kargo import` again",
                    display_dir(dir)
                ));
            }
            return Ok(None);
        };
        if product.product == Product::Plugin {
            self.manual(format!(
                "`{}` is a Kotlin Toolchain plugin (`jvm/amper-plugin`), which Kargo cannot run; recreate it as a `kargo-<name>` subcommand",
                display_dir(dir)
            ));
            return Ok(None);
        }

        let aliases = aliases_of(module);
        let versions = catalog_versions(module);
        let settings = read_settings(module, &aliases, self.catalog.as_ref(), &versions);
        for note in &settings.notes {
            self.manual(format!("`{}`: {note}", display_dir(dir)));
        }
        if module_has_platform_settings(module) {
            self.manual(format!(
                "`{}`: the platform-specific settings (`settings@...`) were applied to the whole package",
                display_dir(dir)
            ));
        }

        let mut package = Package {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            version: settings.version.clone().unwrap_or_else(|| "0.1.0".to_owned()),
            group: settings.group.clone(),
            ..Package::default()
        };
        package.description = at(module, "description").and_then(text);

        // Targets.
        let (mut targets, target_notes) = target_settings(&product, &settings, name);
        for note in target_notes {
            self.manual(format!("`{}`: {note}", display_dir(dir)));
        }
        if targets.is_empty() {
            self.manual(format!("`{}` has no target Kargo can build, so it was not imported", display_dir(dir)));
            return Ok(None);
        }
        let enabled: BTreeSet<TargetKind> = targets.iter().map(|t| t.kind).collect();

        // Dependencies and their platform qualifiers.
        let (mut dependencies, mut dev_dependencies, mut target_dependencies, dep_notes) =
            read_dependencies(module, self.catalog.as_ref(), &versions, dir, locals, &aliases, &enabled);
        for note in dep_notes {
            self.manual(format!("`{}`: {note}", display_dir(dir)));
        }
        dependencies.extend(settings.extra_dependencies.iter().cloned());

        // Frameworks.
        let detected = frameworks::detect(&mut dependencies, &mut dev_dependencies, &settings.hints);
        for line in &detected.translated {
            self.imported.translated(format!("{name}: {line}"));
        }
        for deps in std::iter::once(&mut dependencies).chain(std::iter::once(&mut dev_dependencies)).chain(target_dependencies.values_mut())
        {
            deps.retain(|d| !frameworks::is_built_in(d));
        }
        let mut frameworks = detected.frameworks;
        if let Some(serialization) = frameworks.serialization.as_mut() {
            if !settings.serialization_formats.is_empty() {
                serialization.formats = settings.serialization_formats.clone();
            }
            if serialization.version.is_none() {
                serialization.version = settings.serialization_version.clone();
            }
        }

        // Compiler plugins the settings ask for, on top of the frameworks.
        let mut plugins = settings.plugins.clone();
        unique_plugin_names(&mut plugins);
        for deps in std::iter::once(&mut dependencies).chain(std::iter::once(&mut dev_dependencies)).chain(target_dependencies.values_mut())
        {
            deps.retain(|d| !frameworks::is_built_in(d));
        }

        let repositories = read_repositories(module, &mut self.imported);
        package.kotlin = settings.kotlin;
        package.jvm_target = settings.jvm_target.and_then(|t| java_target(&t));
        package.jdk = settings.jdk;
        package.main_class = settings.main_class;
        package.javac_args = settings.javac_args;
        package.dependencies = dependencies;
        package.dev_dependencies = dev_dependencies;
        package.target_dependencies = target_dependencies;
        package.frameworks = frameworks;
        package.plugins = plugins;
        package.repositories = repositories;
        for (key, value) in settings.android {
            if let Some(target) = targets.iter_mut().find(|t| t.kind == TargetKind::Android) {
                target.set(&key, value);
            }
        }
        if let Some(entry) = &settings.native_entry
            && product.product != Product::IosApp
            && let Some(target) = targets.iter_mut().find(|t| t.kind == TargetKind::Native)
        {
            target.set("entry-point", Value::str(entry));
        }
        package.targets = targets;

        // Sources.
        let (moves, move_notes) = source_moves(&self.root, dir, module, &aliases, &enabled, &product);
        for note in move_notes {
            self.manual(format!("`{}`: {note}", display_dir(dir)));
        }
        package.moves = moves;

        self.imported.translated(format!(
            "{name}: {} dependencies, Kotlin {}",
            package.dependencies.len() + package.dev_dependencies.len() + package.target_dependencies.values().map(Vec::len).sum::<usize>(),
            package.kotlin.version.as_deref().unwrap_or("(default)")
        ));
        Ok(Some(package))
    }
}

/// Reads a YAML file, if it exists.
fn read_yaml(path: &Path) -> Result<Option<Yaml>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Yaml = serde_yaml_ng::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(value))
}

/// A directory of the project, with forward slashes; `.` for the root.
fn display_dir(dir: &Path) -> String {
    let text = dir.to_string_lossy().replace('\\', "/");
    if text.is_empty() { ".".to_owned() } else { text }
}

fn at<'a>(map: &'a Mapping, key: &str) -> Option<&'a Yaml> {
    map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

fn text(value: &Yaml) -> Option<String> {
    match value {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn boolean(value: &Yaml) -> Option<bool> {
    match value {
        Yaml::Bool(b) => Some(*b),
        Yaml::String(s) if s == "true" || s == "enabled" => Some(true),
        Yaml::String(s) if s == "false" || s == "disabled" => Some(false),
        _ => None,
    }
}

fn strings(value: &Yaml) -> Vec<String> {
    match value {
        Yaml::Sequence(items) => items.iter().filter_map(text).collect(),
        other => text(other).into_iter().collect(),
    }
}

/// The top-level keys this importer understands.
fn known_key(key: &str) -> bool {
    matches!(key, "product" | "description" | "layout" | "aliases" | "repositories")
        || key == "dependencies"
        || key.starts_with("dependencies@")
        || key == "test-dependencies"
        || key.starts_with("test-dependencies@")
        || key == "settings"
        || key.starts_with("settings@")
        || key == "test-settings"
        || key.starts_with("test-settings@")
}

fn unknown_keys(map: &Mapping) -> Vec<String> {
    let mut keys: Vec<String> = map.keys().filter_map(|k| k.as_str()).filter(|k| !known_key(k)).map(str::to_owned).collect();
    keys.sort();
    keys.dedup();
    keys
}

fn project_notes(project: &Yaml) -> Vec<String> {
    let Some(map) = project.as_mapping() else { return Vec::new() };
    let mut notes = Vec::new();
    for (key, _) in map {
        match key.as_str() {
            Some("modules") => {}
            Some("plugins") => notes.push(
                "project.yaml lists build plugins; Kargo has no build plugin system, so recreate them as `kargo-<name>` subcommands or `[tasks]`"
                    .to_owned(),
            ),
            Some(other) => notes.push(format!("project.yaml: the key `{other}` has no Kargo equivalent")),
            None => {}
        }
    }
    notes
}

/// The module directories of `project.yaml`, expanding `*` and `?` and `{a,b}` in the paths.
fn module_dirs(root: &Path, project: Option<&Yaml>) -> (Vec<PathBuf>, Vec<String>) {
    let mut notes = Vec::new();
    let mut dirs = Vec::new();
    if root.join("module.yaml").is_file() {
        dirs.push(PathBuf::new());
    }
    let Some(project) = project.and_then(Yaml::as_mapping) else { return (dirs, notes) };
    let modules = at(project, "modules").map(strings).unwrap_or_default();
    for module in modules {
        let module = module.trim().trim_start_matches("./").trim_end_matches('/');
        if module.is_empty() {
            if !dirs.iter().any(|d| d.as_os_str().is_empty()) {
                dirs.push(PathBuf::new());
            }
            continue;
        }
        let alternatives = expand_braces(module);
        let mut matched = false;
        for pattern in alternatives {
            if pattern.contains(['*', '?', '[']) {
                for entry in WalkDir::new(root).max_depth(8).sort_by_file_name() {
                    let Ok(entry) = entry else { continue };
                    if !entry.file_type().is_dir() || entry.path() == root {
                        continue;
                    }
                    if !entry.path().join("module.yaml").is_file() {
                        continue;
                    }
                    let Ok(relative) = entry.path().strip_prefix(root) else { continue };
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    if glob_match(&pattern, &relative) {
                        dirs.push(PathBuf::from(relative));
                        matched = true;
                    }
                }
            } else {
                dirs.push(PathBuf::from(&pattern));
                matched = true;
            }
        }
        if !matched {
            notes.push(format!("project.yaml lists the module `{module}`, which matches no directory with a module.yaml"));
        }
    }
    (dirs, notes)
}

/// `{app,lib}` and `{a,b}/c` become the alternatives of a module path.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else { return vec![pattern.to_owned()] };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else { return vec![pattern.to_owned()] };
    let before = &pattern[..open];
    let after = &pattern[close + 1..];
    pattern[open + 1..close].split(',').flat_map(|choice| expand_braces(&format!("{before}{choice}{after}"))).collect()
}

/// A small glob matcher: `*` and `?` do not cross a `/`.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    match_at(&pattern, &path)
}

fn match_at(pattern: &[char], path: &[char]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some('*') => {
            let mut rest = pattern;
            while rest.first() == Some(&'*') {
                rest = &rest[1..];
            }
            if rest.is_empty() {
                return !path.contains(&'/');
            }
            for at in 0..=path.len() {
                if at > 0 && path[at - 1] == '/' {
                    break;
                }
                if match_at(rest, &path[at..]) {
                    return true;
                }
            }
            false
        }
        Some('?') => !path.is_empty() && path[0] != '/' && match_at(&pattern[1..], &path[1..]),
        Some(expected) => !path.is_empty() && path[0] == *expected && match_at(&pattern[1..], &path[1..]),
    }
}

fn product_of(module: &Mapping) -> Option<ProductDef> {
    let product = at(module, "product")?;
    let (kind, platforms) = match product {
        Yaml::String(kind) => (kind.clone(), Vec::new()),
        Yaml::Mapping(map) => {
            let kind = at(map, "type").and_then(text)?;
            (kind, at(map, "platforms").map(strings).unwrap_or_default())
        }
        _ => return None,
    };
    let product = match kind.as_str() {
        "jvm/app" => Product::JvmApp,
        "jvm/lib" => Product::JvmLib,
        "android/app" => Product::AndroidApp,
        "ios/app" => Product::IosApp,
        "linux/app" => Product::LinuxApp,
        "macos/app" => Product::MacosApp,
        "windows/app" => Product::WindowsApp,
        "wasm-js/app" => Product::WasmJsApp,
        "wasm-wasi/app" => Product::WasmWasiApp,
        "js/app" => Product::JsApp,
        "kmp/lib" => Product::KmpLib,
        "jvm/amper-plugin" => Product::Plugin,
        _ => return None,
    };
    let platforms = if platforms.is_empty() {
        match product {
            Product::JvmApp | Product::JvmLib | Product::Plugin => vec!["jvm".to_owned()],
            Product::AndroidApp => vec!["android".to_owned()],
            Product::IosApp => vec!["iosArm64".to_owned(), "iosSimulatorArm64".to_owned()],
            Product::LinuxApp => vec!["linuxX64".to_owned(), "linuxArm64".to_owned()],
            Product::MacosApp => vec!["macosArm64".to_owned()],
            Product::WindowsApp => vec!["mingwX64".to_owned()],
            Product::WasmJsApp => vec!["wasmJs".to_owned()],
            Product::WasmWasiApp => vec!["wasmWasi".to_owned()],
            Product::JsApp => vec!["js".to_owned()],
            Product::KmpLib => Vec::new(),
        }
    } else {
        platforms
    };
    Some(ProductDef { product, platforms })
}

/// The `[target.<name>]` tables of a product, and what the report should say.
fn target_settings(product: &ProductDef, settings: &SettingsRead, name: &str) -> (Vec<TargetSettings>, Vec<String>) {
    let mut targets = Vec::new();
    let mut notes = Vec::new();
    let mut native: Vec<String> = Vec::new();
    let mut kinds: BTreeSet<TargetKind> = BTreeSet::new();
    for platform in &product.platforms {
        match platform.as_str() {
            "jvm" => {
                kinds.insert(TargetKind::Jvm);
            }
            "android" => {
                kinds.insert(TargetKind::Android);
            }
            "js" => {
                kinds.insert(TargetKind::Js);
            }
            "wasmJs" | "wasmWasi" => {
                kinds.insert(TargetKind::Wasm);
            }
            platform if is_native_platform(platform) => {
                kinds.insert(TargetKind::Native);
                if !native.iter().any(|p| p == platform) {
                    native.push(platform.to_owned());
                }
            }
            other => notes.push(format!("the platform `{other}` has no Kargo target")),
        }
    }
    for kind in kinds {
        let mut target = TargetSettings::new(kind);
        match kind {
            TargetKind::Jvm => {}
            TargetKind::Android => {
                let library = product.product != Product::AndroidApp;
                target.set("kind", Value::str(if library { "library" } else { "application" }));
            }
            TargetKind::Wasm => {
                let library = product.product == Product::KmpLib;
                if product.product == Product::WasmJsApp {
                    target.set("environment", Value::str("browser"));
                } else if product.product == Product::WasmWasiApp {
                    target.set("environment", Value::str("wasi"));
                }
                if library {
                    target.set("kind", Value::str("library"));
                }
            }
            TargetKind::Js => {
                if product.product == Product::KmpLib {
                    target.set("kind", Value::str("library"));
                }
            }
            TargetKind::Native => {
                let kind = match product.product {
                    Product::IosApp => {
                        notes.push(
                            "an iOS app is a Kotlin framework plus Swift and an Xcode project; Kargo builds the framework, and the Swift part needs Xcode"
                                .to_owned(),
                        );
                        "framework"
                    }
                    Product::KmpLib => "library",
                    _ => "executable",
                };
                if !native.is_empty() {
                    target.set("platforms", Value::strings(&native));
                }
                target.set("kind", Value::str(kind));
            }
        }
        targets.push(target);
    }
    if targets.is_empty() {
        notes.push(format!("`{name}` has no platform Kargo can build"));
    }
    // Android needs a namespace; the toolchain has a default, Kargo does not.
    if let Some(android) = targets.iter_mut().find(|t| t.kind == TargetKind::Android)
        && !settings.android.iter().any(|(k, _)| k == "namespace")
    {
        let fallback = settings
            .android
            .iter()
            .find(|(k, _)| k == "application-id")
            .and_then(|(_, value)| match value {
                Value::Str(id) => Some(id.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                let group = settings.group.as_deref().unwrap_or("com.example");
                format!("{group}.{}", name.replace(['-', '.'], "_"))
            });
        notes.push(format!("`{name}` has no Android namespace; it is `{fallback}`, change it to the one of your manifest"));
        android.set("namespace", Value::str(fallback));
    }
    (targets, notes)
}

fn is_native_platform(platform: &str) -> bool {
    matches!(
        platform,
        "linuxX64"
            | "linuxArm64"
            | "mingwX64"
            | "macosX64"
            | "macosArm64"
            | "iosArm64"
            | "iosX64"
            | "iosSimulatorArm64"
            | "watchosArm32"
            | "watchosArm64"
            | "watchosDeviceArm64"
            | "watchosSimulatorArm64"
            | "tvosArm64"
            | "tvosSimulatorArm64"
            | "tvosX64"
            | "androidNativeArm32"
            | "androidNativeArm64"
            | "androidNativeX64"
            | "androidNativeX86"
    )
}

/// The `aliases` of a module: an alias name and the platforms it stands for.
fn aliases_of(module: &Mapping) -> HashMap<String, Vec<String>> {
    let mut aliases = HashMap::new();
    let Some(items) = at(module, "aliases").and_then(Yaml::as_sequence) else { return aliases };
    for item in items {
        let Some(map) = item.as_mapping() else { continue };
        for (name, platforms) in map {
            if let Some(name) = name.as_str() {
                aliases.insert(name.to_owned(), strings(platforms));
            }
        }
    }
    aliases
}

fn module_has_platform_settings(module: &Mapping) -> bool {
    module.keys().filter_map(|k| k.as_str()).any(|k| k.starts_with("settings@"))
}

/// The platform kinds a `@` qualifier (a platform, a family or an alias) covers.
fn qualifier_kinds(qualifier: &str, aliases: &HashMap<String, Vec<String>>, enabled: &BTreeSet<TargetKind>) -> BTreeSet<TargetKind> {
    let mut kinds = BTreeSet::new();
    if let Some(platforms) = aliases.get(qualifier) {
        for platform in platforms {
            kinds.extend(qualifier_kinds(platform, aliases, enabled));
        }
        return kinds;
    }
    let candidates: &[TargetKind] = match qualifier {
        "common" => &[],
        "jvm" => &[TargetKind::Jvm],
        "android" => &[TargetKind::Android],
        "js" => &[TargetKind::Js],
        "wasmJs" | "wasmWasi" => &[TargetKind::Wasm],
        "web" => &[TargetKind::Js, TargetKind::Wasm],
        "native" | "apple" | "ios" | "linux" | "macos" | "mingw" | "watchos" | "tvos" | "androidNative" => &[TargetKind::Native],
        platform if is_native_platform(platform) => &[TargetKind::Native],
        _ => &[],
    };
    for kind in candidates {
        if enabled.contains(kind) {
            kinds.insert(*kind);
        }
    }
    kinds
}

/// The dependencies of one module: main, dev, per target, and what could not be read.
type DependencyRead = (Vec<Dependency>, Vec<Dependency>, BTreeMap<TargetKind, Vec<Dependency>>, Vec<String>);

/// The `[dependencies]`, `[dev-dependencies]` and per-target tables of a module.
#[allow(clippy::too_many_arguments)]
fn read_dependencies(
    module: &Mapping,
    catalog: Option<&Catalog>,
    versions: &CatalogVersions,
    dir: &Path,
    locals: &HashMap<PathBuf, LocalPackage>,
    aliases: &HashMap<String, Vec<String>>,
    enabled: &BTreeSet<TargetKind>,
) -> DependencyRead {
    let mut dependencies = Vec::new();
    let mut dev_dependencies = Vec::new();
    let mut target_dependencies: BTreeMap<TargetKind, Vec<Dependency>> = BTreeMap::new();
    let mut notes = Vec::new();
    for (key, value) in module {
        let Some(key) = key.as_str() else { continue };
        let (base, qualifier) = match key.split_once('@') {
            Some((base, qualifier)) => (base, Some(qualifier)),
            None => (key, None),
        };
        let test = match base {
            "dependencies" => false,
            "test-dependencies" => true,
            _ => continue,
        };
        let Some(items) = value.as_sequence() else {
            notes.push(format!("`{key}` is not a list"));
            continue;
        };
        let kinds = match qualifier {
            None | Some("common") => BTreeSet::new(),
            Some(qualifier) => qualifier_kinds(qualifier, aliases, enabled),
        };
        if qualifier.is_some() && kinds.is_empty() {
            notes.push(format!("`{key}` names a platform Kargo has no target for; its dependencies were left out"));
            continue;
        }
        for item in items {
            let Some(dep) = parse_dependency(item, catalog, versions, dir, locals, &mut notes) else { continue };
            if test {
                dev_dependencies.push(dep);
            } else if kinds.is_empty() {
                dependencies.push(dep);
            } else {
                for kind in &kinds {
                    target_dependencies.entry(*kind).or_default().push(dep.clone());
                }
            }
        }
    }
    (dependencies, dev_dependencies, target_dependencies, notes)
}

/// One item of a dependency list.
fn parse_dependency(
    item: &Yaml,
    catalog: Option<&Catalog>,
    versions: &CatalogVersions,
    dir: &Path,
    locals: &HashMap<PathBuf, LocalPackage>,
    notes: &mut Vec<String>,
) -> Option<Dependency> {
    // A list item is a notation string, `notation: attributes`, or `{ bom: ... }` / `{ swiftPackage: ... }`.
    match item {
        Yaml::String(notation) => {
            let (notation, attributes) = split_attributes(notation);
            let mut dep = notation_dependency(&notation, catalog, versions, dir, locals, notes)?;
            apply_string_attributes(&mut dep, &attributes);
            Some(dep)
        }
        Yaml::Mapping(map) => {
            if let Some(bom) = at(map, "bom") {
                let (notation, _) = split_attributes(&text(bom)?);
                let mut dep = notation_dependency(&notation, catalog, versions, dir, locals, notes)?;
                dep.bom = true;
                return Some(dep);
            }
            for unsupported in ["swiftPackage", "localSwiftPackage", "pod", "cocoapods"] {
                if at(map, unsupported).is_some() {
                    notes.push(format!("`{unsupported}` dependencies have no Kargo equivalent"));
                    return None;
                }
            }
            let (notation, attributes) = map.iter().next()?;
            let notation = notation.as_str()?.trim_end_matches(':');
            let (notation, inline) = split_attributes(notation);
            let mut dep = notation_dependency(&notation, catalog, versions, dir, locals, notes)?;
            apply_string_attributes(&mut dep, &inline);
            match attributes {
                Yaml::Mapping(attributes) => apply_mapping_attributes(&mut dep, attributes),
                other => {
                    if let Some(attribute) = text(other) {
                        apply_string_attributes(&mut dep, &[attribute]);
                    }
                }
            }
            Some(dep)
        }
        Yaml::Null => None,
        _ => {
            notes.push("a dependency is neither a notation nor a mapping".to_owned());
            None
        }
    }
}

/// `g:a:v: exported` -> the notation and the words after it.
fn split_attributes(notation: &str) -> (String, Vec<String>) {
    let mut words = notation.split_whitespace();
    let first = words.next().unwrap_or_default().to_owned();
    (first, words.map(str::to_owned).collect())
}

fn apply_string_attributes(dep: &mut Dependency, attributes: &[String]) {
    for attribute in attributes {
        match attribute.as_str() {
            "exported" => dep.exported = true,
            "compile-only" => dep.scope = Scope::Provided,
            "runtime-only" => dep.scope = Scope::Runtime,
            "all" => dep.scope = Scope::Compile,
            _ => {}
        }
    }
}

fn apply_mapping_attributes(dep: &mut Dependency, attributes: &Mapping) {
    if at(attributes, "exported").and_then(boolean).unwrap_or(false) {
        dep.exported = true;
    }
    if let Some(scope) = at(attributes, "scope").and_then(text) {
        dep.scope = match scope.as_str() {
            "compile-only" => Scope::Provided,
            "runtime-only" => Scope::Runtime,
            _ => Scope::Compile,
        };
    }
}

/// A notation: Maven coordinates, a module path, a catalog reference, or a BOM.
fn notation_dependency(
    notation: &str,
    catalog: Option<&Catalog>,
    versions: &CatalogVersions,
    dir: &Path,
    locals: &HashMap<PathBuf, LocalPackage>,
    notes: &mut Vec<String>,
) -> Option<Dependency> {
    let notation = notation.trim();
    if let Some(path) = notation.strip_prefix("//") {
        return module_dependency(path, dir, locals, notes);
    }
    if notation.starts_with("./") || notation.starts_with("../") {
        let path = normalize_path(&dir.join(notation));
        let found = locals.iter().find(|(module_dir, _)| normalize_path(module_dir) == path).map(|(_, local)| local);
        if let Some(local) = found {
            return Some(local_dependency(local, dir));
        }
        notes.push(format!("the module dependency `{notation}` points at no module of the project"));
        return None;
    }
    if let Some(reference) = notation.strip_prefix('$') {
        return catalog_dependency(reference, catalog, versions, notes);
    }
    if let Some(bom) = notation.strip_prefix("bom:") {
        let mut dep = coordinates(bom.trim(), notes)?;
        dep.bom = true;
        return Some(dep);
    }
    coordinates(notation, notes)
}

fn module_dependency(path: &str, dir: &Path, locals: &HashMap<PathBuf, LocalPackage>, notes: &mut Vec<String>) -> Option<Dependency> {
    let target = PathBuf::from(path.trim_start_matches('/'));
    let Some(local) = locals.get(&target) else {
        notes.push(format!("the module dependency `//{path}` points at no module of the project"));
        return None;
    };
    Some(local_dependency(local, dir))
}

fn local_dependency(local: &LocalPackage, dir: &Path) -> Dependency {
    let mut dep = Dependency::new("", &local.name, None);
    dep.path = Some(LocalPackage { name: local.name.clone(), dir: relative_dir(dir, &local.dir) });
    dep
}

/// `group:artifact[:version[:classifier]][@packaging]`.
fn coordinates(notation: &str, notes: &mut Vec<String>) -> Option<Dependency> {
    let notation = notation.trim().trim_end_matches(':');
    let (coordinates, extension) = match notation.rsplit_once('@') {
        Some((coordinates, extension)) => (coordinates, Some(extension.to_owned())),
        None => (notation, None),
    };
    let parts: Vec<&str> = coordinates.split(':').collect();
    if parts.len() < 2 || parts.len() > 4 || parts.iter().take(2).any(|part| part.is_empty()) {
        notes.push(format!("`{notation}` is not a Maven coordinate (group:artifact:version)"));
        return None;
    }
    let version = parts.get(2).filter(|v| !v.is_empty()).map(|v| (*v).to_owned());
    let mut dep = Dependency::new(parts[0], parts[1], version);
    dep.classifier = parts.get(3).map(|c| (*c).to_owned());
    dep.extension = extension.filter(|e| e != "jar");
    Some(dep)
}

/// A `$libs`, `$compose`, `$kotlin`, `$ktor`, `$springBoot` or `$lombok` reference.
fn catalog_dependency(
    reference: &str,
    catalog: Option<&Catalog>,
    versions: &CatalogVersions,
    notes: &mut Vec<String>,
) -> Option<Dependency> {
    let (catalog_name, key) = reference.split_once('.')?;
    let key = key.trim();
    if key.is_empty() {
        notes.push(format!("`${reference}` names no library"));
        return None;
    }
    let library: Option<Library> = match catalog_name {
        "libs" => {
            let Some(catalog) = catalog else {
                notes.push(format!("`${reference}` uses the project catalog, but the project has no libs.versions.toml"));
                return None;
            };
            catalog.libraries.get(&normalize_alias(key)).cloned()
        }
        "kotlin" => kotlin_catalog(key, versions),
        "compose" => compose_catalog(key, versions),
        "ktor" => Some(Library {
            group: "io.ktor".to_owned(),
            artifact: format!("ktor-{}", key.replace('.', "-")),
            version: versions.ktor.clone(),
        }),
        "springBoot" | "spring" => Some(Library {
            group: "org.springframework.boot".to_owned(),
            artifact: format!("spring-boot-starter-{}", key.replace('.', "-")),
            version: versions.spring_boot.clone(),
        }),
        "lombok" => Some(Library { group: "org.projectlombok".to_owned(), artifact: "lombok".to_owned(), version: None }),
        other => {
            notes.push(format!("the toolchain catalog `${other}` has no Kargo equivalent"));
            return None;
        }
    };
    let Some(library) = library else {
        notes.push(format!("`${reference}` is not in the catalog"));
        return None;
    };
    Some(Dependency::new(library.group, library.artifact, library.version))
}

fn kotlin_catalog(key: &str, versions: &CatalogVersions) -> Option<Library> {
    let (group, artifact) = if key == "reflect" {
        ("org.jetbrains.kotlin", "kotlin-reflect".to_owned())
    } else if key == "test" || key.starts_with("test.") || key == "stdlib" {
        // Kargo adds the standard library and kotlin-test by itself.
        return None;
    } else if let Some(format) = key.strip_prefix("serialization.") {
        ("org.jetbrains.kotlinx", format!("kotlinx-serialization-{format}"))
    } else {
        ("org.jetbrains.kotlin", format!("kotlin-{}", key.replace('.', "-")))
    };
    Some(Library {
        group: group.to_owned(),
        artifact,
        version: versions.serialization.clone().filter(|_| key.starts_with("serialization.")),
    })
}

fn compose_catalog(key: &str, versions: &CatalogVersions) -> Option<Library> {
    if versions.compose_enabled || key.starts_with("desktop") {
        // Compose Desktop, and everything else, is what `[frameworks] compose` adds.
        return None;
    }
    let (group, artifact) = if key == "components.resources" {
        ("org.jetbrains.compose.components".to_owned(), "components-resources".to_owned())
    } else if key == "components.uiToolingPreview" {
        ("org.jetbrains.compose.components".to_owned(), "components-ui-tooling-preview".to_owned())
    } else if let Some((first, rest)) = key.split_once('.') {
        (format!("org.jetbrains.compose.{first}"), rest.replace('.', "-"))
    } else {
        (format!("org.jetbrains.compose.{key}"), key.to_owned())
    };
    Some(Library { group, artifact, version: versions.compose.clone() })
}

/// `dir/./x/../y` -> `dir/y`.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn catalog_versions(module: &Mapping) -> CatalogVersions {
    let settings = at(module, "settings").and_then(Yaml::as_mapping);
    let Some(settings) = settings else { return CatalogVersions::default() };
    let kotlin = at(settings, "kotlin").and_then(Yaml::as_mapping);
    let string_setting = |map: Option<&Mapping>, key: &str| map.and_then(|m| at(m, key)).and_then(text);
    let serialization = kotlin.and_then(|k| at(k, "serialization")).and_then(Yaml::as_mapping);
    CatalogVersions {
        compose: toggle_version(at(settings, "compose")),
        compose_enabled: enabled(at(settings, "compose")),
        ktor: toggle_version(at(settings, "ktor")).or_else(|| string_setting(kotlin, "ktor")),
        spring_boot: toggle_version(at(settings, "springBoot")),
        serialization: serialization.and_then(|map| at(map, "version")).and_then(text),
    }
}

/// `enabled` / `{ enabled: true, version: "1.0" }` -> the version, when there is one.
fn toggle_version(value: Option<&Yaml>) -> Option<String> {
    let value = value?;
    match value {
        Yaml::Mapping(map) => at(map, "version").and_then(text),
        _ => None,
    }
}

fn enabled(value: Option<&Yaml>) -> bool {
    let Some(value) = value else { return false };
    match value {
        Yaml::Bool(b) => *b,
        Yaml::String(s) => matches!(s.as_str(), "enabled" | "true" | "json" | "protobuf" | "cbor"),
        Yaml::Mapping(map) => map_enabled(map),
        _ => false,
    }
}

fn map_enabled(map: &Mapping) -> bool {
    at(map, "enabled").and_then(boolean).unwrap_or(false)
}

fn read_settings(
    module: &Mapping,
    _aliases: &HashMap<String, Vec<String>>,
    catalog: Option<&Catalog>,
    versions: &CatalogVersions,
) -> SettingsRead {
    let mut out = SettingsRead::default();
    let mut merged = at(module, "settings").and_then(Yaml::as_mapping).cloned().unwrap_or_default();
    for (key, value) in module {
        if key.as_str().is_some_and(|key| key.starts_with("settings@"))
            && let Some(map) = value.as_mapping()
        {
            merge_mapping(&mut merged, map);
        }
    }
    let settings = &merged;
    let kotlin = at(settings, "kotlin").and_then(Yaml::as_mapping);
    if let Some(kotlin) = kotlin {
        out.kotlin.version = at(kotlin, "version").and_then(text);
        out.kotlin.language_version = at(kotlin, "languageVersion").and_then(text);
        out.kotlin.api_version = at(kotlin, "apiVersion").and_then(text);
        out.kotlin.warnings_as_errors = at(kotlin, "allWarningsAsErrors").and_then(boolean).unwrap_or(false);
        if let Some(opt_ins) = at(kotlin, "optIns") {
            out.kotlin.opt_in.extend(strings(opt_ins));
        }
        if let Some(args) = at(kotlin, "freeCompilerArgs") {
            for arg in strings(args) {
                if let Some(opt_in) = arg.strip_prefix("-opt-in=") {
                    out.kotlin.opt_in.push(opt_in.to_owned());
                } else if arg == "-Werror" {
                    out.kotlin.warnings_as_errors = true;
                } else {
                    out.kotlin.compiler_args.push(arg);
                }
            }
        }
        if let Some(explicit) = at(kotlin, "explicitApi").and_then(text)
            && matches!(explicit.as_str(), "strict" | "warning")
        {
            out.kotlin.compiler_args.push(format!("-Xexplicit-api={explicit}"));
        }
        if at(kotlin, "progressiveMode").and_then(boolean).unwrap_or(false) {
            out.kotlin.compiler_args.push("-progressive".to_owned());
        }
        if at(kotlin, "suppressWarnings").and_then(boolean).unwrap_or(false) {
            out.kotlin.compiler_args.push("-nowarn".to_owned());
        }
        if let Some(ksp) = at(kotlin, "ksp").and_then(Yaml::as_mapping) {
            out.kotlin.ksp = at(ksp, "version").and_then(text);
            if let Some(processors) = at(ksp, "processors") {
                for processor in strings(processors) {
                    if let Some(mut dep) =
                        notation_dependency(&processor, catalog, versions, Path::new(""), &HashMap::new(), &mut out.notes)
                    {
                        dep.scope = Scope::Ksp;
                        out.extra_dependencies.push(dep);
                    }
                }
            }
            if at(ksp, "processorOptions").is_some() {
                out.notes.push("KSP `processorOptions` have no equivalent in `kargo.toml`".to_owned());
            }
        }
        if let Some(serialization) = at(kotlin, "serialization") {
            out.hints.serialization_plugin = true;
            out.serialization_version = serialization.as_mapping().and_then(|map| at(map, "version")).and_then(text);
            if let Some(format) = serialization.as_str().filter(|f| *f != "enabled") {
                out.serialization_formats.push(format.to_owned());
            }
            if let Some(format) = serialization.as_mapping().and_then(|map| at(map, "format")).and_then(text) {
                out.serialization_formats.push(format);
            }
        }
        if let Some(all_open) = at(kotlin, "allOpen").and_then(Yaml::as_mapping)
            && map_enabled(all_open)
        {
            let annotations = at(all_open, "annotations").map(strings).unwrap_or_default();
            let presets = at(all_open, "presets").map(strings).unwrap_or_default();
            if !annotations.is_empty() || presets.is_empty() {
                out.plugins.extend(compiler_plugins(
                    "allopen",
                    "org.jetbrains.kotlin.allopen",
                    None,
                    &annotations,
                    out.kotlin.version.as_deref(),
                ));
            }
            for preset in presets {
                out.plugins.extend(compiler_plugins(
                    "allopen",
                    "org.jetbrains.kotlin.allopen",
                    Some(&preset),
                    &[],
                    out.kotlin.version.as_deref(),
                ));
            }
        }
        if let Some(no_arg) = at(kotlin, "noArg").and_then(Yaml::as_mapping)
            && map_enabled(no_arg)
        {
            let annotations = at(no_arg, "annotations").map(strings).unwrap_or_default();
            let presets = at(no_arg, "presets").map(strings).unwrap_or_default();
            if !annotations.is_empty() || presets.is_empty() {
                out.plugins.extend(compiler_plugins(
                    "noarg",
                    "org.jetbrains.kotlin.noarg",
                    None,
                    &annotations,
                    out.kotlin.version.as_deref(),
                ));
            }
            for preset in presets {
                out.plugins.extend(compiler_plugins(
                    "noarg",
                    "org.jetbrains.kotlin.noarg",
                    Some(&preset),
                    &[],
                    out.kotlin.version.as_deref(),
                ));
            }
        }
        if let Some(compiler_plugins) = at(kotlin, "compilerPlugins").and_then(Yaml::as_sequence) {
            for plugin in compiler_plugins {
                let Some(map) = plugin.as_mapping() else { continue };
                let Some(id) = at(map, "id").and_then(text) else { continue };
                let Some(dependency) = at(map, "dependency").and_then(text) else { continue };
                let options = at(map, "options")
                    .and_then(Yaml::as_mapping)
                    .map(|map| map.iter().filter_map(|(k, v)| Some((k.as_str()?.to_owned(), text(v)?))).collect())
                    .unwrap_or_default();
                let name = id.rsplit('.').next().unwrap_or(&id).replace(['.', '/'], "-");
                let maven = if dependency.starts_with('$') {
                    match notation_dependency(&dependency, catalog, versions, Path::new(""), &HashMap::new(), &mut out.notes) {
                        Some(dep) if dep.version.is_some() => {
                            format!("{}:{}:{}", dep.group, dep.artifact, dep.version.clone().unwrap_or_default())
                        }
                        _ => dependency.clone(),
                    }
                } else {
                    dependency.clone()
                };
                out.plugins.push(CompilerPlugin { name, maven, id, options });
            }
        }
        for (key, label) in
            [("powerAssert", "power-assert"), ("dataframe", "dataframe"), ("rpc", "kotlinx.rpc"), ("jsPlainObjects", "JS plain objects")]
        {
            if enabled(at(kotlin, key)) {
                out.notes.push(format!("the `{label}` compiler plugin has no Kargo equivalent"));
            }
        }
    }

    if let Some(jvm) = at(settings, "jvm").and_then(Yaml::as_mapping) {
        out.jvm_target = at(jvm, "release").and_then(text);
        out.main_class = at(jvm, "mainClass").and_then(text);
        if let Some(jdk) = at(jvm, "jdk").and_then(Yaml::as_mapping) {
            out.jdk = at(jdk, "version").and_then(text);
        }
        if at(jvm, "runtimeClasspathMode").is_some() || at(jvm, "storeParameterNames").is_some() {
            out.notes.push("the JVM `runtimeClasspathMode` and `storeParameterNames` settings have no equivalent".to_owned());
        }
        if let Some(test) = at(jvm, "test").and_then(Yaml::as_mapping)
            && !test.is_empty()
        {
            out.notes.push("`settings.jvm.test` (JVM arguments, environment and system properties of tests) has no equivalent".to_owned());
        }
    }

    if let Some(java) = at(settings, "java").and_then(Yaml::as_mapping) {
        if let Some(args) = at(java, "freeCompilerArgs") {
            out.javac_args.extend(strings(args));
        }
        if let Some(processing) = at(java, "annotationProcessing").and_then(Yaml::as_mapping) {
            if let Some(processors) = at(processing, "processors") {
                for processor in strings(processors) {
                    if let Some(mut dep) =
                        notation_dependency(&processor, catalog, versions, Path::new(""), &HashMap::new(), &mut out.notes)
                    {
                        dep.scope = Scope::Processor;
                        out.extra_dependencies.push(dep);
                    }
                }
            }
            if at(processing, "processorOptions").is_some() {
                out.notes.push("annotation processor options have no equivalent in `kargo.toml`".to_owned());
            }
        }
    }

    if let Some(android) = at(settings, "android").and_then(Yaml::as_mapping) {
        copy_text(android, "namespace", "namespace", &mut out.android);
        copy_text(android, "applicationId", "application-id", &mut out.android);
        copy_number(android, "minSdk", "min-sdk", &mut out.android);
        copy_number(android, "targetSdk", "target-sdk", &mut out.android);
        copy_number(android, "versionCode", "version-code", &mut out.android);
        copy_text(android, "versionName", "version-name", &mut out.android);
        if let Some(compile) = at(android, "compileSdk") {
            let sdk = match compile {
                Yaml::Mapping(map) => at(map, "apiLevel").and_then(number),
                other => number(other),
            };
            if let Some(sdk) = sdk {
                out.android.push(("compile-sdk".to_owned(), Value::Int(sdk)));
            }
        }
        for key in ["resourcePackaging", "signing", "parcelize", "abiFilters", "buildToolsVersion"] {
            if at(android, key).is_some() {
                out.notes.push(format!("`settings.android.{key}` has no Kargo equivalent"));
            }
        }
    }

    if let Some(native) = at(settings, "native").and_then(Yaml::as_mapping) {
        out.native_entry = at(native, "entryPoint").and_then(text);
    }

    if let Some(publishing) = at(settings, "publishing").and_then(Yaml::as_mapping) {
        out.group = at(publishing, "group").and_then(text);
        out.version = at(publishing, "version").and_then(text);
        for (key, _) in publishing {
            if let Some(key) = key.as_str()
                && !matches!(key, "group" | "version" | "enabled")
            {
                out.notes.push(format!("`settings.publishing.{key}` has no Kargo equivalent"));
            }
        }
    }

    if let Some(junit) = at(settings, "junit").and_then(text)
        && junit != "junit-5"
    {
        out.notes.push(format!("`settings.junit` is `{junit}`; `kargo test` runs JUnit 6 (JUnit 5 tests run unchanged)"));
    }

    if enabled(at(settings, "compose")) {
        out.hints.compose = Some(toggle_version(at(settings, "compose")));
    }
    if enabled(at(settings, "ktor")) {
        out.hints.ktor = Some(toggle_version(at(settings, "ktor")));
    }
    if enabled(at(settings, "springBoot")) {
        out.hints.spring_boot = Some(toggle_version(at(settings, "springBoot")));
    }
    if enabled(at(settings, "lombok")) {
        out.hints.lombok = true;
    }
    if at(settings, "serialization").is_some() {
        out.hints.serialization_plugin = true;
    }
    if let Some(test_settings) = at(module, "test-settings") {
        let _ = test_settings;
        out.notes.push("`test-settings` has no Kargo equivalent; tests run with JUnit 6".to_owned());
    }
    out
}

fn number(value: &Yaml) -> Option<i64> {
    match value {
        Yaml::Number(n) => n.as_i64().or_else(|| n.as_u64().and_then(|v| i64::try_from(v).ok())),
        Yaml::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Deep merge of `settings@...` into `settings`: mappings merge, lists append, scalars replace.
fn merge_mapping(target: &mut Mapping, source: &Mapping) {
    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(Yaml::Mapping(target)), Yaml::Mapping(source)) => merge_mapping(target, source),
            (Some(Yaml::Sequence(target)), Yaml::Sequence(source)) => target.extend(source.iter().cloned()),
            (Some(slot), value) => *slot = value.clone(),
            (None, value) => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn copy_text(from: &Mapping, key: &str, renamed: &str, to: &mut Vec<(String, Value)>) {
    copy_key(from, key, renamed, to, Value::str);
}

fn copy_number(from: &Mapping, key: &str, renamed: &str, to: &mut Vec<(String, Value)>) {
    if let Some(value) = at(from, key).and_then(number) {
        to.push((renamed.to_owned(), Value::Int(value)));
    }
}

fn copy_key(from: &Mapping, key: &str, renamed: &str, to: &mut Vec<(String, Value)>, wrap: impl Fn(String) -> Value) {
    if let Some(value) = at(from, key).and_then(text) {
        to.push((renamed.to_owned(), wrap(value)));
    }
}

/// Compiler plugin rows for `[plugins.<name>]`.
fn compiler_plugins(
    name: &str,
    id: &str,
    preset: Option<&str>,
    annotations: &[String],
    kotlin_version: Option<&str>,
) -> Vec<CompilerPlugin> {
    let version = kotlin_version.unwrap_or(DEFAULT_KOTLIN_VERSION);
    let maven = format!("org.jetbrains.kotlin:kotlin-{name}-compiler-plugin-embeddable:{version}");
    let mut options: Vec<(String, String)> = preset.map(|p| vec![("preset".to_owned(), p.to_owned())]).unwrap_or_default();
    options.extend(annotations.iter().map(|a| ("annotation".to_owned(), a.clone())));
    options
        .into_iter()
        .map(|option| CompilerPlugin { name: name.to_owned(), maven: maven.clone(), id: id.to_owned(), options: vec![option] })
        .collect()
}

fn unique_plugin_names(plugins: &mut [CompilerPlugin]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for plugin in plugins {
        let count = counts.entry(plugin.name.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            plugin.name = format!("{}-{count}", plugin.name);
        }
    }
}

fn read_repositories(module: &Mapping, imported: &mut Imported) -> Vec<Repository> {
    let mut repositories = Vec::new();
    let Some(items) = at(module, "repositories").and_then(Yaml::as_sequence) else { return repositories };
    for item in items {
        let (url, id, resolve, credentials) = match item {
            Yaml::String(value) if value == "mavenLocal" => ("maven-local".to_owned(), None, true, false),
            Yaml::String(value) => (value.clone(), None, true, false),
            Yaml::Mapping(map) => (
                at(map, "url").and_then(text).unwrap_or_default(),
                at(map, "id").and_then(text),
                at(map, "resolve").and_then(boolean).unwrap_or(true),
                at(map, "credentials").is_some(),
            ),
            _ => continue,
        };
        if url.is_empty() {
            continue;
        }
        if !resolve {
            imported.manual(
                "a Kotlin Toolchain repository has `resolve: false`; Kargo has no per-repository switch, so it was left out",
                MIGRATION,
            );
            continue;
        }
        if credentials {
            imported.manual(
                "repository credentials are not copied; name their environment variables in `username-env` and `password-env`",
                MIGRATION,
            );
        }
        if is_standard_repository(&url) || (url == "maven-local" && repositories.iter().any(|r: &Repository| r.url == "maven-local")) {
            continue;
        }
        let name = id.unwrap_or_else(|| repository_name(&url, repositories.len()));
        if repositories.iter().any(|r: &Repository| r.name == name) {
            continue;
        }
        repositories.push(Repository { name, url });
    }
    repositories
}

/// The source directories that move into Kargo's layout.
fn source_moves(
    root: &Path,
    dir: &Path,
    module: &Mapping,
    aliases: &HashMap<String, Vec<String>>,
    enabled: &BTreeSet<TargetKind>,
    product: &ProductDef,
) -> (Vec<DirMove>, Vec<String>) {
    let mut moves = Vec::new();
    let mut notes = Vec::new();
    let base = root.join(dir);
    let maven_like = at(module, "layout").and_then(text).as_deref() == Some("maven-like");
    if maven_like {
        for (from, to) in [
            ("src/main/kotlin", "src"),
            ("src/main/java", "src"),
            ("src/main/resources", "resources"),
            ("src/test/kotlin", "tests"),
            ("src/test/java", "tests"),
            ("src/test/resources", "test-resources"),
        ] {
            moves.push(DirMove { from: dir.join(from), to: dir.join(to) });
        }
    } else {
        moves.push(DirMove { from: dir.join("test"), to: dir.join("tests") });
        moves.push(DirMove { from: dir.join("testResources"), to: dir.join("test-resources") });
    }

    if maven_like {
        return (moves, notes);
    }

    let Ok(entries) = std::fs::read_dir(&base) else { return (moves, notes) };
    let mut names: Vec<String> =
        entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for name in names {
        let Some((prefix, qualifier)) = name.split_once('@') else { continue };
        let test = match prefix {
            "src" => false,
            "test" => true,
            "resources" | "testResources" => {
                notes.push(format!("`{name}` is a platform-specific resource directory, which Kargo has no place for"));
                continue;
            }
            _ => continue,
        };
        let kinds = qualifier_kinds(qualifier, aliases, enabled);
        if qualifier == "common" {
            let to = if test { "tests" } else { "src" };
            moves.push(DirMove { from: dir.join(&name), to: dir.join(to) });
            continue;
        }
        if kinds.is_empty() {
            notes.push(format!("the source directory `{name}` names a platform Kargo has no target for"));
            continue;
        }
        if kinds.len() > 1 {
            let list: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
            notes.push(format!(
                "`{name}` is shared by the {} targets, which Kargo has no source set for; it was not moved",
                list.join(" and ")
            ));
            continue;
        }
        let kind = *kinds.iter().next().expect("one kind");
        let to = if test { format!("tests-{}", kind.as_str()) } else { format!("src-{}", kind.as_str()) };
        moves.push(DirMove { from: dir.join(&name), to: dir.join(to) });
    }

    // Android takes its manifest and resources from `android/`, not from `src/`.
    if enabled.contains(&TargetKind::Android) {
        let android = base.join("src/AndroidManifest.xml");
        if android.is_file() {
            moves.push(DirMove { from: dir.join("src/AndroidManifest.xml"), to: dir.join("android/AndroidManifest.xml") });
        }
        for (from, to) in [("res", "android/res"), ("assets", "android/assets")] {
            if base.join(from).is_dir() {
                moves.push(DirMove { from: dir.join(from), to: dir.join(to) });
            }
        }
        if base.join("jniLibs").is_dir() {
            notes.push("`jniLibs` has no place in Kargo's layout".to_owned());
        }
    }
    // A product with only Android sources keeps them in `src` and `src-android`; everything else
    // needs no move of `src` itself.
    let _ = product;
    (moves, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn single_module(module: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "module.yaml", module);
        write(dir.path(), "src/Main.kt", "fun main() {}\n");
        write(dir.path(), "test/MainTest.kt", "class MainTest\n");
        dir
    }

    #[test]
    fn reads_a_jvm_application_with_a_catalog_and_frameworks() {
        let dir = single_module(
            r#"product: jvm/app

dependencies:
  - $libs.ktor.client
  - org.postgresql:postgresql:42.7.3: runtime-only
  - $compose.material3: exported

settings:
  kotlin:
    version: 2.4.10
    allWarningsAsErrors: true
    optIns: [ kotlin.time.ExperimentalTime ]
    serialization: json
  jvm:
    jdk:
      version: 21
    release: 17
    mainClass: com.example.MainKt
  compose: enabled

repositories:
  - https://jitpack.io
"#,
        );
        write(
            dir.path(),
            "libs.versions.toml",
            r#"[versions]
ktor = "3.6.0"

[libraries]
ktor-client = { module = "io.ktor:ktor-client-core", version.ref = "ktor" }
"#,
        );
        let imported = read(dir.path()).unwrap();
        assert!(!imported.workspace);
        let package = &imported.packages[0];
        assert!(!package.name.is_empty());
        assert_eq!(package.kotlin.version.as_deref(), Some("2.4.10"));
        assert!(package.kotlin.warnings_as_errors);
        assert_eq!(package.kotlin.opt_in, ["kotlin.time.ExperimentalTime"]);
        assert_eq!(package.jdk.as_deref(), Some("21"));
        assert_eq!(package.jvm_target.as_deref(), Some("17"));
        assert_eq!(package.main_class.as_deref(), Some("com.example.MainKt"));
        assert!(package.dependencies.iter().any(|d| d.group == "io.ktor" && d.artifact == "ktor-client-core"));
        assert!(package.dependencies.iter().any(|d| d.artifact == "postgresql" && d.scope == Scope::Runtime));
        assert!(package.frameworks.compose.is_some(), "Compose replaces its catalog dependencies");
        let serialization = package.frameworks.serialization.as_ref().expect("serialization is enabled");
        assert_eq!(serialization.formats, ["json"]);
        assert_eq!(package.repositories.len(), 1);
        assert!(package.moves.iter().any(|m| m.from == Path::new("test") && m.to == Path::new("tests")));
    }

    #[test]
    fn reads_a_multiplatform_library() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "module.yaml",
            r#"product:
  type: kmp/lib
  platforms: [ jvm, android, iosArm64, iosSimulatorArm64 ]

aliases:
  - jvmAndAndroid: [jvm, android]

dependencies:
  - io.ktor:ktor-client-core:3.0.1

dependencies@jvm:
  - com.squareup.okhttp3:okhttp:4.12.0

dependencies@ios:
  - io.ktor:ktor-client-darwin:3.0.1

dependencies@jvmAndAndroid:
  - org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0

test-dependencies:
  - io.mockk:mockk:1.13.0
"#,
        );
        for set in ["src", "src@jvm", "src@ios", "src@native", "test", "test@jvm", "resources", "resources@jvm"] {
            std::fs::create_dir_all(dir.path().join(set)).unwrap();
        }
        write(dir.path(), "src@jvm/Jvm.kt", "");
        let imported = read(dir.path()).unwrap();
        let package = &imported.packages[0];
        assert!(package.targets.iter().any(|t| t.kind == TargetKind::Jvm));
        assert!(package.targets.iter().any(|t| t.kind == TargetKind::Android));
        let native = package.targets.iter().find(|t| t.kind == TargetKind::Native).unwrap();
        assert!(native.rows.iter().any(|(k, v)| k == "platforms" && v == &Value::strings(["iosArm64", "iosSimulatorArm64"])));
        assert!(package.target_dependencies[&TargetKind::Jvm].iter().any(|d| d.artifact == "okhttp"));
        assert!(package.target_dependencies[&TargetKind::Jvm].iter().any(|d| d.artifact == "kotlinx-coroutines-core"));
        assert!(package.target_dependencies[&TargetKind::Android].iter().any(|d| d.artifact == "kotlinx-coroutines-core"));
        assert!(package.target_dependencies[&TargetKind::Native].iter().any(|d| d.artifact == "ktor-client-darwin"));
        assert_eq!(package.dev_dependencies.len(), 1);
        assert!(package.moves.iter().any(|m| m.from == Path::new("src@jvm") && m.to == Path::new("src-jvm")));
        assert!(package.moves.iter().any(|m| m.from == Path::new("src@ios") && m.to == Path::new("src-native")));
        assert!(!package.moves.iter().any(|m| m.from == Path::new("resources@jvm")));
    }

    #[test]
    fn reads_a_workspace_and_its_path_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "project.yaml", "modules:\n  - app\n  - libs/core\n");
        write(dir.path(), "app/module.yaml", "product: jvm/app\ndependencies:\n  - //libs/core\n");
        write(dir.path(), "libs/core/module.yaml", "product: jvm/lib\n");
        write(dir.path(), "app/src/Main.kt", "");
        let imported = read(dir.path()).unwrap();
        assert!(imported.workspace);
        assert_eq!(imported.packages.len(), 2);
        let app = imported.packages.iter().find(|p| p.name == "app").unwrap();
        let core = app.dependencies.iter().find(|d| d.path.is_some()).unwrap();
        assert_eq!(core.path.as_ref().unwrap().name, "core");
        assert_eq!(core.path.as_ref().unwrap().dir, Path::new("../libs/core"));
        assert!(imported.notes.iter().any(|n| n.message.contains("workspace members")));
    }

    #[test]
    fn reports_keys_with_no_equivalent() {
        let dir = single_module(
            r#"product: jvm/app
apply:
  - //common.module-template.yaml
mavenPlugins:
  - something
settings:
  kotlin:
    powerAssert: enabled
"#,
        );
        let imported = read(dir.path()).unwrap();
        let notes: Vec<&str> = imported.notes.iter().map(|n| n.message.as_str()).collect();
        assert!(notes.iter().any(|n| n.contains("apply")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("mavenPlugins")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("power-assert")), "{notes:?}");
    }

    #[test]
    fn globs_match_module_directories() {
        assert!(glob_match("plugins/*", "plugins/a"));
        assert!(!glob_match("plugins/*", "plugins/a/b"));
        assert!(glob_match("libs/*", "libs/core"));
        assert!(glob_match("libs/c?re", "libs/core"));
        assert_eq!(expand_braces("libs/{a,b}"), vec!["libs/a".to_owned(), "libs/b".to_owned()]);
    }
}
