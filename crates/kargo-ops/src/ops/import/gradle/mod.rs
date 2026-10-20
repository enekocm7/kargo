//! Reading a Gradle project: `settings.gradle(.kts)`, `build.gradle(.kts)`, `gradle.properties` and
//! version catalogs.
//!
//! Gradle builds are programs, so this is a best effort: it reads the declarative parts (the
//! `plugins {}`, `dependencies {}`, `kotlin {}`, `android {}` blocks and the usual settings) and lists
//! what it could not translate.

mod catalog;
mod dependencies;
mod dsl;
mod sources;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use kargo_manifest::{DEFAULT_KOTLIN_VERSION, TargetKind, validate_package_name};

use self::catalog::Catalog;
use self::dependencies::{Configuration, Context, Notation, Placement};
use self::dsl::{Scope as Vars, Stmt, Tok};
use super::frameworks::{self, Hints};
use super::model::{CompilerPlugin, Dependency, KotlinSettings, LocalPackage, Package, Repository, Scope, TargetSettings, Value};
use super::{Imported, relative_dir};

/// The documentation page about moving from Gradle.
const MIGRATION: &str = "migration/from-gradle/";

/// The Kotlin/Native targets Kargo builds, by their Gradle names.
const NATIVE_PRESETS: &[&str] = &[
    "linuxX64",
    "linuxArm64",
    "mingwX64",
    "macosX64",
    "macosArm64",
    "iosArm64",
    "iosX64",
    "iosSimulatorArm64",
    "watchosArm64",
    "watchosSimulatorArm64",
    "tvosArm64",
    "tvosSimulatorArm64",
    "androidNativeArm64",
    "androidNativeArm32",
    "androidNativeX64",
    "androidNativeX86",
];

/// Native presets Kotlin has and Kargo does not build.
const NATIVE_UNSUPPORTED: &[&str] = &["watchosX64", "watchosArm32", "tvosX64", "linuxArm32Hfp", "mingwX86", "wasm32"];

pub(super) fn read(root: &Path) -> Result<Imported> {
    let mut build = Build::load(root);
    build.run()?;
    Ok(build.imported)
}

fn read_script(dir: &Path, stem: &str) -> Vec<Stmt> {
    for extension in ["gradle.kts", "gradle"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(format!("{stem}.{extension}"))) {
            return dsl::parse(&text);
        }
    }
    Vec::new()
}

/// `key=value` and `key: value` lines of `gradle.properties`.
fn read_properties(root: &Path) -> HashMap<String, String> {
    let mut properties = HashMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("gradle.properties")) else { return properties };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            properties.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    properties
}

/// A plugin a script applies.
#[derive(Debug, Clone, PartialEq)]
struct PluginUse {
    id: String,
    version: Option<String>,
    /// `false` for `apply false`, which only declares the version.
    apply: bool,
}

/// One Gradle project of the build.
struct ProjectDef {
    /// `:` for the root project, `:core`, `:libs:util`.
    path: String,
    dir: PathBuf,
    /// The statements of its script, after the ones it inherits from the root script.
    stmts: Vec<Stmt>,
}

/// The kind of module a Kotlin plugin makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Jvm,
    Multiplatform,
    Android,
    Js,
}

struct Build {
    root: PathBuf,
    properties: HashMap<String, String>,
    catalogs: Vec<Catalog>,
    settings: Vec<Stmt>,
    root_script: Vec<Stmt>,
    imported: Imported,
    /// Plugin versions declared by the root script (`apply false`), for the projects that apply them without one.
    declared_versions: HashMap<String, String>,
}

impl Build {
    fn load(root: &Path) -> Self {
        Build {
            root: root.to_path_buf(),
            properties: read_properties(root),
            catalogs: catalog::load_all(root),
            settings: read_script(root, "settings"),
            root_script: read_script(root, "build"),
            imported: Imported::default(),
            declared_versions: HashMap::new(),
        }
    }

    fn manual(&mut self, message: impl Into<String>, docs: &'static str) {
        self.imported.manual(message, docs);
    }

    /// The names the scripts can refer to: `gradle.properties`, `val`s and `extra` properties, and catalog versions.
    fn vars(&self, stmts: &[Stmt]) -> Vars {
        let mut vars = Vars { vars: self.properties.clone(), ..Vars::default() };
        for catalog in &self.catalogs {
            vars.versions.extend(catalog.versions.clone());
        }
        // Twice, so a variable can use one that is declared after it.
        for _ in 0..2 {
            collect_vars(stmts, &mut vars);
            collect_vars(&self.root_script, &mut vars);
        }
        vars
    }

    fn run(&mut self) -> Result<()> {
        let settings_vars = self.vars(&[]);
        let root_name = self
            .settings
            .iter()
            .find(|s| s.is(&["rootProject", "name"]))
            .and_then(|s| s.assigned().and_then(|t| settings_vars.eval(t)))
            .or_else(|| self.root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_owned());

        // The projects: the root, and the ones `settings` includes.
        let mut includes: Vec<String> = Vec::new();
        for stmt in &self.settings {
            match stmt.name().as_deref() {
                Some("include") => {
                    for arg in stmt.args() {
                        if let Some(path) = arg.string() {
                            let path = format!(":{}", path.trim_start_matches(':'));
                            if !includes.contains(&path) {
                                includes.push(path);
                            }
                        }
                    }
                }
                Some("includeBuild") => self.imported.manual(
                    "the settings file uses includeBuild (a composite build); each included build has to be imported on its own",
                    MIGRATION,
                ),
                _ => {}
            }
        }
        // `include(":a:b")` also includes `:a`.
        let mut paths: Vec<String> = Vec::new();
        for include in &includes {
            let mut prefix = String::new();
            for segment in include.trim_start_matches(':').split(':') {
                prefix = format!("{prefix}:{segment}");
                if !paths.contains(&prefix) {
                    paths.push(prefix.clone());
                }
            }
        }
        let dir_of = |path: &str| PathBuf::from(path.trim_start_matches(':').replace(':', "/"));
        if self.settings.iter().any(|s| s.name().as_deref() == Some("project") && s.path().len() > 1) {
            self.manual(
                "the settings file moves a project's directory with projectDir; Kargo expects each module in the directory of its name",
                MIGRATION,
            );
        }

        let mut projects: Vec<ProjectDef> = Vec::new();
        let mut all_paths = vec![":".to_owned()];
        all_paths.extend(paths.iter().filter(|p| self.root.join(dir_of(p)).is_dir()).cloned());
        for path in &paths {
            if !self.root.join(dir_of(path)).is_dir() {
                self.manual(format!("the project `{path}` is included, but its directory does not exist"), MIGRATION);
            }
        }

        // Plugin versions and the blocks that configure other projects come from the root script.
        let root_uses = self.plugins(&self.root_script.clone());
        for plugin in &root_uses {
            if let Some(version) = &plugin.version {
                self.declared_versions.insert(plugin.id.clone(), version.clone());
            }
        }
        let SharedBlocks { all, subs, named } = shared_blocks(&self.root_script);
        for path in &all_paths {
            let dir = if path == ":" { PathBuf::new() } else { dir_of(path) };
            let own = if path == ":" { self.root_script.clone() } else { read_script(&self.root.join(&dir), "build") };
            let mut stmts = all.clone();
            if path != ":" {
                stmts.extend(subs.iter().cloned());
            }
            for (target, block) in &named {
                if target == path {
                    stmts.extend(block.iter().cloned());
                }
            }
            stmts.extend(own);
            projects.push(ProjectDef { path: path.clone(), dir, stmts });
        }

        // Which projects are Kotlin modules, and their names as packages.
        let workspace = projects.len() > 1;
        let mut kinds: HashMap<String, Kind> = HashMap::new();
        for project in &projects {
            let uses = self.plugins(&project.stmts);
            if let Some(kind) = kind_of(&uses) {
                kinds.insert(project.path.clone(), kind);
            }
        }
        if workspace && kinds.contains_key(":") {
            self.manual("the root project applies a Kotlin plugin and also includes other projects; Kargo's workspace root has no package, so the root project's own code was not imported", MIGRATION);
            kinds.remove(":");
        }
        let kotlin_projects: Vec<&ProjectDef> = projects.iter().filter(|p| kinds.contains_key(&p.path)).collect();
        if kotlin_projects.is_empty() {
            bail!(
                "no Gradle project applies a Kotlin plugin (kotlin(\"jvm\"), kotlin(\"multiplatform\") or kotlin(\"android\")), so there is no Kotlin code to import"
            );
        }
        let mut names: HashMap<String, String> = HashMap::new();
        for project in &kotlin_projects {
            let last = if project.path == ":" { root_name.clone() } else { project.path.rsplit(':').next().unwrap_or_default().to_owned() };
            let duplicate =
                kotlin_projects.iter().filter(|p| p.path != project.path && p.path.rsplit(':').next() == Some(last.as_str())).count() > 0;
            let wanted = if duplicate && project.path != ":" { project.path.trim_start_matches(':').replace(':', "-") } else { last };
            let (name, renamed) = package_name(&wanted);
            if renamed {
                self.manual(format!("the project name `{wanted}` is not a valid package name; the package is called `{name}`"), MIGRATION);
            }
            names.insert(project.path.clone(), name);
        }
        let locals: HashMap<String, LocalPackage> =
            kotlin_projects.iter().map(|p| (p.path.clone(), LocalPackage { name: names[&p.path].clone(), dir: p.dir.clone() })).collect();
        let project_dirs: HashMap<String, String> =
            projects.iter().map(|p| (p.path.clone(), p.dir.to_string_lossy().replace('\\', "/"))).collect();

        let mut packages = Vec::new();
        for project in &kotlin_projects {
            let kind = kinds[&project.path];
            if let Some(package) = self.translate(project, kind, &names[&project.path], &locals, &project_dirs, &root_name)? {
                packages.push(package);
            }
        }
        for project in projects.iter().filter(|p| !kinds.contains_key(&p.path) && p.path != ":") {
            self.manual(format!("the project `{}` applies no Kotlin plugin, so it was not imported", project.path), MIGRATION);
        }
        if packages.is_empty() {
            bail!("none of the Gradle projects could be imported");
        }
        self.imported.workspace = workspace;
        let list: Vec<_> = packages.iter().map(|p| p.name.clone()).collect();
        if workspace {
            self.imported.translated(format!("include(...) -> workspace members ({})", list.join(", ")));
        }
        self.imported.packages = packages;
        Ok(())
    }

    /// The plugins a script applies, in `plugins {}`, `apply` statements and `buildscript` classpaths.
    fn plugins(&self, stmts: &[Stmt]) -> Vec<PluginUse> {
        let vars = self.vars(stmts);
        let mut uses = Vec::new();
        for stmt in stmts {
            match stmt.name().as_deref() {
                Some("plugins") => {
                    for plugin in stmt.block.iter().flatten() {
                        if let Some(found) = self.plugin_use(plugin, &vars) {
                            uses.push(found);
                        }
                    }
                }
                Some("apply") => {
                    let args = stmt.args();
                    let id = args.iter().find(|a| a.name.as_deref() == Some("plugin")).and_then(|a| vars.eval(&a.value));
                    if let Some(id) = id {
                        let id = match id.as_str() {
                            "kotlin" => "org.jetbrains.kotlin.jvm".to_owned(),
                            "kotlin-android" => "org.jetbrains.kotlin.android".to_owned(),
                            "kotlin-multiplatform" => "org.jetbrains.kotlin.multiplatform".to_owned(),
                            "kotlin-kapt" => "org.jetbrains.kotlin.kapt".to_owned(),
                            "kotlin-spring" => "org.jetbrains.kotlin.plugin.spring".to_owned(),
                            other => other.to_owned(),
                        };
                        let version = self.declared_versions.get(&id).cloned();
                        uses.push(PluginUse { id, version, apply: true });
                    }
                }
                Some("buildscript") => {
                    // `classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:1.9.0")` gives the Kotlin version.
                    dsl::walk(stmt.block.as_deref().unwrap_or_default(), &mut |inner, _| {
                        if inner.name().as_deref() == Some("classpath")
                            && let Some(text) = inner.args().first().and_then(|a| vars.eval(&a.value))
                            && let Some(version) = text.strip_prefix("org.jetbrains.kotlin:kotlin-gradle-plugin:")
                        {
                            uses.push(PluginUse {
                                id: "org.jetbrains.kotlin.buildscript".to_owned(),
                                version: Some(version.to_owned()),
                                apply: false,
                            });
                        }
                    });
                }
                _ => {}
            }
        }
        // A version declared by the root script applies to the projects that name the plugin without one.
        for plugin in &mut uses {
            if plugin.version.is_none() {
                plugin.version = self.declared_versions.get(&plugin.id).cloned();
            }
        }
        uses
    }

    fn plugin_use(&self, stmt: &Stmt, vars: &Vars) -> Option<PluginUse> {
        let name = stmt.name()?;
        let args = stmt.args();
        let first = args.first();
        // `version "x"` and `apply false` follow the call.
        let tokens = &stmt.tokens;
        let version = tokens.iter().position(|t| matches!(t, Tok::Ident(w) if w == "version")).and_then(|at| {
            let end = tokens[at + 1..].iter().position(|t| matches!(t, Tok::Ident(w) if w == "apply")).map_or(tokens.len(), |e| at + 1 + e);
            vars.eval(&tokens[at + 1..end])
        });
        let apply = !tokens.windows(2).any(|w| matches!((&w[0], &w[1]), (Tok::Ident(a), Tok::Ident(f)) if a == "apply" && f == "false"));
        // Groovy writes `id 'x' version 'y'`, so the first token is the string.
        let first_string = first.and_then(|a| match a.value.first() {
            Some(Tok::Str(s)) => Some(s.clone()),
            _ => None,
        });
        let (id, version) = match name.as_str() {
            "kotlin" => (format!("org.jetbrains.kotlin.{}", first_string?), version),
            "id" => (first_string?, version),
            "alias" => {
                let path: Vec<&str> =
                    first?.value.iter().filter_map(|t| if let Tok::Ident(s) = t { Some(s.as_str()) } else { None }).collect();
                let catalog = self.catalogs.iter().find(|c| Some(c.name.as_str()) == path.first().copied())?;
                let alias =
                    dsl::normalize_alias(&path.get(2..)?.iter().filter(|s| !matches!(**s, "get")).copied().collect::<Vec<_>>().join("."));
                let found = catalog.plugins.get(&alias)?;
                (found.id.clone(), found.version.clone())
            }
            other => (other.to_owned(), version),
        };
        Some(PluginUse { id, version, apply })
    }

    #[allow(clippy::too_many_arguments)]
    fn translate(
        &mut self,
        project: &ProjectDef,
        kind: Kind,
        name: &str,
        locals: &HashMap<String, LocalPackage>,
        project_dirs: &HashMap<String, String>,
        root_name: &str,
    ) -> Result<Option<Package>> {
        let stmts = &project.stmts;
        let uses = self.plugins(stmts);
        let vars = self.vars(stmts);
        let applied: Vec<&PluginUse> = uses.iter().filter(|p| p.apply).collect();
        let has = |id: &str| applied.iter().any(|p| p.id == id);
        let version_of = |id: &str| uses.iter().find(|p| p.id == id).and_then(|p| p.version.clone());
        let label = if project.path == ":" { root_name.to_owned() } else { project.path.clone() };

        if kind == Kind::Js {
            self.manual(
                format!("`{label}` is a Kotlin/JS project; Kargo builds the JVM, Wasm, Native and Android targets"),
                "guides/wasm/",
            );
            return Ok(None);
        }

        let mut package = Package { dir: project.dir.clone(), name: name.to_owned(), ..Package::default() };

        // Coordinates.
        let top_level = |key: &str| {
            stmts.iter().rev().filter(|s| s.path().len() == 1 && s.name().as_deref() == Some(key)).find_map(|s| top_value(&vars, s))
        };
        package.version = top_level("version").filter(|v| v != "unspecified").unwrap_or_else(|| "0.1.0".to_owned());
        package.group = top_level("group").or_else(|| {
            let root = self.root_script.iter().rev().filter(|s| s.name().as_deref() == Some("group")).find_map(|s| top_value(&vars, s));
            root.or_else(|| self.properties.get("group").cloned())
        });
        package.description = top_level("description");

        // Kotlin.
        let mut kotlin = KotlinSettings {
            version: uses
                .iter()
                .filter(|p| p.id.starts_with("org.jetbrains.kotlin.") && !p.id.starts_with("org.jetbrains.kotlin.plugin."))
                .find_map(|p| p.version.clone())
                .or_else(|| vars.vars.get("kotlin_version").or_else(|| vars.vars.get("kotlinVersion")).cloned())
                .or_else(|| vars.versions.get("kotlin").cloned()),
            ..KotlinSettings::default()
        };
        if kotlin.version.is_none() {
            self.manual(
                format!("the Kotlin version of `{label}` was not found; Kargo uses {DEFAULT_KOTLIN_VERSION}"),
                "reference/manifest/",
            );
        }
        let mut jvm_target: Option<String> = None;
        let mut jdk: Option<String> = None;
        let mut main_class: Option<String> = None;
        let mut javac_args: Vec<String> = Vec::new();
        let mut allopen: Vec<(String, String)> = Vec::new();
        let mut noarg: Vec<(String, String)> = Vec::new();
        let mut custom_tasks: Vec<String> = Vec::new();
        let mut main_dirs: Vec<PathBuf> = Vec::new();
        let mut test_dirs: Vec<PathBuf> = Vec::new();
        let mut repositories: Vec<Repository> = Vec::new();
        let mut explicit_api = false;
        dsl::walk(stmts, &mut |stmt, ancestors| {
            let Some(name) = stmt.name() else { return };
            let first = stmt.path().first().cloned().unwrap_or_default();
            let in_block = |block: &str| ancestors.iter().any(|a| a == block);
            if first == "tasks" && matches!(name.as_str(), "register" | "create") {
                if let Some(task) = stmt.tokens.iter().find_map(|t| if let Tok::Str(s) = t { Some(s.clone()) } else { None }) {
                    custom_tasks.push(task);
                }
                return;
            }
            match first.as_str() {
                "jvmTarget" => jvm_target = jvm_target.take().or_else(|| value_or_version(&vars, stmt)),
                "jvmToolchain" => {
                    if let Some(v) = stmt.args().first().and_then(|a| vars.eval(&a.value)) {
                        jdk = Some(v.clone());
                        jvm_target = jvm_target.take().or(Some(v));
                    }
                }
                "sourceCompatibility" | "targetCompatibility" | "jvmTargetCompatibility" if !in_block("android") => {
                    jvm_target = jvm_target.take().or_else(|| value_or_version(&vars, stmt));
                }
                "languageVersion" if in_block("toolchain") => {
                    if let Some(v) = stmt.args().first().and_then(|a| number_in(&a.value)).or_else(|| stmt.assigned().and_then(number_in)) {
                        jdk = Some(v.clone());
                        jvm_target = jvm_target.take().or(Some(v));
                    }
                }
                "languageVersion" => kotlin.language_version = kotlin.language_version.take().or_else(|| value_or_version(&vars, stmt)),
                "apiVersion" => kotlin.api_version = kotlin.api_version.take().or_else(|| value_or_version(&vars, stmt)),
                "freeCompilerArgs" => {
                    for arg in strings_in(stmt.rest()) {
                        if let Some(opt_in) = arg.strip_prefix("-opt-in=") {
                            kotlin.opt_in.push(opt_in.to_owned());
                        } else if arg == "-Werror" {
                            kotlin.warnings_as_errors = true;
                        } else if !kotlin.compiler_args.contains(&arg) {
                            kotlin.compiler_args.push(arg);
                        }
                    }
                }
                "optIn" if !in_block("dependencies") => kotlin.opt_in.extend(strings_in(stmt.rest())),
                "allWarningsAsErrors" => {
                    kotlin.warnings_as_errors |= stmt.assigned().is_some_and(|t| matches!(t, [Tok::Ident(v)] if v == "true"))
                        || stmt.args().first().is_some_and(|a| matches!(a.value.as_slice(), [Tok::Ident(v)] if v == "true"));
                }
                "explicitApi" | "explicitApiWarning" => explicit_api = true,
                "mainClass" | "mainClassName" => main_class = main_class.take().or_else(|| value_of(&vars, stmt)),
                "annotation" if in_block("allOpen") => {
                    allopen.extend(strings_in(stmt.rest()).into_iter().map(|a| ("annotation".to_owned(), a)))
                }
                "annotation" if in_block("noArg") => {
                    noarg.extend(strings_in(stmt.rest()).into_iter().map(|a| ("annotation".to_owned(), a)))
                }
                "annotations" if in_block("allOpen") => {
                    allopen.extend(strings_in(stmt.rest()).into_iter().map(|a| ("annotation".to_owned(), a)))
                }
                "annotations" if in_block("noArg") => {
                    noarg.extend(strings_in(stmt.rest()).into_iter().map(|a| ("annotation".to_owned(), a)))
                }
                "compilerArgs" if in_block("withType") || in_block("compileJava") => javac_args.extend(strings_in(stmt.rest())),
                "srcDir" | "srcDirs" | "setSrcDirs" if in_block("sourceSets") => {
                    let dirs: Vec<PathBuf> = strings_in(stmt.rest()).into_iter().map(PathBuf::from).collect();
                    let test = ancestors.iter().any(|a| a.to_ascii_lowercase().contains("test"))
                        || stmt.path().iter().any(|p| p.to_ascii_lowercase().contains("test"));
                    if test { test_dirs.extend(dirs) } else { main_dirs.extend(dirs) }
                }
                "task" if ancestors.is_empty() => {
                    if let Some(task) =
                        stmt.args().first().and_then(|a| a.string().map(str::to_owned)).or_else(|| match stmt.rest().first() {
                            Some(Tok::Ident(n)) => Some(n.clone()),
                            _ => None,
                        })
                    {
                        custom_tasks.push(task);
                    }
                }
                _ => {}
            }
        });
        // Repositories of the project and of `dependencyResolutionManagement` in the settings.
        for scope in [stmts, &self.settings[..]] {
            dsl::walk(scope, &mut |stmt, ancestors| {
                // The repositories plugins come from are not the project's.
                if !ancestors.iter().any(|a| a == "repositories")
                    || ancestors.iter().any(|a| matches!(a.as_str(), "pluginManagement" | "buildscript"))
                {
                    return;
                }
                match stmt.name().as_deref() {
                    Some("maven") => {
                        let url = stmt.args().first().and_then(|a| vars.eval(&a.value)).or_else(|| {
                            stmt.block.iter().flatten().find_map(|s| match s.name().as_deref() {
                                Some("url" | "setUrl") => s
                                    .assigned()
                                    .or(Some(s.rest()))
                                    .and_then(|t| uri_value(&vars, t))
                                    .or_else(|| s.args().first().and_then(|a| uri_value(&vars, &a.value))),
                                _ => None,
                            })
                        });
                        if let Some(url) = url
                            && !is_standard_repository(&url)
                            && !repositories.iter().any(|r| r.url == url)
                        {
                            repositories.push(Repository { name: repository_name(&url, repositories.len()), url });
                        }
                    }
                    Some("mavenLocal") => {
                        if !repositories.iter().any(|r| r.url == "maven-local") {
                            repositories.push(Repository { name: "local".to_owned(), url: "maven-local".to_owned() });
                        }
                    }
                    Some("jcenter") => self.imported.manual("jcenter() is shut down; Maven Central is used instead", MIGRATION),
                    _ => {}
                }
            });
        }
        if kotlin.compiler_args.iter().any(|a| a == "-Xexplicit-api=strict") {
            explicit_api = false;
        }
        if explicit_api {
            kotlin.compiler_args.push("-Xexplicit-api=strict".to_owned());
        }
        let _ = &mut javac_args;
        let compiler_plugin = |plugin: &str, id: &str, preset: Option<&str>, options: &[(String, String)]| -> Vec<CompilerPlugin> {
            let version = kotlin.version.clone().unwrap_or_else(|| DEFAULT_KOTLIN_VERSION.to_owned());
            let mut all: Vec<(String, String)> = options.to_vec();
            if let Some(preset) = preset {
                all.insert(0, ("preset".to_owned(), preset.to_owned()));
            }
            all.into_iter()
                .map(|option| CompilerPlugin {
                    name: plugin.to_owned(),
                    maven: format!("org.jetbrains.kotlin:kotlin-{plugin}-compiler-plugin-embeddable:{version}"),
                    id: id.to_owned(),
                    options: vec![option],
                })
                .collect()
        };

        // Targets.
        let mut jvm_names: Vec<String> = Vec::new();
        let mut targets: Vec<TargetSettings> = Vec::new();
        match kind {
            Kind::Jvm => {}
            Kind::Android => targets.push(self.android_target(stmts, &vars, &uses, name, &package)),
            Kind::Multiplatform => {
                let found = self.multiplatform_targets(stmts, &vars, &uses, name, &package, &label, &mut jvm_names);
                targets = found;
            }
            Kind::Js => unreachable!("returned above"),
        }
        if kind == Kind::Android || (kind == Kind::Multiplatform && targets.iter().any(|t| t.kind == TargetKind::Android)) {
            // The Android Gradle plugin has its own idea of the Java version.
            jvm_target = jvm_target.filter(|_| kind != Kind::Android);
        }

        // Dependencies.
        let ctx_projects: HashMap<String, String> = project_dirs.clone();
        let ctx = Context {
            vars: &vars,
            catalogs: &self.catalogs,
            kotlin_version: kotlin.version.as_deref(),
            jvm_targets: &jvm_names,
            projects: &ctx_projects,
        };
        let mut collected = Collected::default();
        let multiplatform = kind == Kind::Multiplatform;
        collect_dependencies(&ctx, stmts, &mut collected);
        let mut main_deps: Vec<Dependency> = Vec::new();
        let mut dev_deps: Vec<Dependency> = Vec::new();
        let mut target_deps: BTreeMap<TargetKind, Vec<Dependency>> = BTreeMap::new();
        for (placement, mut dep, local) in collected.deps {
            if let Some(gradle_path) = local {
                let Some(found) = locals.get(&gradle_path) else {
                    collected
                        .notes
                        .push(format!("`{label}` depends on {gradle_path}, which is not a Kotlin project that could be imported"));
                    continue;
                };
                dep.path = Some(LocalPackage { name: found.name.clone(), dir: relative_dir(&project.dir, &found.dir) });
                dep.version = None;
                dep.bom = false;
            }
            if placement.test {
                dev_deps.push(dep);
            } else if multiplatform && let Some(target) = placement.target {
                target_deps.entry(target).or_default().push(dep);
            } else {
                main_deps.push(dep);
            }
        }
        for note in collected.notes {
            self.manual(note, MIGRATION);
        }
        if collected.kapt {
            self.manual(
                format!("`{label}` uses kapt, which Kargo does not run; use a KSP processor (`scope = \"ksp\"`) or a Java annotation processor (`scope = \"processor\"`)"),
                "guides/dependencies/#kotlin-symbol-processing-ksp",
            );
        }
        package.npm_dependencies = collected.npm;

        // Frameworks.
        let spring = has("org.springframework.boot");
        let ktor_plugin = version_of("io.ktor.plugin");
        let hints = Hints {
            spring_boot: spring.then(|| version_of("org.springframework.boot")),
            jpa: has("org.jetbrains.kotlin.plugin.jpa"),
            serialization_plugin: has("org.jetbrains.kotlin.plugin.serialization"),
            compose: (has("org.jetbrains.compose") || collected.compose || has("org.jetbrains.kotlin.plugin.compose"))
                .then(|| version_of("org.jetbrains.compose")),
            ktor: has("io.ktor.plugin").then_some(ktor_plugin),
            lombok: has("io.freefair.lombok") || has("org.jetbrains.kotlin.plugin.lombok"),
        };
        let detected = frameworks::detect(&mut main_deps, &mut dev_deps, &hints);
        for line in &detected.translated {
            self.imported.translated(format!("{name}: {line}"));
        }
        let spring_framework = detected.frameworks.spring_boot.is_some();
        package.frameworks = detected.frameworks;
        for deps in std::iter::once(&mut main_deps).chain(std::iter::once(&mut dev_deps)).chain(target_deps.values_mut()) {
            deps.retain(|d| !frameworks::is_built_in(d));
        }
        // What a framework adds by itself.
        if spring_framework {
            kotlin.compiler_args.retain(|a| a != "-Xjsr305=strict" && a != "-java-parameters");
        }
        // Kotlin compiler plugins.
        let mut plugins: Vec<CompilerPlugin> = Vec::new();
        if has("org.jetbrains.kotlin.plugin.spring") && !spring_framework {
            plugins.extend(compiler_plugin("allopen", "org.jetbrains.kotlin.allopen", Some("spring"), &[]));
        }
        if has("org.jetbrains.kotlin.plugin.jpa") && !spring_framework {
            plugins.extend(compiler_plugin("noarg", "org.jetbrains.kotlin.noarg", Some("jpa"), &[]));
        }
        if has("org.jetbrains.kotlin.plugin.allopen") {
            if allopen.is_empty() {
                self.manual(
                    format!("`{label}` applies the all-open plugin without annotations that Kargo could read"),
                    "guides/tasks-and-plugins/#compiler-plugins",
                );
            }
            plugins.extend(compiler_plugin("allopen", "org.jetbrains.kotlin.allopen", None, &allopen));
        }
        if has("org.jetbrains.kotlin.plugin.noarg") {
            if noarg.is_empty() {
                self.manual(
                    format!("`{label}` applies the no-arg plugin without annotations that Kargo could read"),
                    "guides/tasks-and-plugins/#compiler-plugins",
                );
            }
            plugins.extend(compiler_plugin("noarg", "org.jetbrains.kotlin.noarg", None, &noarg));
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for plugin in &mut plugins {
            let count = counts.entry(plugin.name.clone()).or_insert(0);
            *count += 1;
            if *count > 1 {
                plugin.name = format!("{}-{count}", plugin.name);
            }
        }
        package.plugins = plugins;

        // KSP's version.
        if let Some(ksp) = version_of("com.google.devtools.ksp") {
            if ksp.contains('-') {
                self.manual(
                    format!("`{label}` uses the KSP plugin {ksp}; Kargo runs KSP 2 (`[kotlin] ksp`), whose versions look like 2.3.12; the processors need no change"),
                    "guides/dependencies/#kotlin-symbol-processing-ksp",
                );
            } else {
                kotlin.ksp = Some(ksp);
            }
        }

        // Plugins with no setting in Kargo.
        for plugin in applied.iter().filter(|p| !is_handled(&p.id)) {
            match advice(&plugin.id) {
                Some((text, docs)) => self.manual(format!("`{label}` applies `{}`: {text}", plugin.id), docs),
                None => self.manual(format!("`{label}` applies the plugin `{}`, which Kargo has no equivalent for", plugin.id), MIGRATION),
            }
        }
        if !custom_tasks.is_empty() {
            custom_tasks.sort();
            custom_tasks.dedup();
            self.manual(
                format!("`{label}` defines the tasks {}; recreate them as `[tasks]` or `[plugins.<name>]` hooks", custom_tasks.join(", ")),
                "guides/tasks-and-plugins/",
            );
        }

        // Repositories and the rest of the package.
        if !repositories.is_empty() {
            self.imported.translated(format!(
                "{name}: repositories -> [repositories] ({})",
                repositories.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        package.repositories = repositories;
        package.jvm_target = jvm_target.and_then(|t| super::maven::java_target(&t));
        package.jdk = jdk;
        package.main_class = main_class;
        package.javac_args = javac_args;
        package.kotlin = kotlin;
        package.dependencies = main_deps;
        package.dev_dependencies = dev_deps;
        package.target_dependencies = target_deps;
        package.targets = targets;

        // Sources.
        let android_only = kind == Kind::Android;
        let layout = match kind {
            Kind::Multiplatform => {
                let layout = sources::multiplatform_moves(&self.root, &project.dir, &jvm_names);
                for set in &layout.left {
                    self.manual(format!("`{label}`: {set}"), "guides/multiplatform/");
                }
                if !layout.merged_native.is_empty() {
                    self.manual(
                        format!("`{label}`: the Kotlin/Native source sets {} all go to src-native/; files with the same name collide, and code for one platform needs `expect`/`actual`", layout.merged_native.join(", ")),
                        "guides/native/",
                    );
                }
                layout.moves
            }
            _ => {
                let moves = sources::standard_moves(&project.dir, android_only, &main_dirs, &test_dirs);
                for leftover in ["androidTest", "debug", "release"] {
                    if android_only && self.root.join(&project.dir).join("src").join(leftover).is_dir() {
                        self.manual(
                            format!("`{label}` has a `src/{leftover}` source set, which Kargo has no place for"),
                            "guides/android/",
                        );
                    }
                }
                moves
            }
        };
        package.moves = layout;
        self.imported.translated(format!(
            "{name}: {} dependencies, Kotlin {}",
            package.dependencies.len() + package.dev_dependencies.len() + package.target_dependencies.values().map(Vec::len).sum::<usize>(),
            package.kotlin.version.as_deref().unwrap_or("(default)")
        ));
        Ok(Some(package))
    }

    fn android_target(&mut self, stmts: &[Stmt], vars: &Vars, uses: &[PluginUse], name: &str, package: &Package) -> TargetSettings {
        let mut target = TargetSettings::new(TargetKind::Android);
        let library = uses.iter().any(|p| p.apply && p.id == "com.android.library");
        let mut namespace: Option<String> = None;
        let mut application_id: Option<String> = None;
        let mut numbers: BTreeMap<&str, i64> = BTreeMap::new();
        let mut version_name: Option<String> = None;
        let mut minify = false;
        let mut leftovers: Vec<&str> = Vec::new();
        dsl::walk(stmts, &mut |stmt, ancestors| {
            if !ancestors.iter().any(|a| a == "android") {
                if stmt.name().as_deref() == Some("android") {
                    return;
                }
                return;
            }
            let Some(key) = stmt.path().first().cloned() else { return };
            match key.as_str() {
                "namespace" => namespace = namespace.take().or_else(|| value_of(vars, stmt)),
                "applicationId" => application_id = application_id.take().or_else(|| value_of(vars, stmt)),
                "minSdk" | "minSdkVersion" => {
                    if let Some(n) = int_of(vars, stmt) {
                        numbers.entry("min-sdk").or_insert(n);
                    }
                }
                "targetSdk" | "targetSdkVersion" => {
                    if let Some(n) = int_of(vars, stmt) {
                        numbers.entry("target-sdk").or_insert(n);
                    }
                }
                "compileSdk" | "compileSdkVersion" => {
                    if let Some(n) = int_of(vars, stmt) {
                        numbers.entry("compile-sdk").or_insert(n);
                    }
                }
                "versionCode" => {
                    if let Some(n) = int_of(vars, stmt) {
                        numbers.entry("version-code").or_insert(n);
                    }
                }
                "versionName" => version_name = version_name.take().or_else(|| value_of(vars, stmt)),
                "isMinifyEnabled" | "minifyEnabled" if ancestors.iter().any(|a| a == "release") => {
                    minify = stmt.assigned().is_some_and(|t| matches!(t, [Tok::Ident(v)] if v == "true"));
                }
                "productFlavors" => leftovers.push("productFlavors"),
                "signingConfigs" => leftovers.push("signingConfigs"),
                _ => {}
            }
        });
        let namespace = namespace.or_else(|| application_id.clone()).unwrap_or_else(|| {
            let group = package.group.clone().unwrap_or_else(|| "com.example".to_owned());
            let derived = format!("{group}.{}", name.replace(['-', '.'], "_"));
            self.manual(
                format!("`{name}` has no Android namespace; it is `{derived}`, change it to the one of your manifest"),
                "guides/android/",
            );
            derived
        });
        target.set("namespace", Value::str(&namespace));
        if application_id.as_deref().is_some_and(|id| id != namespace) {
            target.set("application-id", Value::str(application_id.unwrap_or_default()));
        }
        target.set("kind", Value::str(if library { "library" } else { "application" }));
        for key in ["min-sdk", "target-sdk", "compile-sdk", "version-code"] {
            if let Some(value) = numbers.get(key) {
                target.set(key, Value::Int(*value));
            }
        }
        if let Some(version_name) = version_name {
            target.set("version-name", Value::str(version_name));
        }
        if minify {
            target.set("minify", Value::Bool(true));
        }
        leftovers.sort();
        leftovers.dedup();
        for left in leftovers {
            self.manual(
                format!("`{name}` configures {left}; see Android product flavors and signing for the `kargo.toml` equivalent"),
                "guides/android/",
            );
        }
        target
    }

    #[allow(clippy::too_many_arguments)]
    fn multiplatform_targets(
        &mut self,
        stmts: &[Stmt],
        vars: &Vars,
        uses: &[PluginUse],
        name: &str,
        package: &Package,
        label: &str,
        jvm_names: &mut Vec<String>,
    ) -> Vec<TargetSettings> {
        let mut jvm = false;
        let mut wasm: Option<WasmFound> = None;
        let mut wasi = false;
        let mut platforms: Vec<String> = Vec::new();
        let mut native_executable = false;
        let mut native_framework = false;
        let mut entry_point: Option<String> = None;
        let mut android = false;
        let mut js = false;
        let mut unsupported: Vec<String> = Vec::new();
        let mut cocoapods = false;
        for stmt in stmts.iter().filter(|s| s.name().as_deref() == Some("kotlin")) {
            for target in stmt.block.iter().flatten() {
                let Some(function) = target.name() else { continue };
                if target.path().len() != 1 {
                    continue;
                }
                match function.as_str() {
                    "jvm" => {
                        jvm = true;
                        if let Some(custom) = target.args().first().and_then(|a| a.string().map(str::to_owned)) {
                            jvm_names.push(custom);
                        }
                    }
                    "androidTarget" | "android" => android = true,
                    "js" => js = true,
                    "wasmJs" => {
                        let mut found = WasmFound::default();
                        dsl::walk(target.block.as_deref().unwrap_or_default(), &mut |inner, _| match inner.name().as_deref() {
                            Some("browser") => found.browser = true,
                            Some("nodejs") | Some("d8") => found.node = true,
                            Some("executable") => found.executable = true,
                            Some("outputModuleName" | "moduleName") => found.module = found.module.take().or_else(|| value_of(vars, inner)),
                            _ => {}
                        });
                        wasm = Some(found);
                    }
                    "wasmWasi" => wasi = true,
                    "cocoapods" => cocoapods = true,
                    preset if NATIVE_PRESETS.contains(&preset) => {
                        if !platforms.iter().any(|p| p == preset) {
                            platforms.push(preset.to_owned());
                        }
                        dsl::walk(target.block.as_deref().unwrap_or_default(), &mut |inner, _| match inner.name().as_deref() {
                            Some("executable") => native_executable = true,
                            Some("framework") => native_framework = true,
                            Some("entryPoint") => entry_point = entry_point.take().or_else(|| value_of(vars, inner)),
                            _ => {}
                        });
                    }
                    preset if NATIVE_UNSUPPORTED.contains(&preset) => unsupported.push(preset.to_owned()),
                    _ => {}
                }
            }
        }
        let _ = uses;
        let mut targets = Vec::new();
        if jvm {
            targets.push(TargetSettings::new(TargetKind::Jvm));
        }
        if let Some(found) = &wasm {
            let mut wasm_target = TargetSettings::new(TargetKind::Wasm);
            let environment = if found.browser { "browser" } else { "js" };
            wasm_target.set("environment", Value::str(environment));
            if !found.executable {
                wasm_target.set("kind", Value::str("library"));
            }
            if let Some(module) = &found.module
                && module.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                wasm_target.set("module-name", Value::str(module));
            }
            if found.browser && found.node {
                self.manual(
                    format!("`{label}` targets wasmJs for both the browser and Node.js; the Wasm target is set up for the browser"),
                    "guides/wasm/",
                );
            }
            targets.push(wasm_target);
            if wasi {
                self.manual(format!("`{label}` has wasmJs and wasmWasi; Kargo has one Wasm target (`environment = \"wasi\"` or `\"js\"`), and it is set up for wasmJs"), "guides/wasm/");
            }
        } else if wasi {
            let mut wasm_target = TargetSettings::new(TargetKind::Wasm);
            wasm_target.set("environment", Value::str("wasi"));
            targets.push(wasm_target);
        }
        if !platforms.is_empty() {
            let mut native = TargetSettings::new(TargetKind::Native);
            native.set("platforms", Value::strings(&platforms));
            let kind = if native_framework {
                "framework"
            } else if native_executable {
                "executable"
            } else {
                "library"
            };
            native.set("kind", Value::str(kind));
            if let Some(entry) = entry_point.filter(|_| native_executable) {
                native.set("entry-point", Value::str(entry));
            }
            targets.push(native);
        }
        if android {
            let uses_library = uses.iter().any(|p| p.id == "com.android.library" || p.id == "com.android.kotlin.multiplatform.library");
            let _ = uses_library;
            targets.push(self.android_target(stmts, vars, uses, name, package));
        }
        if js {
            self.manual(format!("`{label}` has a Kotlin/JS target, which Kargo does not build"), "guides/wasm/");
        }
        for preset in unsupported {
            self.manual(format!("`{label}` targets {preset}, which Kargo does not build"), "guides/native/");
        }
        if cocoapods {
            self.manual(
                format!("`{label}` uses the CocoaPods plugin; Kargo builds XCFrameworks with `kind = \"framework\"` instead"),
                "guides/native/",
            );
        }
        if targets.is_empty() {
            self.manual(
                format!("no target of `{label}` could be read from its kotlin {{ }} block; add `[target.*]` tables by hand"),
                "guides/multiplatform/",
            );
        }
        targets
    }
}

#[derive(Debug, Default)]
struct WasmFound {
    browser: bool,
    node: bool,
    executable: bool,
    module: Option<String>,
}

fn kind_of(uses: &[PluginUse]) -> Option<Kind> {
    let applied = |id: &str| uses.iter().any(|p| p.apply && p.id == id);
    if applied("org.jetbrains.kotlin.multiplatform") {
        Some(Kind::Multiplatform)
    } else if applied("org.jetbrains.kotlin.js") {
        Some(Kind::Js)
    } else if applied("org.jetbrains.kotlin.android")
        || ((applied("com.android.application") || applied("com.android.library")) && applied("org.jetbrains.kotlin.jvm"))
    {
        Some(Kind::Android)
    } else if applied("org.jetbrains.kotlin.jvm") {
        Some(Kind::Jvm)
    } else {
        None
    }
}

/// The statements the root script hands to the other projects.
struct SharedBlocks {
    /// `allprojects {}`: every project, the root too.
    all: Vec<Stmt>,
    /// `subprojects {}`: every project but the root.
    subs: Vec<Stmt>,
    /// `project(":x") {}`: one project, by its Gradle path.
    named: Vec<(String, Vec<Stmt>)>,
}

/// The statements the root script hands to other projects: `allprojects {}`, `subprojects {}` and `project(":x") {}`.
fn shared_blocks(script: &[Stmt]) -> SharedBlocks {
    let mut all = Vec::new();
    let mut subs = Vec::new();
    let mut named = Vec::new();
    for stmt in script {
        let Some(block) = &stmt.block else { continue };
        match stmt.name().as_deref() {
            Some("allprojects") => all.extend(block.iter().cloned()),
            Some("subprojects") => subs.extend(block.iter().cloned()),
            Some("configure") => {
                let which = stmt.tokens.iter().find_map(|t| if let Tok::Ident(n) = t { Some(n.as_str()) } else { None });
                match which {
                    Some("allprojects") => all.extend(block.iter().cloned()),
                    Some("subprojects") => subs.extend(block.iter().cloned()),
                    _ => {}
                }
            }
            Some("project") => {
                if let Some(path) = stmt.args().first().and_then(|a| a.string().map(str::to_owned)) {
                    named.push((format!(":{}", path.trim_start_matches(':')), block.clone()));
                }
            }
            _ => {}
        }
    }
    SharedBlocks { all, subs, named }
}

/// What the dependency declarations of a script gave.
#[derive(Default)]
struct Collected {
    /// The dependency, where it applies, and the Gradle path when it is another project.
    deps: Vec<(Placement, Dependency, Option<String>)>,
    notes: Vec<String>,
    kapt: bool,
    compose: bool,
    npm: Vec<(String, String)>,
}

/// Reads every `dependencies {}` block: the project's own, and the ones of the source sets of a
/// multiplatform project.
fn collect_dependencies(ctx: &Context, stmts: &[Stmt], out: &mut Collected) {
    for stmt in stmts {
        match stmt.name().as_deref() {
            Some("dependencies") if stmt.block.is_some() => {
                let set = source_set_of(stmt);
                let default = match &set {
                    Some(name) => match sources::classify(name, ctx.jvm_targets) {
                        Some(info) if info.kind == sources::SetKind::Supported => Placement { target: info.target, test: info.test },
                        _ => {
                            out.notes.push(format!("the dependencies of the source set `{name}` have no place in Kargo's layout; add them to the right [dependencies] table by hand"));
                            continue;
                        }
                    },
                    None => Placement::default(),
                };
                declarations(ctx, stmt.block.as_deref().unwrap_or_default(), default, out);
            }
            Some("sourceSets") | Some("kotlin") | Some("buildscript") if stmt.block.is_some() => {
                if stmt.name().as_deref() != Some("buildscript") {
                    collect_dependencies(ctx, stmt.block.as_deref().unwrap_or_default(), out);
                }
            }
            _ => {
                // `val commonMain by getting { dependencies { } }`, `commonMain { dependencies { } }`,
                // `getByName("x") { }`.
                if let Some(block) = &stmt.block
                    && block.iter().any(|s| s.name().as_deref() == Some("dependencies"))
                {
                    let name = set_name(stmt);
                    for inner in block.iter().filter(|s| s.name().as_deref() == Some("dependencies")) {
                        let default = match name.as_deref().and_then(|n| sources::classify(n, ctx.jvm_targets)) {
                            Some(info) if info.kind == sources::SetKind::Supported => Placement { target: info.target, test: info.test },
                            Some(_) | None if name.is_some() => {
                                out.notes.push(format!(
                                    "the dependencies of the source set `{}` have no place in Kargo's layout; add them to the right [dependencies] table by hand",
                                    name.clone().unwrap_or_default()
                                ));
                                continue;
                            }
                            _ => Placement::default(),
                        };
                        declarations(ctx, inner.block.as_deref().unwrap_or_default(), default, out);
                    }
                }
            }
        }
    }
}

/// The source set a statement like `commonMain.dependencies { }` or `sourceSets["x"].dependencies { }` is about.
fn source_set_of(stmt: &Stmt) -> Option<String> {
    let path = stmt.path();
    if path.len() >= 2 && path.last().map(String::as_str) == Some("dependencies") {
        let owner = &path[..path.len() - 1];
        return owner.iter().rev().find(|s| !matches!(s.as_str(), "sourceSets" | "kotlin")).cloned();
    }
    None
}

/// The source set a statement like `val commonMain by getting { }` or `getByName("jvmMain") { }` declares.
fn set_name(stmt: &Stmt) -> Option<String> {
    match stmt.tokens.as_slice() {
        [Tok::Ident(v), Tok::Ident(name), ..] if v == "val" => Some(name.clone()),
        _ => match stmt.name().as_deref() {
            Some("getByName" | "create" | "creating" | "named" | "maybeCreate") => {
                stmt.args().first().and_then(|a| a.string().map(str::to_owned))
            }
            Some(name) if name.ends_with("Main") || name.ends_with("Test") => Some(name.to_owned()),
            _ => None,
        },
    }
}

/// The declarations inside a `dependencies {}` block.
fn declarations(ctx: &Context, block: &[Stmt], default: Placement, out: &mut Collected) {
    for stmt in block {
        let Some(name) = stmt.name() else { continue };
        if stmt.path().len() != 1 {
            continue;
        }
        let configuration =
            dependencies::configuration(&name, ctx.jvm_targets).or_else(|| dependencies::ksp_configuration(&name, ctx.jvm_targets));
        let Some(configuration) = configuration else { continue };
        match configuration {
            Configuration::Kapt => out.kapt = true,
            Configuration::Unsupported(what) => out.notes.push(format!("{what} is not supported (`{}`)", name)),
            Configuration::Dependency { placement, scope, exported } => {
                let placement = match placement {
                    Some(p) => Placement { target: p.target.or(default.target), test: p.test || default.test },
                    None => default,
                };
                let args = stmt.args();
                if args.is_empty() {
                    continue;
                }
                match dependencies::notation(ctx, &args, stmt.block.as_deref()) {
                    Notation::Libraries(libs) => {
                        for mut dep in libs {
                            dep.scope = if dep.bom { Scope::Compile } else { scope };
                            dep.exported = exported && !dep.bom && !placement.test;
                            out.deps.push((placement, dep, None));
                        }
                    }
                    Notation::Project(path) => {
                        let dep = Dependency { scope, exported: exported && !placement.test, ..Dependency::default() };
                        out.deps.push((placement, dep, Some(path)));
                    }
                    Notation::Compose => out.compose = true,
                    Notation::Npm(name, version) => out.npm.push((name, version)),
                    Notation::Unsupported(what) => out.notes.push(format!("the dependency {what} (in `{}`) could not be translated", name)),
                }
            }
        }
    }
}

/// `val x = "..."`, `val x: String by project`, `extra["x"] = ...`, `ext { x = ... }`.
fn collect_vars(stmts: &[Stmt], vars: &mut Vars) {
    dsl::walk(stmts, &mut |stmt, ancestors| {
        let tokens = &stmt.tokens;
        // `val name [: Type] = value` / `def name = value`
        if let [Tok::Ident(keyword), Tok::Ident(name), rest @ ..] = tokens.as_slice()
            && matches!(keyword.as_str(), "val" | "var" | "def" | "final" | "String")
        {
            let rest = skip_type(rest);
            if let [Tok::Punct('='), value @ ..] = rest
                && let Some(text) = vars.eval(value)
            {
                vars.vars.insert(name.clone(), text);
            }
            return;
        }
        // `extra["x"] = "..."`, `ext["x"] = ...`, `project.extra["x"] = ...`
        if let Some(open) = tokens.iter().position(|t| matches!(t, Tok::Punct('[')))
            && matches!(tokens.first(), Some(Tok::Ident(n)) if matches!(n.as_str(), "extra" | "ext" | "project" | "rootProject"))
            && let (Some(Tok::Str(key)), Some(Tok::Punct(']')), Some(Tok::Punct('='))) =
                (tokens.get(open + 1), tokens.get(open + 2), tokens.get(open + 3))
            && let Some(text) = vars.eval(&tokens[open + 4..])
        {
            vars.vars.insert(key.clone(), text);
            return;
        }
        let path = stmt.path();
        // `ext.x = "..."`
        if path.len() == 2
            && matches!(path[0].as_str(), "ext" | "extra")
            && let Some(text) = stmt.assigned().and_then(|t| vars.eval(t))
        {
            vars.vars.insert(path[1].clone(), text);
            return;
        }
        // Inside `ext { }` / `extra { }`: `x = "..."`, `set("x", "...")`
        if ancestors.last().is_some_and(|a| a == "ext" || a == "extra") {
            if path.len() == 1
                && stmt.name().as_deref() == Some("set")
                && let [key, value, ..] = stmt.args().as_slice()
                && let (Some(key), Some(text)) = (key.string().map(str::to_owned), vars.eval(&value.value))
            {
                vars.vars.insert(key, text);
            } else if path.len() == 1
                && let Some(text) = stmt.assigned().and_then(|t| vars.eval(t))
            {
                vars.vars.insert(path[0].clone(), text);
            }
        }
    });
}

/// Tokens after `: Type` in `val x: Type = ...`.
fn skip_type(tokens: &[Tok]) -> &[Tok] {
    match tokens {
        [Tok::Punct(':'), Tok::Ident(_), rest @ ..] => match rest {
            [Tok::Punct('<'), ..] => rest.iter().position(|t| matches!(t, Tok::Punct('>'))).map_or(rest, |close| &rest[close + 1..]),
            _ => rest,
        },
        other => other,
    }
}

/// The value of `group = "..."` / `group '...'` / `version = rootProject.version`.
fn top_value(vars: &Vars, stmt: &Stmt) -> Option<String> {
    stmt.assigned().and_then(|t| vars.eval(t))
}

/// The value a statement sets: `x = "v"`, `x.set("v")`, `x("v")`, `x "v"`.
fn value_of(vars: &Vars, stmt: &Stmt) -> Option<String> {
    let path = stmt.path();
    if path.len() >= 2 && !matches!(path.last().map(String::as_str), Some("set" | "convention" | "value")) {
        return None;
    }
    stmt.assigned().and_then(|t| vars.eval(t)).or_else(|| stmt.args().first().and_then(|a| vars.eval(&a.value)))
}

/// Like [`value_of`], but also understands the enum constants versions are written as (`JvmTarget.JVM_17`).
fn value_or_version(vars: &Vars, stmt: &Stmt) -> Option<String> {
    value_of(vars, stmt).or_else(|| {
        let tokens: Vec<Tok> = stmt.assigned().map(<[Tok]>::to_vec).or_else(|| stmt.args().first().map(|a| a.value.clone()))?;
        enum_version(&tokens)
    })
}

/// `JvmTarget.JVM_17` is `17`, `JavaVersion.VERSION_1_8` is `1.8`, `KotlinVersion.KOTLIN_2_0` is `2.0`.
fn enum_version(tokens: &[Tok]) -> Option<String> {
    tokens.iter().rev().find_map(|t| match t {
        Tok::Ident(name) => {
            let rest = name.strip_prefix("JVM_").or_else(|| name.strip_prefix("VERSION_")).or_else(|| name.strip_prefix("KOTLIN_"))?;
            (rest.chars().all(|c| c.is_ascii_digit() || c == '_')).then(|| rest.replace('_', "."))
        }
        _ => None,
    })
}

fn int_of(vars: &Vars, stmt: &Stmt) -> Option<i64> {
    value_of(vars, stmt).and_then(|v| v.parse().ok())
}

/// A number inside `JavaLanguageVersion.of(17)`.
fn number_in(tokens: &[Tok]) -> Option<String> {
    tokens.iter().find_map(|t| if let Tok::Num(n) = t { Some(n.clone()) } else { None })
}

/// Every string literal in `tokens`: the elements of `listOf("a", "b")`.
fn strings_in(tokens: &[Tok]) -> Vec<String> {
    tokens.iter().filter_map(|t| if let Tok::Str(s) = t { Some(s.clone()) } else { None }).collect()
}

/// The URL in `uri("https://...")`, `"https://..."` or `java.net.URI("...")`.
fn uri_value(vars: &Vars, tokens: &[Tok]) -> Option<String> {
    let inner = match tokens {
        [Tok::Ident(f), Tok::Punct('('), inner @ .., Tok::Punct(')')] if matches!(f.as_str(), "uri" | "URI") => inner,
        other => other,
    };
    vars.eval(inner)
}

fn is_standard_repository(url: &str) -> bool {
    let url = url.trim_end_matches('/');
    [
        "repo.maven.apache.org/maven2",
        "repo1.maven.org/maven2",
        "dl.google.com/dl/android/maven2",
        "maven.google.com",
        "plugins.gradle.org/m2",
        "jcenter.bintray.com",
    ]
    .iter()
    .any(|known| url.contains(known))
}

fn repository_name(url: &str, count: usize) -> String {
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("repo");
    let name: String = host.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() { format!("repo{}", count + 1) } else { name }
}

/// A Kargo package name for a Gradle project name.
fn package_name(project: &str) -> (String, bool) {
    if validate_package_name(project).is_ok() {
        return (project.to_owned(), false);
    }
    let mut name: String = project.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        name.insert_str(0, "pkg-");
    }
    (name, true)
}

/// Plugins Kargo (or this importer) deals with, or that do nothing a Kargo project needs.
fn is_handled(id: &str) -> bool {
    id.starts_with("org.jetbrains.kotlin.")
        || matches!(
            id,
            "java"
                | "java-library"
                | "application"
                | "base"
                | "idea"
                | "eclipse"
                | "org.springframework.boot"
                | "io.spring.dependency-management"
                | "io.ktor.plugin"
                | "org.jetbrains.compose"
                | "com.google.devtools.ksp"
                | "com.android.application"
                | "com.android.library"
                | "io.freefair.lombok"
        )
}

/// What replaces a well-known plugin.
fn advice(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "maven-publish" | "signing" | "com.vanniktech.maven.publish" | "com.gradleup.nmcp" => {
            ("`kargo publish` publishes to Maven repositories and Maven Central", "guides/publishing/")
        }
        "com.github.johnrengelman.shadow" | "com.gradleup.shadow" | "shadow" => {
            ("`kargo package` builds a fat jar", "guides/distributing/")
        }
        "org.jlleitschuh.gradle.ktlint" | "com.diffplug.spotless" => ("`kargo fmt` runs ktlint", "guides/formatting-and-linting/"),
        "io.gitlab.arturbosch.detekt" => ("`kargo lint` runs detekt", "guides/formatting-and-linting/"),
        "org.jetbrains.dokka" => ("`kargo doc` runs Dokka", "guides/documentation/"),
        "org.graalvm.buildtools.native" => ("`kargo package --format native-image` builds a GraalVM executable", "guides/distributing/"),
        "com.github.ben-manes.versions" => ("`kargo outdated` lists newer versions", "reference/commands/"),
        "jacoco" | "org.sonarqube" => {
            ("Kargo has no coverage or code-quality plugin; run the tool from a `[tasks]` entry", "guides/tasks-and-plugins/")
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
