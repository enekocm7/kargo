//! Android support: Kargo generates a Gradle project (Android Gradle Plugin)
//! under `target/android` that points at the Kargo source layout, then drives
//! Gradle. Android packaging (resources, dexing, signing) is AGP's domain.
//!
//! The Gradle that runs the build is, in order of precedence: a `gradlew` in the
//! project root, the Gradle on `PATH` with `--system-gradle`, or a Kargo-managed
//! distribution (`gradle-version`, by default the version the AGP in use needs).
//! With a managed Gradle, the generated project also gets a Gradle wrapper for it,
//! so it builds on its own and opens in Android Studio.
//!
//! Layout used for Android:
//! ```text
//! src/, src-android/          Kotlin/Java sources
//! tests/, tests-android/      local unit tests
//! android/AndroidManifest.xml
//! android/res/, android/assets/
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::android_device::{self, Adb, AndroidRun, Selection, Toolbox};
use crate::builtins;
use crate::ops::build::{announce, compiling};
use crate::ops::context::BuildContext;
use crate::ops::test::{emit_report_event, emit_summary, human_out};
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::SourceSet;
use kargo_compiler::diagnostics::Diagnostic;
use kargo_compiler::testreport::{self, TestSummary};
use kargo_manifest::{
    AndroidConfig, AndroidLint, AndroidNdk, AndroidSdkRequest, AndroidTarget, BuildConfigValue, Dependency, GeneratedKind, Manifest, Scope,
    TargetKind,
};
use kargo_maven::Repository;
use kargo_maven::repo::Content;
use kargo_toolchain::android_sdk::{self, Licenses};
use kargo_toolchain::gradle::{self as toolchain_gradle, AgpGradle};
use kargo_toolchain::{GradleDistribution, Jdk};
use kargo_util::config::Config;
use kargo_util::process;
use kargo_util::settings::Settings;

pub const AGP_VERSION: &str = "9.4.1";

/// The baseline of Android lint when `[target.android.lint] baseline` is not set.
pub const DEFAULT_LINT_BASELINE: &str = "lint-baseline.xml";

/// The `lint.xml` of Android lint when `[target.android.lint] config` is not set.
const DEFAULT_LINT_CONFIG: &str = "lint.xml";

/// The Gradle property that lets the `lint` block name a baseline that does not exist yet.
pub const CREATE_BASELINE_PROPERTY: &str = "kargo.lint.createBaseline";
pub const COMPOSE_BOM_VERSION: &str = "2026.09.00";
pub const ACTIVITY_COMPOSE_VERSION: &str = "1.13.0";
pub const JUNIT4_VERSION: &str = "4.13.2";

fn kts_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$");
    format!("\"{escaped}\"")
}

fn kts_path(p: &Path) -> String {
    kts_string(&p.to_string_lossy().replace('\\', "/"))
}

fn coord(d: &Dependency) -> String {
    let mut s = d.module.to_string();
    if let Some(v) = &d.version {
        s.push(':');
        s.push_str(v);
    }
    if let Some(c) = &d.classifier {
        if d.version.is_none() {
            s.push(':');
        }
        s.push(':');
        s.push_str(c);
    }
    if let Some(e) = &d.extension {
        s.push('@');
        s.push_str(e);
    }
    s
}

/// The Gradle configuration of a dependency: `api` for an `exported` one, which the package's
/// consumers compile against too.
fn configuration(scope: Scope, test: bool, exported: bool) -> &'static str {
    match (scope, test) {
        (Scope::Compile, false) if exported => "api",
        (Scope::Processor, true) => "testAnnotationProcessor",
        (Scope::Processor, false) => "annotationProcessor",
        (Scope::Ksp, true) => "kspTest",
        (Scope::Ksp, false) => "ksp",
        (_, true) => "testImplementation",
        (Scope::Compile, false) => "implementation",
        (Scope::Runtime, false) => "runtimeOnly",
        (Scope::Provided, false) => "compileOnly",
    }
}

fn dependency_line(d: &Dependency, test: bool) -> String {
    dependency_line_in(d, configuration(d.scope, test, d.exported))
}

/// The Gradle line of `d` in the configuration `conf`.
fn dependency_line_in(d: &Dependency, conf: &str) -> String {
    if d.bom {
        return format!("    {conf}(platform({}))", kts_string(&coord(d)));
    }
    let mut line = format!("    {conf}({})", kts_string(&coord(d)));
    let mut body = Vec::new();
    for ex in &d.exclusions {
        if ex.artifact == "*" {
            body.push(format!("exclude(group = {})", kts_string(&ex.group)));
        } else {
            body.push(format!("exclude(group = {}, module = {})", kts_string(&ex.group), kts_string(&ex.artifact)));
        }
    }
    if !d.transitive {
        body.push("isTransitive = false".to_owned());
    }
    if d.force {
        body.push(format!("version {{ strictly({}) }}", kts_string(d.version.as_deref().unwrap_or(""))));
    }
    if !body.is_empty() {
        line.push_str(&format!(" {{ {} }}", body.join("; ")));
    }
    line
}

/// The Gradle Plugin Portal, which `pluginManagement` searches after Google and Maven Central.
const GRADLE_PLUGIN_PORTAL: &str = "https://plugins.gradle.org/m2";

/// Renders `settings.gradle.kts` for a single-project build.
pub fn settings_gradle(manifest: &Manifest, settings: &Settings) -> String {
    settings_gradle_with(manifest, &[], settings)
}

/// One repository of `settings.gradle.kts` (`name` and `url`): its mirror from `[mirrors]` when there
/// is one (with the environment variables of `[mirrors.credentials]`), else `builtin` (such as
/// `google()`) for a repository Gradle knows, or a `maven` block with the repository's own credentials.
/// Its `exclude-groups` become a `content` block, and its `include-groups` an `exclusiveContent` block, so
/// that Gradle asks no other repository for those groups either.
fn gradle_repository(
    out: &mut String,
    seen: &mut Vec<String>,
    settings: &Settings,
    repo: (&str, &str),
    builtin: Option<&str>,
    has_credentials: bool,
    content: &Content,
) {
    let (name, url) = repo;
    let mirror = if url.contains("://") && !url.starts_with("file:") { settings.repository_mirror(name, url) } else { None };
    let url = mirror.unwrap_or(url);
    if seen.iter().any(|u| u == url) {
        return;
    }
    seen.push(url.to_owned());
    let mut excludes = String::new();
    if !content.exclude_groups.is_empty() {
        excludes.push_str("content {\n");
        for pattern in &content.exclude_groups {
            let _ = writeln!(excludes, "    {}", group_filter("exclude", pattern));
        }
        excludes.push_str("}\n");
    }
    let mut block = String::new();
    match (mirror, builtin) {
        (None, Some(builtin)) if excludes.is_empty() => {
            let _ = writeln!(block, "{builtin}");
        }
        (None, Some(builtin)) => {
            let _ = write!(block, "{} {{\n{}}}\n", builtin.trim_end_matches("()"), indent(&excludes, 4));
        }
        _ => {
            let _ = writeln!(block, "maven {{\n    url = uri({})", kts_string(url));
            let env = |user: &str, password: &str| {
                format!(
                    "    credentials {{\n        username = providers.environmentVariable({}).orNull\n        password = providers.environmentVariable({}).orNull\n    }}\n",
                    kts_string(user),
                    kts_string(password)
                )
            };
            if has_credentials && mirror.is_none() {
                let suffix = name.to_uppercase().replace('-', "_");
                block.push_str(&env(&format!("KARGO_REPO_USER_{suffix}"), &format!("KARGO_REPO_PASSWORD_{suffix}")));
            } else if let Some(creds) = settings.credentials_env(url) {
                block.push_str(&env(&creds.username_env, &creds.password_env));
            }
            block.push_str(&indent(&excludes, 4));
            block.push_str("}\n");
        }
    }
    if !content.include_groups.is_empty() {
        let mut filter = String::new();
        for pattern in &content.include_groups {
            let _ = writeln!(filter, "{}", group_filter("include", pattern));
        }
        block = format!(
            "exclusiveContent {{\n    forRepository {{\n{}    }}\n    filter {{\n{}    }}\n}}\n",
            indent(&block, 8),
            indent(&filter, 8)
        );
    }
    out.push_str(&indent(&block, 8));
}

/// The Gradle call that includes or excludes (`verb`) the groups of a group pattern (`com.corp`, or `com.corp.*`).
fn group_filter(verb: &str, pattern: &str) -> String {
    match pattern.strip_suffix(".*") {
        Some(group) => format!("{verb}GroupByRegex({})", kts_string(&format!(r"{}(\..*)?", group.replace('.', r"\.")))),
        None => format!("{verb}Group({})", kts_string(pattern)),
    }
}

/// `text` with every line indented by `width` spaces.
fn indent(text: &str, width: usize) -> String {
    text.lines().map(|line| format!("{:width$}{line}\n", "")).collect()
}

/// Renders `settings.gradle.kts`: the root project named after `manifest`'s package, and the
/// `modules` (name and manifest) as subprojects in `modules/<name>`. The repositories of all of them
/// apply, each replaced by its mirror from `settings`.
pub fn settings_gradle_with(manifest: &Manifest, modules: &[(&str, &Manifest)], settings: &Settings) -> String {
    let google = ("google", kargo_maven::repo::GOOGLE);
    let central = ("maven-central", kargo_maven::repo::MAVEN_CENTRAL);
    let portal = ("gradle-plugin-portal", GRADLE_PLUGIN_PORTAL);
    let mut plugin_repos = String::new();
    let mut seen = Vec::new();
    let all = Content::default();
    gradle_repository(&mut plugin_repos, &mut seen, settings, google, Some("google()"), false, &all);
    gradle_repository(&mut plugin_repos, &mut seen, settings, central, Some("mavenCentral()"), false, &all);
    gradle_repository(&mut plugin_repos, &mut seen, settings, portal, Some("gradlePluginPortal()"), false, &all);
    let mut repos = String::new();
    let mut seen = Vec::new();
    let declared: Vec<&Repository> = manifest.repositories.iter().chain(modules.iter().flat_map(|(_, m)| &m.repositories)).collect();
    // Google and Maven Central first, when the packages use them (`[repositories] defaults = false` leaves them out).
    for (builtin, call) in [(google, "google()"), (central, "mavenCentral()")] {
        if let Some(repo) = declared.iter().find(|r| r.url == builtin.1) {
            gradle_repository(&mut repos, &mut seen, settings, builtin, Some(call), false, &repo.content);
        }
    }
    for repo in declared {
        if repo.url == kargo_maven::repo::MAVEN_CENTRAL || repo.url == kargo_maven::repo::GOOGLE {
            continue;
        }
        gradle_repository(&mut repos, &mut seen, settings, (&repo.name, &repo.url), None, repo.credentials.is_some(), &repo.content);
    }
    let mut includes = String::new();
    for (name, _) in modules {
        let _ = writeln!(includes, "include({})", kts_string(&format!(":{name}")));
        let _ = writeln!(
            includes,
            "project({}).projectDir = file({})",
            kts_string(&format!(":{name}")),
            kts_string(&format!("modules/{name}"))
        );
    }
    if !includes.is_empty() {
        includes.insert(0, '\n');
    }
    format!(
        r#"// Generated by Kargo from kargo.toml. Do not edit: changes are overwritten.
pluginManagement {{
    repositories {{
{plugin_repos}    }}
}}

dependencyResolutionManagement {{
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {{
{repos}    }}
}}

rootProject.name = {name}
{includes}"#,
        name = kts_string(&manifest.package.name)
    )
}

/// A Gradle plugin a package's build applies (or, `applied = false`, only puts on the classpath).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradlePlugin {
    pub id: String,
    pub version: String,
    pub applied: bool,
}

/// The Gradle plugins of an Android package.
pub fn plugins(manifest: &Manifest, android: &AndroidTarget) -> Vec<GradlePlugin> {
    let plugin = |id: &str, version: &str, applied: bool| GradlePlugin { id: id.to_owned(), version: version.to_owned(), applied };
    let cfg = &android.config;
    let kotlin = &manifest.kotlin.version;
    let fw = &manifest.frameworks;
    let android_plugin = if cfg.kind == "application" { "com.android.application" } else { "com.android.library" };
    // AGP's built-in Kotlin compiles with the Kotlin Gradle plugin on the
    // build classpath; pin it to `[kotlin] version` instead of AGP's default.
    let mut out = vec![plugin(android_plugin, agp_version(cfg), true), plugin("org.jetbrains.kotlin.android", kotlin, false)];
    if fw.compose.is_some() {
        out.push(plugin("org.jetbrains.kotlin.plugin.compose", kotlin, true));
    }
    if fw.serialization.is_some() {
        out.push(plugin("org.jetbrains.kotlin.plugin.serialization", kotlin, true));
    }
    if fw.lombok.is_some() {
        out.push(plugin("org.jetbrains.kotlin.plugin.lombok", kotlin, true));
    }
    let uses_ksp = manifest.dependencies_for(TargetKind::Android).iter().chain(&manifest.dev_dependencies).any(|d| d.scope == Scope::Ksp);
    if uses_ksp {
        out.push(plugin("com.google.devtools.ksp", builtins::ksp_version(manifest), true));
    }
    out
}

/// How one package's `build.gradle.kts` fits into the generated build.
#[derive(Debug, Clone, Default)]
pub struct GradleModule {
    /// A subproject: its plugins come without versions, which the root project declares.
    pub subproject: bool,
    /// Plugins of the subprojects that the root project puts on the classpath (`apply false`).
    pub subproject_plugins: Vec<GradlePlugin>,
    /// Lines for the `dependencies` block, such as `implementation(project(":core"))`.
    pub dependencies: Vec<String>,
    /// The output directories of the package's `[[generate]]` steps, added to its source sets.
    pub generated: GeneratedDirs,
    /// Where a subproject builds: its package's own `target/android/build`, so its AAR is where
    /// `kargo build` of that package puts it.
    pub build_dir: Option<PathBuf>,
}

/// The directories `[[generate]]` steps write for the Android target, by what they hold.
#[derive(Debug, Clone, Default)]
pub struct GeneratedDirs {
    pub sources: Vec<PathBuf>,
    pub test_sources: Vec<PathBuf>,
    pub resources: Vec<PathBuf>,
}

impl GeneratedDirs {
    fn of(ws: &Workspace, profile: &ProfileKind) -> Self {
        let dirs = |kind| crate::generate::dirs(ws, profile, TargetKind::Android, kind);
        Self {
            sources: dirs(GeneratedKind::Sources),
            test_sources: dirs(GeneratedKind::TestSources),
            resources: dirs(GeneratedKind::Resources),
        }
    }
}

/// Renders `build.gradle.kts` for a single-project Android build.
pub fn build_gradle(ws: &Workspace, android: &AndroidTarget) -> Result<String> {
    build_gradle_with(ws, android, &GradleModule::default())
}

/// Renders `build.gradle.kts` for one Android package of the generated build.
pub fn build_gradle_with(ws: &Workspace, android: &AndroidTarget, module: &GradleModule) -> Result<String> {
    let manifest = ws.manifest();
    let cfg = &android.config;
    let kotlin = &manifest.kotlin.version;
    let fw = &manifest.frameworks;
    let is_app = cfg.kind == "application";
    let root = ws.root();

    let own = plugins(manifest, android);
    let mut plugins: Vec<String> = Vec::new();
    for p in &own {
        match (module.subproject, p.applied) {
            (true, false) => {}
            (true, true) => plugins.push(format!("    id({})", kts_string(&p.id))),
            (false, true) => plugins.push(format!("    id({}) version {}", kts_string(&p.id), kts_string(&p.version))),
            (false, false) => plugins.push(format!("    id({}) version {} apply false", kts_string(&p.id), kts_string(&p.version))),
        }
    }
    for p in &module.subproject_plugins {
        let line = format!("    id({}) version {} apply false", kts_string(&p.id), kts_string(&p.version));
        if !own.iter().any(|o| o.id == p.id) && !plugins.contains(&line) {
            plugins.push(line);
        }
    }

    let mut s = String::new();
    let _ = writeln!(s, "// Generated by Kargo from kargo.toml. Do not edit: changes are overwritten.");
    let _ = writeln!(s, "plugins {{\n{}\n}}\n", plugins.join("\n"));
    if let Some(dir) = &module.build_dir {
        let _ = writeln!(s, "layout.buildDirectory.set(file({}))\n", kts_path(dir));
    }
    let _ = writeln!(s, "android {{");
    let _ = writeln!(s, "    namespace = {}", kts_string(&cfg.namespace));
    let _ = writeln!(s, "    compileSdk = {}", cfg.compile_sdk);
    if let Some(version) = &cfg.build_tools {
        let _ = writeln!(s, "    buildToolsVersion = {}", kts_string(version));
    }
    if let Some(version) = cfg.ndk.as_ref().and_then(|n| n.version.as_ref()) {
        let _ = writeln!(s, "    ndkVersion = {}", kts_string(version));
    }
    let _ = writeln!(s, "\n    defaultConfig {{");
    if is_app {
        let _ = writeln!(s, "        applicationId = {}", kts_string(cfg.application_id.as_deref().unwrap_or(&cfg.namespace)));
        let _ = writeln!(s, "        targetSdk = {}", cfg.target_sdk);
        let _ = writeln!(s, "        versionCode = {}", cfg.version_code);
        let _ = writeln!(s, "        versionName = {}", kts_string(cfg.version_name.as_deref().unwrap_or(&manifest.package.version)));
    }
    let _ = writeln!(s, "        minSdk = {}", cfg.min_sdk);
    for file in &cfg.consumer_proguard_files {
        let _ = writeln!(s, "        consumerProguardFiles({})", kts_path(&root.join(file)));
    }
    build_config_fields(&mut s, "        ", &cfg.build_config);
    manifest_placeholders(&mut s, "        ", &cfg.manifest_placeholders);
    ndk_default_config(&mut s, cfg.ndk.as_ref());
    let _ = writeln!(s, "    }}");
    external_native_build(&mut s, cfg.ndk.as_ref(), root);
    android_resources(&mut s, cfg);
    lint_options(&mut s, cfg, root);
    signing_and_build_types(&mut s, cfg, root, is_app);
    product_flavors(&mut s, cfg);
    let mut features = Vec::new();
    if fw.compose.is_some() {
        features.push("        compose = true");
    }
    if has_build_config(cfg) {
        features.push("        buildConfig = true");
    }
    if !features.is_empty() {
        let _ = writeln!(s, "\n    buildFeatures {{\n{}\n    }}", features.join("\n"));
    }
    let compose_resources = has_compose_resources(ws).then(|| compose_resources_dir(ws));
    let mut src_dirs = vec![root.join("src"), root.join(TargetKind::Android.source_dir_name())];
    src_dirs.extend(module.generated.sources.iter().cloned());
    // The generated `Res` class of `composeResources/`, and the files it reads as assets.
    let mut asset_dirs = vec![root.join("android/assets")];
    if let Some(dir) = &compose_resources {
        src_dirs.push(dir.join("kotlin"));
        asset_dirs.push(dir.join("resources"));
    }
    let mut test_dirs = vec![root.join("tests"), root.join("tests-android")];
    test_dirs.extend(module.generated.test_sources.iter().cloned());
    let mut resource_dirs = vec![ws.resources()];
    resource_dirs.extend(module.generated.resources.iter().cloned());
    let dirs = |ds: &[PathBuf]| ds.iter().map(|d| kts_path(d)).collect::<Vec<_>>().join(", ");
    // AGP's built-in Kotlin does not compile Kotlin files found through the
    // Java source directories, so the Kotlin directories are registered too.
    let kotlin_dirs = |s: &mut String, ds: &[PathBuf]| {
        for d in ds {
            let _ = writeln!(s, "            kotlin.directories.add({})", kts_path(d));
        }
    };
    let _ = writeln!(s, "\n    sourceSets {{");
    let _ = writeln!(s, "        getByName(\"main\") {{");
    let _ = writeln!(s, "            java.setSrcDirs(listOf({}))", dirs(&src_dirs));
    kotlin_dirs(&mut s, &src_dirs);
    let _ = writeln!(s, "            manifest.srcFile({})", kts_path(&root.join("android/AndroidManifest.xml")));
    let _ = writeln!(s, "            res.setSrcDirs(listOf({}))", kts_path(&root.join("android/res")));
    let _ = writeln!(s, "            assets.setSrcDirs(listOf({}))", dirs(&asset_dirs));
    // Prebuilt native libraries, one directory per ABI (`arm64-v8a/libfoo.so`).
    let _ = writeln!(s, "            jniLibs.setSrcDirs(listOf({}))", kts_path(&root.join("android/jniLibs")));
    let _ = writeln!(s, "            resources.setSrcDirs(listOf({}))", dirs(&resource_dirs));
    let _ = writeln!(s, "        }}");
    let _ = writeln!(s, "        getByName(\"test\") {{");
    let _ = writeln!(s, "            java.setSrcDirs(listOf({}))", dirs(&test_dirs));
    kotlin_dirs(&mut s, &test_dirs);
    let _ = writeln!(s, "            resources.setSrcDirs(listOf({}))", kts_path(&ws.test_resources()));
    let _ = writeln!(s, "        }}");
    let _ = writeln!(s, "    }}");
    // Like the other targets, a test filter that matches nothing is not an error.
    let _ = writeln!(s, "\n    testOptions {{\n        unitTests.all {{ it.filter.isFailOnNoMatchingTests = false }}\n    }}");
    let _ = writeln!(s, "}}\n");

    // Compiler options.
    let mut free_args: Vec<String> = manifest.kotlin.compiler_args.clone();
    let contributions = builtins::contributions(manifest, TargetKind::Android, root)?;
    free_args.extend(contributions.compiler_args.iter().cloned());
    // The Compose Gradle plugin applies the compiler plugin; its options go in as `-P` arguments,
    // like on the JVM target.
    if let Some(compose) = &manifest.frameworks.compose {
        builtins::check_compose_stability_config(manifest, root)?;
        for (key, value) in builtins::compose_compiler_options(compose, root, &ws.work_dir("compose"), TargetKind::Android) {
            free_args.push("-P".to_owned());
            free_args.push(format!("plugin:{}:{key}={value}", builtins::COMPOSE_PLUGIN_ID));
        }
    }
    let _ = writeln!(s, "kotlin {{\n    compilerOptions {{");
    if !free_args.is_empty() {
        let args: Vec<String> = free_args.iter().map(|a| kts_string(a)).collect();
        let _ = writeln!(s, "        freeCompilerArgs.addAll({})", args.join(", "));
    }
    if !manifest.kotlin.opt_in.is_empty() {
        let opts: Vec<String> = manifest.kotlin.opt_in.iter().map(|a| kts_string(a)).collect();
        let _ = writeln!(s, "        optIn.addAll({})", opts.join(", "));
    }
    if manifest.kotlin.warnings_as_errors {
        let _ = writeln!(s, "        allWarningsAsErrors.set(true)");
    }
    let _ = writeln!(s, "    }}\n}}\n");

    // `expect` declarations of `src/` and their `actual`s in `src-android/`: AGP compiles the module as
    // plain Android Kotlin, so the compiler is told about the two fragments like the other targets'.
    let mut common_dirs = vec![root.join("src")];
    common_dirs.extend(module.generated.sources.iter().cloned());
    if let Some(dir) = &compose_resources {
        common_dirs.push(dir.join("kotlin"));
    }
    let mut common_test_dirs = vec![root.join("tests")];
    common_test_dirs.extend(module.generated.test_sources.iter().cloned());
    let main_fragments = fragment_args(&common_dirs, &root.join(TargetKind::Android.source_dir_name()));
    let test_fragments = fragment_args(&common_test_dirs, &root.join("tests-android"));
    if !main_fragments.is_empty() || !test_fragments.is_empty() {
        let list = |args: &[String]| format!("listOf({})", args.iter().map(|a| kts_string(a)).collect::<Vec<_>>().join(", "));
        let _ = writeln!(s, "tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {{");
        let _ = writeln!(s, "    val unitTest = name.contains(\"UnitTest\")");
        let _ = writeln!(
            s,
            "    compilerOptions.freeCompilerArgs.addAll(if (unitTest) {} else {})",
            list(&test_fragments),
            list(&main_fragments)
        );
        let _ = writeln!(s, "}}\n");
    }

    // Dependencies.
    let _ = writeln!(s, "dependencies {{");
    let mut seen = Vec::new();
    let mut push = |s: &mut String, d: &Dependency, test: bool| {
        let key = (d.module.clone(), d.classifier.clone(), d.extension.clone(), test);
        if !seen.contains(&key) {
            seen.push(key);
            let _ = writeln!(s, "{}", dependency_line(d, test));
        }
    };
    for d in manifest.dependencies_for(TargetKind::Android) {
        push(&mut s, &d, false);
    }
    // The dependencies of the dev and release profiles are in the configurations of their build types.
    for (profile, build_type) in [("dev", "debug"), ("release", "release")] {
        for d in &manifest.profile(profile).expect("built-in profile").dependencies {
            let _ = writeln!(s, "{}", profile_dependency_line(d, profile, build_type)?);
        }
    }
    for d in
        contributions.dependencies.iter().filter(|d| !(d.module.group == "org.jetbrains.kotlin" && d.module.artifact == "kotlin-stdlib"))
    {
        push(&mut s, d, false);
    }
    if let Some(co) = &fw.coroutines {
        let v = co.version.as_deref().unwrap_or(builtins::COROUTINES_VERSION);
        push(&mut s, &Dependency::new("org.jetbrains.kotlinx:kotlinx-coroutines-android".parse()?, Some(v.to_owned())), false);
    }
    if fw.compose.is_some() {
        let mut bom = Dependency::new("androidx.compose:compose-bom".parse()?, Some(COMPOSE_BOM_VERSION.to_owned()));
        bom.bom = true;
        push(&mut s, &bom, false);
        for m in ["androidx.compose.ui:ui", "androidx.compose.ui:ui-tooling-preview", "androidx.compose.material3:material3"] {
            push(&mut s, &Dependency::new(m.parse()?, None), false);
        }
        push(&mut s, &Dependency::new("androidx.activity:activity-compose".parse()?, Some(ACTIVITY_COMPOSE_VERSION.to_owned())), false);
        if compose_resources.is_some() {
            let version = fw.compose.as_ref().and_then(|c| c.version.clone()).unwrap_or_else(|| builtins::COMPOSE_VERSION.to_owned());
            push(&mut s, &Dependency::new(crate::compose::resources::RUNTIME.parse()?, Some(version)), false);
        }
    }
    if fw.lombok.is_some() {
        let v = fw.lombok.as_ref().and_then(|l| l.version.clone()).unwrap_or_else(|| builtins::LOMBOK_VERSION.to_owned());
        let _ = writeln!(s, "    annotationProcessor({})", kts_string(&format!("org.projectlombok:lombok:{v}")));
    }
    for line in &module.dependencies {
        let _ = writeln!(s, "{line}");
    }
    for d in &manifest.dev_dependencies {
        push(&mut s, d, true);
    }
    push(&mut s, &Dependency::new("org.jetbrains.kotlin:kotlin-test-junit".parse()?, Some(kotlin.clone())), true);
    push(&mut s, &Dependency::new("junit:junit".parse()?, Some(JUNIT4_VERSION.to_owned())), true);
    let _ = writeln!(s, "}}");
    s.push_str(&doc_classpath_task_kts(&variant_name(&default_flavors(cfg, &[])?, "debug")));
    Ok(s)
}

/// The compiler arguments that make the Kotlin files of `common` and of `platform` two fragments of one
/// multiplatform compilation, so `expect` and `actual` declarations match. Empty unless both have Kotlin files.
fn fragment_args(common: &[PathBuf], platform: &Path) -> Vec<String> {
    let files = |dirs: &[PathBuf]| dirs.iter().flat_map(|d| kargo_util::fs::collect_files(d, &["kt"])).collect::<Vec<_>>();
    SourceSet::new(files(common), files(&[platform.to_path_buf()]), TargetKind::Android.as_str()).fragment_args()
}

/// The Gradle line of a `[profile.<profile>.dependencies]` entry, in the configuration of its build
/// type: `debugImplementation` for the dev profile, so only debug builds get the dependency.
/// Processors and KSP are configured per variant in Gradle, so they are not supported here.
fn profile_dependency_line(d: &Dependency, profile: &str, build_type: &str) -> Result<String> {
    if matches!(d.scope, Scope::Processor | Scope::Ksp) {
        bail!(
            "`{}` in [profile.{profile}.dependencies] needs a scope the Android target cannot give to one build type; use [dependencies]",
            d.name
        );
    }
    Ok(dependency_line_in(d, &format!("{build_type}{}", capitalize(configuration(d.scope, false, d.exported)))))
}

/// Whether the Android build of `ws` gets a `Res` class: Compose and a `composeResources/` directory.
fn has_compose_resources(ws: &Workspace) -> bool {
    ws.manifest().frameworks.compose.is_some() && crate::compose::resources::present(ws.root())
}

/// Where [`generate`] writes the `Res` class and the assets of `composeResources/` for `ws`.
fn compose_resources_dir(ws: &Workspace) -> PathBuf {
    crate::compose::resources::output_dir(&ws.work_dir("android"))
}

fn has_build_config(cfg: &AndroidConfig) -> bool {
    !cfg.build_config.is_empty() || cfg.flavors.values().any(|f| !f.build_config.is_empty())
}

/// Renders the `${placeholders}` of `AndroidManifest.xml`.
fn manifest_placeholders(s: &mut String, indent: &str, placeholders: &BTreeMap<String, BuildConfigValue>) {
    for (name, value) in placeholders {
        let _ = writeln!(s, "{indent}manifestPlaceholders[{}] = {}", kts_string(name), kts_string(&value.text()));
    }
}

/// Renders `"a", "b"`: Kotlin strings separated by commas.
fn kts_list(items: &[String]) -> String {
    items.iter().map(|i| kts_string(i)).collect::<Vec<_>>().join(", ")
}

/// Renders the ABI filters and CMake arguments of `defaultConfig`.
fn ndk_default_config(s: &mut String, ndk: Option<&AndroidNdk>) {
    let Some(ndk) = ndk else { return };
    if !ndk.abi_filters.is_empty() {
        let _ = writeln!(s, "        ndk {{\n            abiFilters.addAll(listOf({}))\n        }}", kts_list(&ndk.abi_filters));
    }
    if ndk.cmake.is_some() && (!ndk.cmake_arguments.is_empty() || !ndk.cpp_flags.is_empty()) {
        let _ = writeln!(s, "        externalNativeBuild {{\n            cmake {{");
        if !ndk.cmake_arguments.is_empty() {
            let _ = writeln!(s, "                arguments({})", kts_list(&ndk.cmake_arguments));
        }
        if !ndk.cpp_flags.is_empty() {
            let _ = writeln!(s, "                cppFlags({})", kts_list(&ndk.cpp_flags));
        }
        let _ = writeln!(s, "            }}\n        }}");
    }
}

/// Renders the CMake or ndk-build project that builds the native code.
fn external_native_build(s: &mut String, ndk: Option<&AndroidNdk>, root: &Path) {
    let Some(ndk) = ndk else { return };
    if let Some(cmake) = &ndk.cmake {
        let _ = writeln!(s, "\n    externalNativeBuild {{\n        cmake {{");
        let _ = writeln!(s, "            path = file({})", kts_path(&root.join(cmake)));
        if let Some(version) = &ndk.cmake_version {
            let _ = writeln!(s, "            version = {}", kts_string(version));
        }
        let _ = writeln!(s, "        }}\n    }}");
    } else if let Some(mk) = &ndk.ndk_build {
        let _ = writeln!(s, "\n    externalNativeBuild {{\n        ndkBuild {{");
        let _ = writeln!(s, "            path = file({})", kts_path(&root.join(mk)));
        let _ = writeln!(s, "        }}\n    }}");
    }
}

/// Renders the resource filters.
fn android_resources(s: &mut String, cfg: &AndroidConfig) {
    if !cfg.locales.is_empty() {
        let _ = writeln!(s, "\n    androidResources {{\n        localeFilters.addAll(listOf({}))\n    }}", kts_list(&cfg.locales));
    }
}

/// Renders the `lint` block: the baseline, the severity overrides and the reports.
///
/// The baseline and `lint.xml` are named explicitly, since AGP would look for them in the generated project
/// instead of the package. A baseline that does not exist yet is only used with `-Pkargo.lint.createBaseline`
/// (`kargo lint --android --create-baseline`), because AGP fails a lint run that has to create one.
fn lint_options(s: &mut String, cfg: &AndroidConfig, root: &Path) {
    let lint = &cfg.lint;
    let _ = writeln!(s, "\n    lint {{");
    let baseline = root.join(lint.baseline.as_deref().unwrap_or(Path::new(DEFAULT_LINT_BASELINE)));
    let _ = writeln!(s, "        val baselineFile = file({})", kts_path(&baseline));
    let _ = writeln!(
        s,
        "        if (baselineFile.exists() || providers.gradleProperty({}).isPresent) {{ baseline = baselineFile }}",
        kts_string(CREATE_BASELINE_PROPERTY)
    );
    let config = root.join(lint.config.as_deref().unwrap_or(Path::new(DEFAULT_LINT_CONFIG)));
    if lint.config.is_some() || config.is_file() {
        let _ = writeln!(s, "        lintConfig = file({})", kts_path(&config));
    }
    if let Some(abort) = lint.abort_on_error {
        let _ = writeln!(s, "        abortOnError = {abort}");
    }
    if lint.warnings_as_errors {
        let _ = writeln!(s, "        warningsAsErrors = true");
    }
    if lint.check_dependencies {
        let _ = writeln!(s, "        checkDependencies = true");
    }
    for (property, ids) in [
        ("disable", &lint.disable),
        ("enable", &lint.enable),
        ("fatal", &lint.fatal),
        ("error", &lint.error),
        ("warning", &lint.warning),
        ("informational", &lint.informational),
        ("ignore", &lint.ignore),
    ] {
        if !ids.is_empty() {
            let _ = writeln!(s, "        {property}.addAll(listOf({}))", kts_list(ids));
        }
    }
    let reports = lint.reports();
    for report in AndroidLint::REPORTS {
        let _ = writeln!(s, "        {report}Report = {}", reports.contains(&report));
    }
    let _ = writeln!(s, "    }}");
}

fn build_config_fields(s: &mut String, indent: &str, fields: &BTreeMap<String, BuildConfigValue>) {
    for (name, value) in fields {
        let (ty, literal) = value.java();
        let _ = writeln!(s, "{indent}buildConfigField({}, {}, {})", kts_string(ty), kts_string(name), kts_string(&literal));
    }
}

/// Renders `signingConfigs` and `buildTypes`: signing, R8 and resource shrinking.
/// Passwords are read from the environment when Gradle runs, so they never reach a file.
fn signing_and_build_types(s: &mut String, cfg: &AndroidConfig, root: &Path, is_app: bool) {
    if !cfg.signing.is_empty() {
        let _ = writeln!(s, "\n    signingConfigs {{");
        for (build_type, signing) in &cfg.signing {
            // AGP always defines the `debug` signing config (the debug keystore).
            let open = if build_type == "debug" { "getByName" } else { "create" };
            let _ = writeln!(s, "        {open}({}) {{", kts_string(build_type));
            let _ = writeln!(s, "            storeFile = file({})", kts_path(&root.join(&signing.store_file)));
            let env = |var: &str| format!("providers.environmentVariable({}).orNull", kts_string(var));
            let _ = writeln!(s, "            storePassword = {}", env(&signing.store_password_env));
            let _ = writeln!(s, "            keyAlias = {}", kts_string(&signing.key_alias));
            let _ = writeln!(s, "            keyPassword = {}", env(signing.key_password_env()));
            if let Some(store_type) = &signing.store_type {
                let _ = writeln!(s, "            storeType = {}", kts_string(store_type));
            }
            let _ = writeln!(s, "        }}");
        }
        let _ = writeln!(s, "    }}");
    }
    let mut types = String::new();
    for build_type in AndroidConfig::BUILD_TYPES {
        let mut body = Vec::new();
        if build_type == "release" {
            if cfg.minify {
                body.push("isMinifyEnabled = true".to_owned());
                if cfg.shrink_resources.unwrap_or(is_app) {
                    body.push("isShrinkResources = true".to_owned());
                }
            }
            if cfg.minify || !cfg.proguard_files.is_empty() {
                let mut files = vec!["getDefaultProguardFile(\"proguard-android-optimize.txt\")".to_owned()];
                files.extend(cfg.proguard_files.iter().map(|f| format!("file({})", kts_path(&root.join(f)))));
                body.push(format!("proguardFiles({})", files.join(", ")));
            }
        }
        if cfg.signing.contains_key(build_type) {
            body.push(format!("signingConfig = signingConfigs.getByName({})", kts_string(build_type)));
        }
        if !body.is_empty() {
            let _ = writeln!(types, "        getByName({}) {{", kts_string(build_type));
            for line in body {
                let _ = writeln!(types, "            {line}");
            }
            let _ = writeln!(types, "        }}");
        }
    }
    if !types.is_empty() {
        let _ = write!(s, "\n    buildTypes {{\n{types}    }}\n");
    }
}

/// Renders `flavorDimensions` and `productFlavors`.
fn product_flavors(s: &mut String, cfg: &AndroidConfig) {
    if cfg.flavors.is_empty() {
        return;
    }
    let dims: Vec<String> = cfg.dimensions().iter().map(|d| kts_string(d)).collect();
    let _ = writeln!(s, "\n    flavorDimensions += listOf({})", dims.join(", "));
    let _ = writeln!(s, "    productFlavors {{");
    for (name, flavor) in &cfg.flavors {
        let _ = writeln!(s, "        create({}) {{", kts_string(name));
        let _ = writeln!(s, "            dimension = {}", kts_string(&flavor.dimension));
        if let Some(suffix) = &flavor.application_id_suffix {
            let _ = writeln!(s, "            applicationIdSuffix = {}", kts_string(suffix));
        }
        if let Some(suffix) = &flavor.version_name_suffix {
            let _ = writeln!(s, "            versionNameSuffix = {}", kts_string(suffix));
        }
        if let Some(code) = flavor.version_code {
            let _ = writeln!(s, "            versionCode = {code}");
        }
        if let Some(min) = flavor.min_sdk {
            let _ = writeln!(s, "            minSdk = {min}");
        }
        build_config_fields(s, "            ", &flavor.build_config);
        manifest_placeholders(s, "            ", &flavor.manifest_placeholders);
        let _ = writeln!(s, "        }}");
    }
    let _ = writeln!(s, "    }}");
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The AGP variant name of `flavors` (in dimension order) and `build_type`, e.g. `freeRelease`.
pub fn variant_name(flavors: &[String], build_type: &str) -> String {
    let mut name = String::new();
    for part in flavors.iter().map(String::as_str).chain([build_type]) {
        let part = if name.is_empty() { part.to_owned() } else { capitalize(part) };
        name.push_str(&part);
    }
    name
}

/// One flavor per dimension, in dimension order: the one named in `chosen`, else the first
/// of the dimension (by name). Empty without flavors.
pub fn default_flavors(cfg: &AndroidConfig, chosen: &[String]) -> Result<Vec<String>> {
    for name in chosen {
        if !cfg.flavors.contains_key(name) {
            let known: Vec<&str> = cfg.flavors.keys().map(String::as_str).collect();
            if known.is_empty() {
                bail!("unknown flavor `{name}`: [target.android] defines no flavors");
            }
            bail!("unknown flavor `{name}` (expected one of: {})", known.join(", "));
        }
    }
    let mut out = Vec::new();
    for dim in cfg.dimensions() {
        let picked: Vec<&String> = chosen.iter().filter(|c| cfg.flavors[c.as_str()].dimension == dim).collect();
        match picked[..] {
            [] => out.extend(cfg.flavors_of(&dim).next().map(|(n, _)| n.clone())),
            [one] => out.push(one.clone()),
            _ => {
                let names: Vec<String> = picked.iter().map(|p| format!("`{p}`")).collect();
                bail!("pick one flavor of dimension `{dim}`, not {}", names.join(" and "))
            }
        }
    }
    Ok(out)
}

/// Every combination of one flavor per dimension, in dimension order (a single empty one without flavors).
pub fn flavor_combinations(cfg: &AndroidConfig) -> Vec<Vec<String>> {
    let mut combos = vec![Vec::new()];
    for dim in cfg.dimensions() {
        let names: Vec<&String> = cfg.flavors_of(&dim).map(|(n, _)| n).collect();
        combos = combos.into_iter().flat_map(|c| names.iter().map(move |n| [c.clone(), vec![(*n).clone()]].concat())).collect();
    }
    combos
}

/// Gradle task `kargo doc` runs to learn what the main sources compile against.
const DOC_CLASSPATH_TASK: &str = "kargoDocClasspath";

/// The task of [`DOC_CLASSPATH_TASK_KTS`] for `variant` (the default debug variant).
fn doc_classpath_task_kts(variant: &str) -> String {
    DOC_CLASSPATH_TASK_KTS.replace("debugCompileClasspath", &format!("{variant}CompileClasspath"))
}

/// Writes the boot classpath (`android.jar`) and the classes jars of the debug
/// compile classpath, one per line. AGP unpacks AARs into `android-classes-jar` artifacts.
const DOC_CLASSPATH_TASK_KTS: &str = r#"
// Used by `kargo doc`: writes the classpath the main sources compile against.
tasks.register("kargoDocClasspath") {
    val boot = androidComponents.sdkComponents.bootClasspath
    val libraries = configurations.getByName("debugCompileClasspath").incoming.artifactView {
        attributes { attribute(Attribute.of("artifactType", String::class.java), "android-classes-jar") }
    }.files
    val out = layout.buildDirectory.file("kargo/doc-classpath.txt")
    outputs.file(out)
    outputs.upToDateWhen { false }
    doLast {
        val files = boot.get().map { it.asFile } + libraries.files
        out.get().asFile.writeText(files.joinToString("\n") { it.absolutePath })
    }
}
"#;

// Each module's `R` class holds only its own resources; a library's are `<its namespace>.R`.
const GRADLE_PROPERTIES: &str = "# Generated by Kargo.\norg.gradle.jvmargs=-Xmx2g -Dfile.encoding=UTF-8\nandroid.useAndroidX=true\n\
                                 android.nonTransitiveRClass=true\nkotlin.code.style=official\n";

/// Locates the system Android SDK: `ANDROID_HOME`, `ANDROID_SDK_ROOT`, then Android Studio's default location.
pub fn android_sdk() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                return Some(dir);
            }
        }
    }
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Android").join("Sdk")).into_iter().collect()
    } else if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Android/sdk")).into_iter().collect()
    } else {
        dirs::home_dir().map(|h| h.join("Android/Sdk")).into_iter().collect()
    };
    candidates.into_iter().find(|d| d.is_dir())
}

/// Fails unless the Android package `ws` has its `android/AndroidManifest.xml`.
fn require_android_manifest(ws: &Workspace) -> Result<()> {
    let manifest_file = ws.root().join("android/AndroidManifest.xml");
    if !manifest_file.is_file() {
        bail!("Android builds need {}; `kargo new --target android` creates one", manifest_file.display());
    }
    Ok(())
}

/// A path dependency that reaches an Android package as a jar: the package, its scope for the
/// consumer, whether it is part of the consumer's API, and whether only the tests use it.
#[derive(Debug, Clone, Copy)]
struct JarDependency {
    package: usize,
    scope: Scope,
    exported: bool,
    test: bool,
}

/// The `dependencies` lines for the path dependencies of the Android package `package`. A
/// dependency with `[target.android]` (an Android library) is the Gradle subproject `:<name>`,
/// added to `subprojects`; any other one is the JVM jar Kargo builds, with the jars and Maven
/// dependencies it brings along (`exported` ones for compiling, the others at runtime).
fn path_dependency_lines(ctx: &BuildContext, package: usize, subprojects: &mut Vec<usize>) -> Result<Vec<String>> {
    let consumer = ctx.project.package(package);
    let mut lines: Vec<String> = Vec::new();
    let push = |lines: &mut Vec<String>, line: String| {
        if !lines.contains(&line) {
            lines.push(line);
        }
    };
    let mut jars: Vec<JarDependency> = Vec::new();
    let mut queue: std::collections::VecDeque<(usize, JarDependency)> = consumer
        .path_dependencies()
        .iter()
        .filter(|(d, i)| d.target.is_none_or(|t| t == TargetKind::Android) && *i != ctx.package)
        .map(|(d, i)| (package, JarDependency { package: *i, scope: d.scope, exported: d.exported, test: d.dev }))
        .collect();
    while let Some((from, dep)) = queue.pop_front() {
        let pkg = ctx.project.package(dep.package);
        let from = ctx.project.package(from).name();
        if let Some(android) = &pkg.manifest().targets.android {
            if android.config.kind != "library" {
                bail!(
                    "`{from}` depends on `{}`, an Android application; only an Android library (`kind = \"library\"` under [target.android]) can be a dependency",
                    pkg.name()
                );
            }
            require_android_manifest(pkg)?;
            let project = kts_string(&format!(":{}", pkg.name()));
            push(&mut lines, format!("    {}(project({project}))", configuration(dep.scope, dep.test, dep.exported)));
            if !subprojects.contains(&dep.package) {
                subprojects.push(dep.package);
            }
            continue;
        }
        if !pkg.manifest().targets.is_enabled(TargetKind::Jvm) {
            bail!(
                "`{from}` depends on the path dependency `{}`, which enables neither the android nor the jvm target; add [target.android] with kind = \"library\" (or [target.jvm]) to {}",
                pkg.name(),
                pkg.manifest_path().display()
            );
        }
        match jars.iter_mut().find(|j| j.package == dep.package && j.test == dep.test) {
            Some(j) if j.scope == dep.scope || j.scope == Scope::Compile => continue,
            Some(j) => j.scope = Scope::Compile,
            None => jars.push(dep),
        }
        for (inner, child) in pkg.path_dependencies().iter().filter(|(d, _)| d.applies_to(TargetKind::Jvm, false)) {
            if let Some(scope) = crate::ops::context::inherited_scope(dep.scope, inner.scope, inner.exported) {
                let next = JarDependency { package: *child, scope, exported: dep.exported && inner.exported, test: dep.test };
                queue.push_back((dep.package, next));
            }
        }
    }
    for jar in jars {
        let pkg = ctx.project.package(jar.package);
        let file = ctx
            .built_artifact(format!("{}/jvm", pkg.name()), || Ok(Some(crate::ops::build::build_jvm(&ctx.for_package(jar.package))?.jar)))?
            .context("a path dependency built no jar")?;
        push(&mut lines, format!("    {}(files({}))", configuration(jar.scope, jar.test, jar.exported), kts_path(&file)));
        let theirs = builtins::contributions(pkg.manifest(), TargetKind::Jvm, pkg.root())?.dependencies;
        let own = pkg.manifest().dependencies_for(TargetKind::Jvm);
        let mut deps = own.clone();
        deps.extend(theirs.into_iter().filter(|t| !own.iter().any(|d| d.same_artifact(t))));
        for mut d in deps {
            // AGP adds the Kotlin standard library itself.
            if d.module.group == "org.jetbrains.kotlin" && d.module.artifact == "kotlin-stdlib" {
                continue;
            }
            if !d.bom {
                let Some(scope) = crate::ops::context::inherited_scope(jar.scope, d.scope, d.exported) else { continue };
                d.scope = scope;
            }
            d.exported &= jar.exported;
            push(&mut lines, dependency_line(&d, jar.test));
        }
    }
    Ok(lines)
}

/// The Android libraries the generated build of `package` has as subprojects: the ones it reaches
/// through path dependencies, directly or through JVM packages, as [`path_dependency_lines`] finds them.
pub fn android_subprojects(ctx: &BuildContext, package: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let mut queue: std::collections::VecDeque<usize> = ctx
        .project
        .package(package)
        .path_dependencies()
        .iter()
        .filter(|(d, i)| d.target.is_none_or(|t| t == TargetKind::Android) && *i != package)
        .map(|(_, i)| *i)
        .collect();
    while let Some(index) = queue.pop_front() {
        if seen.contains(&index) {
            continue;
        }
        seen.push(index);
        let pkg = ctx.project.package(index);
        let next: Vec<usize> = if pkg.manifest().targets.android.is_some() {
            out.push(index);
            pkg.path_dependencies()
                .iter()
                .filter(|(d, i)| d.target.is_none_or(|t| t == TargetKind::Android) && *i != index)
                .map(|(_, i)| *i)
                .collect()
        } else {
            pkg.path_dependencies().iter().filter(|(d, _)| d.applies_to(TargetKind::Jvm, false)).map(|(_, i)| *i).collect()
        };
        queue.extend(next);
    }
    out
}

/// The package of this invocation whose Gradle run builds the Android library `ctx` as a subproject,
/// if any: another selected Android package that uses it.
fn built_by(ctx: &BuildContext) -> Option<usize> {
    if ctx.ws.manifest().targets.android.as_ref().is_none_or(|a| a.config.kind != "library") {
        return None;
    }
    ctx.selected().into_iter().find(|&other| {
        other != ctx.package
            && ctx.project.package(other).manifest().targets.android.is_some()
            && android_subprojects(ctx, other).contains(&ctx.package)
    })
}

/// Writes the Gradle project; returns its directory. The Android libraries the package depends
/// on through `path` are subprojects of it, in `modules/<name>`, building into their own
/// `target/android/build`.
pub fn generate(ctx: &BuildContext) -> Result<PathBuf> {
    let ws = ctx.ws;
    let android = ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?;
    require_android_manifest(ws)?;
    let dir = ws.work_dir("android");
    std::fs::create_dir_all(&dir)?;
    let write = |name: &str, contents: &str| -> Result<()> {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            kargo_util::fs::write_string_atomic(&path, contents)?;
        }
        Ok(())
    };

    let mut subprojects: Vec<usize> = Vec::new();
    let root_lines = path_dependency_lines(ctx, ctx.package, &mut subprojects)?;
    // A subproject may bring more (an Android library depending on another).
    let mut modules: Vec<(usize, Vec<String>)> = Vec::new();
    while let Some(&next) = subprojects.iter().find(|i| !modules.iter().any(|(m, _)| m == *i)) {
        let lines = path_dependency_lines(ctx, next, &mut subprojects)?;
        modules.push((next, lines));
    }
    let mut subproject_plugins: Vec<GradlePlugin> = Vec::new();
    for (index, lines) in &modules {
        let pkg = ctx.project.package(*index);
        let pkg_android = pkg.manifest().targets.android.as_ref().expect("subprojects are Android packages");
        // One Gradle build loads one Android Gradle Plugin.
        if agp_version(&pkg_android.config) != agp_version(&android.config) {
            bail!(
                "`{}` builds with Android Gradle Plugin {} and its dependency `{}` with {}; the Android packages of one build \
                 share one AGP, so set the same `agp-version` under [target.android] (or leave it out in both)",
                ws.name(),
                agp_version(&android.config),
                pkg.name(),
                agp_version(&pkg_android.config)
            );
        }
        for p in plugins(pkg.manifest(), pkg_android) {
            if !subproject_plugins.iter().any(|s| s.id == p.id) {
                subproject_plugins.push(GradlePlugin { applied: false, ..p });
            }
        }
        crate::compose::resources::generate(pkg.root(), pkg.manifest(), &compose_resources_dir(pkg))?;
        crate::generate::ensure(&ctx.for_package(*index), TargetKind::Android)?;
        let module = GradleModule {
            subproject: true,
            subproject_plugins: Vec::new(),
            dependencies: lines.clone(),
            generated: GeneratedDirs::of(pkg, &ctx.profile),
            build_dir: Some(pkg.work_dir("android").join("build")),
        };
        write(&format!("modules/{}/build.gradle.kts", pkg.name()), &build_gradle_with(pkg, pkg_android, &module)?)?;
    }
    let names: Vec<(&str, &Manifest)> =
        modules.iter().map(|(i, _)| (ctx.project.package(*i).name(), ctx.project.package(*i).manifest())).collect();
    write("settings.gradle.kts", &settings_gradle_with(ws.manifest(), &names, ctx.config.settings()))?;
    crate::compose::resources::generate(ws.root(), ws.manifest(), &compose_resources_dir(ws))?;
    crate::generate::ensure(ctx, TargetKind::Android)?;
    let root = GradleModule {
        subproject: false,
        subproject_plugins,
        dependencies: root_lines,
        generated: GeneratedDirs::of(ws, &ctx.profile),
        build_dir: None,
    };
    write("build.gradle.kts", &build_gradle_with(ws, android, &root)?)?;
    write("gradle.properties", GRADLE_PROPERTIES)?;
    let sdk = sdk(ctx.ws, ctx.config, Licenses::Ask)?;
    let escaped = sdk.to_string_lossy().replace('\\', "\\\\").replace(':', "\\:");
    write("local.properties", &format!("sdk.dir={escaped}\n"))?;
    Ok(dir)
}

/// The SDK packages an Android package builds with: its platform, the build-tools (`build-tools`, else those of
/// its AGP), platform-tools, and the NDK and CMake it pins.
pub(crate) fn sdk_packages(cfg: &AndroidConfig) -> Vec<String> {
    let build_tools = cfg.build_tools.as_deref().unwrap_or_else(|| android_sdk::build_tools_for_agp(agp_version(cfg)));
    let mut packages = android_sdk::packages_for(cfg.compile_sdk, build_tools);
    if let Some(ndk) = &cfg.ndk {
        packages.extend(ndk.version.iter().map(|v| android_sdk::ndk_package(v)));
        packages.extend(ndk.cmake_version.iter().map(|v| android_sdk::cmake_package(v)));
    }
    packages
}

/// The Android SDK the Android target of `ws` builds with. With `[toolchain] android-sdk = "<api>"` it is Kargo's,
/// with the packages installed (`licenses` says how their licenses get accepted); otherwise the system SDK, which
/// must have the packages, or accept the licenses that let AGP download them.
pub fn sdk(ws: &Workspace, config: &Config, licenses: Licenses) -> Result<PathBuf> {
    let cfg = &ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?.config;
    let packages = sdk_packages(cfg);
    match ws.manifest().toolchain.android_sdk {
        AndroidSdkRequest::Platform(api) => {
            android_sdk::ensure(config, &packages, licenses, &format!("kargo toolchain install android-sdk {api}"))
        }
        AndroidSdkRequest::System => {
            let api = cfg.compile_sdk;
            let sdk = android_sdk().with_context(|| {
                format!(
                    "Android SDK not found; set ANDROID_HOME to your SDK directory, or set `android-sdk = \"{api}\"` \
                     under [toolchain] in kargo.toml to have Kargo install it"
                )
            })?;
            let missing = android_sdk::missing(&sdk, &packages[..2]);
            if !missing.is_empty() && !android_sdk::has_accepted_license(&sdk) {
                let quoted: Vec<String> = missing.iter().map(|p| format!("\"{p}\"")).collect();
                bail!(
                    "the Android SDK at {} lacks {}; install with `sdkmanager {}`, or set `android-sdk = \"{api}\"` under \
                     [toolchain] in kargo.toml to have Kargo manage the SDK",
                    sdk.display(),
                    missing.join(" and "),
                    quoted.join(" ")
                );
            }
            Ok(sdk)
        }
    }
}

/// Describes the Android SDK of `ws` for `kargo toolchain which`, without installing anything.
pub fn describe_sdk(ws: &Workspace, config: &Config) -> Result<String> {
    let cfg = &ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?.config;
    let packages = sdk_packages(cfg);
    Ok(match ws.manifest().toolchain.android_sdk {
        AndroidSdkRequest::Platform(api) => {
            let sdk = config.android_sdk_dir();
            match android_sdk::missing(&sdk, &packages).as_slice() {
                [] => format!("{api} (Kargo-managed at {})", sdk.display()),
                missing => format!("{api} (Kargo-managed; {} not installed yet, installed on first android build)", missing.join(", ")),
            }
        }
        AndroidSdkRequest::System => match android_sdk() {
            None => "system (not found; set ANDROID_HOME)".to_owned(),
            Some(sdk) => match android_sdk::missing(&sdk, &packages[..2]).as_slice() {
                [] => format!("system at {}", sdk.display()),
                missing => format!("system at {} ({} missing)", sdk.display(), missing.join(", ")),
            },
        },
    })
}

/// The AGP build type of `profile`. Custom profiles have no build type yet: the generated Gradle
/// project declares only `debug` and `release`.
fn build_type(profile: &ProfileKind) -> Result<&'static str> {
    match profile {
        ProfileKind::Dev => Ok("debug"),
        ProfileKind::Release => Ok("release"),
        ProfileKind::Custom(name) => bail!("the Android target builds the dev and release profiles only; `{name}` is not supported yet"),
    }
}

fn android_config<'a>(ctx: &'a BuildContext) -> Result<&'a AndroidConfig> {
    Ok(&ctx.ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?.config)
}

/// The variant `check`, `test` and `doc` use: the build type of the profile with the
/// first flavor of each dimension, capitalized for task names (`FreeRelease`).
fn default_variant(ctx: &BuildContext) -> Result<String> {
    let cfg = android_config(ctx)?;
    Ok(capitalize(&variant_name(&default_flavors(cfg, &[])?, build_type(&ctx.profile)?)))
}

/// Where AGP writes the generated code of the default variant of `ctx`, for editors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidGenerated {
    /// The variant, such as `freeDebug`.
    pub variant: String,
    /// The directory of the `BuildConfig` class, when the package has one.
    pub build_config: Option<PathBuf>,
    /// The jar with the `R` class of the package (and, without `android.nonTransitiveRClass`, its libraries').
    pub r_class_jar: PathBuf,
}

/// What AGP generates for the default variant of the Android package `ctx`, once Gradle built it.
pub fn generated(ctx: &BuildContext) -> Result<AndroidGenerated> {
    let cfg = android_config(ctx)?;
    let variant = variant_name(&default_flavors(cfg, &[])?, build_type(&ctx.profile)?);
    let build = ctx.ws.work_dir("android").join("build");
    let r_class_jar = build
        .join("intermediates/compile_and_runtime_not_namespaced_r_class_jar")
        .join(&variant)
        .join(format!("process{}Resources", capitalize(&variant)))
        .join("R.jar");
    let build_config = has_build_config(cfg).then(|| build.join("generated/source/buildConfig").join(&variant));
    Ok(AndroidGenerated { variant, build_config, r_class_jar })
}

/// Checks, before Gradle runs, what signing the build type of `ctx` needs: the keystore
/// exists and the environment variables with its passwords are set.
fn check_signing(ctx: &BuildContext) -> Result<()> {
    let cfg = android_config(ctx)?;
    let build_type = build_type(&ctx.profile)?;
    let Some(signing) = cfg.signing.get(build_type) else {
        return Ok(());
    };
    let section = format!("[target.android.signing.{build_type}]");
    let store = ctx.ws.root().join(&signing.store_file);
    if !store.is_file() {
        bail!("{section}: keystore {} not found; create it with `keytool -genkeypair` or fix `store-file`", store.display());
    }
    for var in [signing.store_password_env.as_str(), signing.key_password_env()] {
        if std::env::var_os(var).is_none_or(|v| v.is_empty()) {
            bail!("{section} reads a password from the environment variable `{var}`, which is not set; set it before building");
        }
    }
    Ok(())
}

/// Warns that the release build of an application is not signed.
fn warn_unsigned(ctx: &BuildContext) -> Result<()> {
    let cfg = android_config(ctx)?;
    if ctx.profile == ProfileKind::Release && cfg.kind == "application" && !cfg.signing.contains_key("release") {
        ctx.shell()
            .warn("the release build is unsigned and cannot be installed or uploaded; add [target.android.signing.release] to kargo.toml");
    }
    Ok(())
}

/// The Android Gradle Plugin version of `cfg` (`agp-version`, or Kargo's default).
pub fn agp_version(cfg: &AndroidConfig) -> &str {
    cfg.agp_version.as_deref().unwrap_or(AGP_VERSION)
}

/// Where the Gradle that runs an Android build comes from, in order of precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GradleChoice {
    /// A Gradle wrapper (`gradlew`) in the project root.
    Project(PathBuf),
    /// The Gradle on `PATH`, with `--system-gradle`.
    System(PathBuf),
    /// A Kargo-managed distribution; `pinned` when `gradle-version` names it.
    Managed { version: String, pinned: bool },
}

/// Picks the Gradle for `ws` without downloading anything. Fails when the
/// `agp-version` and `gradle-version` of the project are incompatible.
pub fn choose_gradle(ws: &Workspace, config: &Config) -> Result<GradleChoice> {
    if let Some(wrapper) = toolchain_gradle::project_wrapper(ws.root()) {
        return Ok(GradleChoice::Project(wrapper));
    }
    let android = ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?;
    let agp = agp_version(&android.config);
    let (version, pinned) = match &android.config.gradle_version {
        Some(version) => {
            toolchain_gradle::check_compatible(agp, version)?;
            (version.clone(), true)
        }
        None => (toolchain_gradle::gradle_for_agp(agp)?.version().to_owned(), false),
    };
    if config.system_gradle() {
        let system = process::find_on_path("gradle").with_context(|| {
            format!("`--system-gradle` needs `gradle` on PATH; install it (https://gradle.org/install) or drop the flag to let Kargo download Gradle {version}")
        })?;
        return Ok(GradleChoice::System(system));
    }
    Ok(GradleChoice::Managed { version, pinned })
}

fn managed_reason(pinned: bool, agp: &str) -> String {
    if pinned { "gradle-version in kargo.toml".to_owned() } else { format!("default for AGP {agp}") }
}

/// Describes the Gradle `kargo toolchain which` reports, without downloading it.
pub fn describe_gradle(ws: &Workspace, config: &Config) -> Result<String> {
    let android = ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?;
    let agp = agp_version(&android.config);
    Ok(match choose_gradle(ws, config)? {
        GradleChoice::Project(wrapper) => format!("project wrapper at {}", wrapper.display()),
        GradleChoice::System(gradle) => format!("system at {} (--system-gradle)", gradle.display()),
        GradleChoice::Managed { version, pinned } => {
            let why = managed_reason(pinned, agp);
            match GradleDistribution::find(config, &version)? {
                Some(dist) => format!("{version} ({why}) at {}", dist.home.display()),
                None => format!("{version} ({why}; not installed yet, installed on first android build)"),
            }
        }
    })
}

/// The Gradle launcher for `choice`, with the managed distribution it belongs to (if any),
/// downloading that distribution when needed.
fn launcher(ctx: &BuildContext, choice: GradleChoice) -> Result<(PathBuf, Option<GradleDistribution>)> {
    let shell = ctx.shell();
    match choice {
        GradleChoice::Project(wrapper) => {
            shell.verbose("Using", format!("the project's Gradle wrapper at {}", wrapper.display()));
            Ok((wrapper, None))
        }
        GradleChoice::System(gradle) => {
            shell.verbose("Using", format!("Gradle at {} (--system-gradle)", gradle.display()));
            Ok((gradle, None))
        }
        GradleChoice::Managed { version, pinned } => {
            let android = ctx.ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?;
            let agp = agp_version(&android.config);
            if !pinned && let AgpGradle::Newer(_) = toolchain_gradle::gradle_for_agp(agp)? {
                shell.warn(format!(
                    "Kargo does not know which Gradle Android Gradle Plugin {agp} needs and uses Gradle {version}; \
                     set `gradle-version` under [target.android] if the build fails"
                ));
            }
            let dist = match GradleDistribution::find(ctx.config, &version)? {
                Some(dist) => dist,
                None if ctx.config.offline() => {
                    if let Some(system) = process::find_on_path("gradle") {
                        shell.warn(format!(
                            "Gradle {version} is not installed and Kargo is offline; using the Gradle on PATH ({}) instead",
                            system.display()
                        ));
                        return Ok((system, None));
                    }
                    bail!(
                        "Gradle {version} is not installed and Kargo is offline; run `kargo fetch` (or `kargo toolchain install gradle`) while online"
                    );
                }
                None => GradleDistribution::ensure(ctx.config, &version)?,
            };
            shell.verbose("Using", format!("Gradle {version} ({}) at {}", managed_reason(pinned, agp), dist.home.display()));
            Ok((dist.executable(), Some(dist)))
        }
    }
}

/// The JDK that runs Gradle, checked against the AGP version.
fn gradle_jdk<'a>(ctx: &'a BuildContext) -> Result<&'a Jdk> {
    let android = ctx.ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?;
    let agp = agp_version(&android.config);
    let jdk = ctx.jdk()?;
    let min = toolchain_gradle::min_jdk_for_agp(agp);
    if jdk.version < min {
        bail!(
            "Android Gradle Plugin {agp} needs JDK {min} or newer, but this project uses JDK {} at {}; set `jdk = {min}` under [toolchain] in kargo.toml",
            jdk.version,
            jdk.home.display()
        );
    }
    Ok(jdk)
}

/// Copies the Gradle wrapper of `dist` into the generated project at `dir`.
fn write_wrapper(ctx: &BuildContext, dist: &GradleDistribution, dir: &Path, jdk: &Jdk) -> Result<()> {
    let template = dist.wrapper(ctx.config, &jdk.home)?;
    for file in toolchain_gradle::WRAPPER_FILES {
        let (from, to) = (template.join(file), dir.join(file));
        if std::fs::read(&from).ok() != std::fs::read(&to).ok() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to).with_context(|| format!("failed to write {}", to.display()))?;
        }
    }
    Ok(())
}

/// `kargo fetch` for Android: installs the Kargo-managed SDK packages, downloads the Gradle
/// the build uses and prepares its wrapper. Gradle itself fetches the dependencies during the build.
pub fn fetch(ctx: &BuildContext) -> Result<()> {
    if ctx.ws.manifest().toolchain.android_sdk != AndroidSdkRequest::System {
        sdk(ctx.ws, ctx.config, Licenses::Ask)?;
    }
    let choice = choose_gradle(ctx.ws, ctx.config)?;
    let jdk = gradle_jdk(ctx)?;
    if let (_, Some(dist)) = launcher(ctx, choice)? {
        dist.wrapper(ctx.config, &jdk.home)?;
    }
    ctx.shell().note("Android dependencies are fetched by Gradle during `kargo build --target android`");
    Ok(())
}

/// Runs Gradle tasks in the generated project.
pub fn gradle(ctx: &BuildContext, tasks: &[String]) -> Result<bool> {
    gradle_with_env(ctx, tasks, &[])
}

/// Runs Gradle tasks with more environment variables, such as `ANDROID_SERIAL`, which picks the device that
/// `install` and `connected*` tasks use.
pub fn gradle_with_env(ctx: &BuildContext, tasks: &[String], env: &[(&str, &str)]) -> Result<bool> {
    ctx.with_gradle(|| run_gradle(ctx, tasks, env))
}

/// Whether the Gradle task `name` runs on a device: `installDebug`, `uninstallDebug`, `connectedDebugAndroidTest`.
fn is_device_task(name: &str) -> bool {
    let task = name.rsplit(':').next().unwrap_or(name);
    task.starts_with("connected") || task.starts_with("install") || task.starts_with("uninstall") || task.ends_with("AndroidTest")
}

/// `kargo android <gradle args>`: runs the tasks. A task that uses a device gets the one chosen like `kargo run -t android`
/// chooses it (`device` is `--device` or `KARGO_ANDROID_DEVICE`), passed to Gradle as `ANDROID_SERIAL`.
pub fn run_tasks(ctx: &BuildContext, args: &[String], device: Option<&str>) -> Result<bool> {
    if !args.iter().any(|a| is_device_task(a)) {
        return gradle(ctx, args);
    }
    let opts = AndroidRun { device: device.map(str::to_owned), ..Default::default() };
    let selection = choose_device(ctx, &opts)?;
    gradle_with_env(ctx, args, &[("ANDROID_SERIAL", &selection.serial)])
}

fn run_gradle(ctx: &BuildContext, tasks: &[String], env: &[(&str, &str)]) -> Result<bool> {
    // Chosen first, so an incompatible `agp-version` and `gradle-version` fail before anything else.
    let choice = choose_gradle(ctx.ws, ctx.config)?;
    let dir = generate(ctx)?;
    let jdk = gradle_jdk(ctx)?;
    let (gradle, dist) = launcher(ctx, choice)?;
    if let Some(dist) = &dist
        && let Err(e) = write_wrapper(ctx, dist, &dir, jdk)
    {
        ctx.shell().warn(format!("could not add a Gradle wrapper to {}: {e:#}", dir.display()));
    }
    let mut cmd = Command::new(&gradle);
    cmd.arg("--project-dir").arg(&dir).arg("--console=plain");
    if !ctx.shell().is_verbose() {
        cmd.arg("--quiet");
    }
    if ctx.config.offline() {
        cmd.arg("--offline");
    }
    cmd.env("JAVA_HOME", &jdk.home).envs(env.iter().copied());
    if ctx.ws.manifest().toolchain.android_sdk != AndroidSdkRequest::System {
        // AGP also reads these, and fails when they name another SDK than `sdk.dir`.
        cmd.env("ANDROID_HOME", ctx.config.android_sdk_dir()).env_remove("ANDROID_SDK_ROOT");
    }
    cmd.args(tasks).current_dir(ctx.ws.root());
    if kargo_compiler::diagnostics::is_json() {
        // Stdout carries only JSON messages.
        cmd.stdout(std::io::stderr());
    }
    let status = process::status(ctx.shell(), &mut cmd, "Gradle")?;
    Ok(status.success())
}

/// The classpath of the Android main sources (`android.jar` first, then the
/// dependencies' classes), as resolved by Gradle. Used by `kargo doc`.
pub fn doc_classpath(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let list = ctx.ws.work_dir("android").join("build/kargo/doc-classpath.txt");
    if !gradle(ctx, &[DOC_CLASSPATH_TASK.to_owned()])? {
        bail!("Gradle `{DOC_CLASSPATH_TASK}` failed; check that `kargo build --target android` works");
    }
    let text = std::fs::read_to_string(&list).with_context(|| format!("failed to read {}", list.display()))?;
    Ok(text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect())
}

/// `kargo build --target android`: assembles the APK/AAR of every flavor. An Android library that
/// another package of this invocation uses is assembled by that package's Gradle run, together
/// with its own, so Gradle configures the build once.
pub fn build(ctx: &BuildContext) -> Result<PathBuf> {
    let cfg = android_config(ctx)?;
    let build_type = build_type(&ctx.profile)?;
    let out = ctx.ws.work_dir("android").join("build/outputs");
    if let Some(by) = built_by(ctx) {
        compiling(ctx, TargetKind::Android, &format!(", gradle, with {}", ctx.project.package(by).name()));
        return Ok(out.join("aar"));
    }
    compiling(ctx, TargetKind::Android, ", gradle");
    check_signing(ctx)?;
    let task = format!("assemble{}", capitalize(build_type));
    let selected = ctx.selected();
    let mut tasks = vec![task.clone()];
    for library in android_subprojects(ctx, ctx.package).into_iter().filter(|i| selected.contains(i)) {
        tasks.push(format!(":{}:{task}", ctx.project.package(library).name()));
    }
    if !gradle(ctx, &tasks)? {
        bail!("Gradle `{}` failed", tasks.join("`, `"));
    }
    warn_unsigned(ctx)?;
    Ok(match (cfg.kind == "application", cfg.flavors.is_empty()) {
        (true, true) => out.join("apk").join(build_type),
        // Gradle writes apk/<flavor>/<build type>/ per flavor.
        (true, false) => out.join("apk"),
        (false, _) => out.join("aar"),
    })
}

/// The AAR of the release build of an Android library, for `kargo publish`.
pub fn release_aar(ctx: &BuildContext) -> Result<PathBuf> {
    let cfg = android_config(ctx)?;
    if cfg.kind != "library" {
        bail!("only an Android library (`kind = \"library\"` under [target.android]) builds an AAR");
    }
    if !cfg.flavors.is_empty() {
        bail!("`kargo publish` cannot publish an Android library with product flavors yet; it publishes one release AAR");
    }
    compiling(ctx, TargetKind::Android, ", gradle");
    // AGP names the AAR after the Gradle project, which is the package.
    let aar = ctx.ws.work_dir("android").join("build/outputs/aar").join(format!("{}-release.aar", ctx.ws.name()));
    let _ = std::fs::remove_file(&aar);
    if !gradle(ctx, &["assembleRelease".to_owned()])? {
        bail!("Gradle `assembleRelease` failed");
    }
    if !aar.is_file() {
        bail!("Gradle `assembleRelease` wrote no {}", aar.display());
    }
    Ok(aar)
}

/// `kargo package --target android`: builds the APKs and Android App Bundles (AABs) of
/// an application, or the AARs of a library, for every flavor, and copies them to
/// `target/<profile>/android`. Returns the copied files, App Bundles first.
pub fn package(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let cfg = android_config(ctx)?;
    let is_app = cfg.kind == "application";
    compiling(ctx, TargetKind::Android, ", gradle");
    check_signing(ctx)?;
    let build_type = build_type(&ctx.profile)?;
    let mut tasks = Vec::new();
    for flavors in flavor_combinations(cfg) {
        let variant = capitalize(&variant_name(&flavors, build_type));
        if is_app {
            tasks.push(format!("bundle{variant}"));
        }
        tasks.push(format!("assemble{variant}"));
    }
    let outputs = ctx.ws.work_dir("android").join("build/outputs");
    for kind in ["bundle", "apk", "aar"] {
        let dir = outputs.join(kind);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("failed to clear {}", dir.display()))?;
        }
    }
    if !gradle(ctx, &tasks)? {
        bail!("Gradle `{}` failed", tasks.join("`, `"));
    }
    warn_unsigned(ctx)?;
    let mut found = Vec::new();
    for (kind, ext) in [("bundle", "aab"), ("apk", "apk"), ("aar", "aar")] {
        collect_files(&outputs.join(kind), ext, &mut found)?;
    }
    if found.is_empty() {
        bail!("Gradle `{}` produced no APK, AAB or AAR under {}", tasks.join("`, `"), outputs.display());
    }
    let dest = ctx.ws.output_dir(&ctx.profile, TargetKind::Android);
    std::fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let mut copied = Vec::new();
    for file in found {
        let to = dest.join(file.file_name().context("an output without a file name")?);
        std::fs::copy(&file, &to).with_context(|| format!("failed to copy {} to {}", file.display(), to.display()))?;
        ctx.shell().status("Packaged", to.display());
        copied.push(to);
    }
    Ok(copied)
}

/// The files with extension `ext` under `dir`, sorted, appended to `out`.
fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_files(&path, ext, out)?;
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    Ok(())
}

/// `kargo check --target android`: compiles the Kotlin and Java sources (and,
/// with `tests`, the unit tests) without dexing or packaging an APK/AAR.
pub fn check(ctx: &BuildContext, tests: bool) -> Result<()> {
    announce(ctx, "Checking", TargetKind::Android, if tests { ", gradle, tests" } else { ", gradle" });
    let variant = default_variant(ctx)?;
    let mut tasks = vec![format!("compile{variant}Sources")];
    if tests {
        tasks.push(format!("compile{variant}UnitTestSources"));
    }
    if !gradle(ctx, &tasks)? {
        bail!("Gradle `{}` failed", tasks.join("`, `"));
    }
    Ok(())
}

/// `kargo lint --android`: runs Android lint on the default variant (the build type of the profile with the first flavor
/// of each dimension) and copies its reports to `target/<profile>/android/lint/`. With `--message-format json` the
/// issues become `compiler-message`s. Returns `false` when lint reported errors.
///
/// With `create_baseline`, records the issues in the baseline file instead of reporting them.
pub fn lint(ctx: &BuildContext, create_baseline: bool) -> Result<bool> {
    let cfg = android_config(ctx)?;
    let root = ctx.ws.root();
    let baseline = cfg.lint.baseline.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_LINT_BASELINE));
    if !create_baseline && cfg.lint.baseline.is_some() && !root.join(&baseline).is_file() {
        bail!(
            "[target.android.lint] baseline file `{}` does not exist; create it with `kargo lint --android --create-baseline`",
            baseline.display()
        );
    }
    announce(ctx, "Linting", TargetKind::Android, ", gradle, lint");
    let variant = variant_name(&default_flavors(cfg, &[])?, build_type(&ctx.profile)?);
    let reports = ctx.ws.work_dir("android").join("build/reports");
    let stem = format!("lint-results-{variant}");
    for old in lint_reports(&reports, &stem) {
        let _ = std::fs::remove_file(old);
    }
    let mut args = Vec::new();
    if create_baseline {
        args.push(format!("updateLintBaseline{}", capitalize(&variant)));
        args.push(format!("-P{CREATE_BASELINE_PROPERTY}"));
    } else {
        args.push(format!("lint{}", capitalize(&variant)));
    }
    let ok = gradle(ctx, &args)?;
    if create_baseline {
        if !ok {
            bail!("Gradle `{}` failed", args[0]);
        }
        ctx.shell().status("Created", format!("baseline {}", baseline.display()));
        return Ok(true);
    }

    let found = lint_reports(&reports, &stem);
    if found.is_empty() {
        bail!("Gradle `{}` failed before lint wrote a report; its output is above", args[0]);
    }
    let dest = ctx.ws.output_dir(&ctx.profile, TargetKind::Android).join("lint");
    std::fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
    for file in &found {
        let to = dest.join(file.file_name().context("a report without a file name")?);
        std::fs::copy(file, &to).with_context(|| format!("failed to copy {} to {}", file.display(), to.display()))?;
        ctx.shell().status("Report", to.display());
        if kargo_compiler::diagnostics::is_json()
            && file.extension().is_some_and(|e| e == "sarif")
            && let Ok(text) = std::fs::read_to_string(file)
        {
            for diagnostic in parse_sarif(&text) {
                kargo_compiler::diagnostics::emit(&diagnostic.to_json());
            }
        }
    }
    Ok(ok)
}

/// The reports Android lint wrote for the variant whose files start with `stem`, sorted.
fn lint_reports(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.file_stem().is_some_and(|s| s.to_string_lossy() == stem)
                && p.extension().is_some_and(|e| matches!(e.to_str(), Some("html" | "xml" | "sarif" | "txt")))
        })
        .collect();
    found.sort();
    found
}

/// The issues of a SARIF report from Android lint, as compiler diagnostics (`note` results are `info`).
fn parse_sarif(text: &str) -> Vec<Diagnostic> {
    let Ok(sarif) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for result in sarif["runs"].as_array().into_iter().flatten().flat_map(|run| run["results"].as_array().into_iter().flatten()) {
        let severity = match result["level"].as_str() {
            Some("error") => "error",
            Some("warning") => "warning",
            _ => "info",
        };
        let mut message = result["message"]["text"].as_str().unwrap_or_default().to_owned();
        if let Some(rule) = result["ruleId"].as_str() {
            message = format!("{message} [{rule}]");
        }
        let location = &result["locations"][0]["physicalLocation"];
        let file = location["artifactLocation"]["uri"].as_str().map(sarif_path);
        let line = location["region"]["startLine"].as_u64().map(|l| l as u32);
        let column = location["region"]["startColumn"].as_u64().map(|c| c as u32);
        let rendered = match (&file, line, column) {
            (Some(f), Some(l), Some(c)) => format!("{f}:{l}:{c}: {severity}: {message}"),
            (Some(f), Some(l), None) => format!("{f}:{l}: {severity}: {message}"),
            (Some(f), None, _) => format!("{f}: {severity}: {message}"),
            (None, ..) => format!("{severity}: {message}"),
        };
        out.push(Diagnostic { severity: severity.to_owned(), message, file, line, column, rendered });
    }
    out
}

/// The file path of a SARIF location: `file:///C:/src/A.kt` is `C:/src/A.kt`, `file:///src/A.kt` is `/src/A.kt`.
fn sarif_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    let path = String::from_utf8_lossy(&decoded).into_owned();
    // `/C:/src` is a Windows path with a drive letter.
    match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => path[1..].to_owned(),
        _ => path,
    }
}

/// `kargo test --target android`: runs local unit tests and reports the
/// results Gradle writes as JUnit XML, copying those files to `report_dir` if given.
pub fn test(ctx: &BuildContext, filter: Option<&str>, no_run: bool, report_dir: Option<&Path>) -> Result<bool> {
    compiling(ctx, TargetKind::Android, ", gradle tests");
    let variant = default_variant(ctx)?;
    let task = format!("test{variant}UnitTest");
    if no_run {
        return gradle(ctx, &[format!("compile{variant}UnitTestSources")]);
    }
    let results = ctx.ws.work_dir("android").join("build/test-results").join(&task);
    if results.exists() {
        std::fs::remove_dir_all(&results).with_context(|| format!("failed to clear {}", results.display()))?;
    }
    let mut args = vec![task.clone()];
    if let Some(filter) = filter {
        // Gradle matches `package.Class.method`; accept `Class#method` too.
        args.extend(["--tests".to_owned(), format!("*{}*", filter.replace('#', "."))]);
    }
    ctx.shell().status("Running", "Android unit tests (Gradle)");
    let gradle_ok = gradle(ctx, &args)?;

    let mut summary = TestSummary::default();
    let mut reports: Vec<PathBuf> = match std::fs::read_dir(&results) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "xml")).collect(),
        Err(_) => Vec::new(),
    };
    reports.sort();
    for report in &reports {
        let xml = std::fs::read_to_string(report).with_context(|| format!("failed to read {}", report.display()))?;
        if let (Some(dir), Some(name)) = (report_dir, report.file_name()) {
            std::fs::write(dir.join(name), &xml).with_context(|| format!("failed to copy {} to {}", report.display(), dir.display()))?;
        }
        let json = kargo_compiler::diagnostics::is_json();
        let package = &ctx.ws.manifest().package.name;
        let part = testreport::report_junit_xml_with(&xml, human_out(), |event| {
            if json {
                emit_report_event(package, TargetKind::Android, event);
            }
        })
        .with_context(|| format!("failed to parse {}", report.display()))?;
        summary.merge(part);
    }
    if summary == TestSummary::default() && (filter.is_none() || !gradle_ok) {
        if gradle_ok {
            // Guards against Gradle silently skipping the sources (`NO-SOURCE`).
            ctx.shell().error(format!("Gradle `{task}` ran no tests although the project has test sources"));
        } else {
            ctx.shell().error(format!("Gradle `{task}` failed"));
        }
        return Ok(false);
    }
    emit_summary(ctx, TargetKind::Android, summary.passed, summary.failed, summary.ignored, gradle_ok && summary.success());
    testreport::print_summary(&summary, None, human_out())?;
    if !gradle_ok && summary.success() {
        ctx.shell().error(format!("Gradle `{task}` failed"));
    }
    Ok(gradle_ok && summary.success())
}

/// An app installed and started on a device.
pub struct Deployed {
    pub adb: Adb,
    pub serial: String,
    pub app_id: String,
    /// The emulator was started for this run.
    pub started_emulator: bool,
    /// The process of the app, when it was found.
    pub pid: Option<u32>,
}

/// Picks the device `kargo run -t android` uses (see [`android_device::select`]). It comes before the build: it may
/// ask a question, and there is no use compiling for a device that does not exist. Kargo's SDK is installed first, so
/// that `adb` is there.
pub fn choose_device(ctx: &BuildContext, opts: &AndroidRun) -> Result<Selection> {
    sdk(ctx.ws, ctx.config, Licenses::Ask)?;
    let toolbox = Toolbox::locate(ctx.config, Some(ctx.ws.manifest()));
    android_device::select(&toolbox, ctx.shell(), opts.device.as_deref(), &opts.emulator)
}

/// Installs the app on the chosen device with Gradle (`install<Variant>` with `ANDROID_SERIAL`, so no other device gets
/// it) and starts it. `flavors` (`--flavor`) picks the flavor of a dimension; the others use their first flavor.
pub fn deploy(ctx: &BuildContext, flavors: &[String], selection: Selection) -> Result<Deployed> {
    let cfg = android_config(ctx)?;
    let flavors = validate_run(ctx, flavors)?;
    let build_type = build_type(&ctx.profile)?;
    if !flavors.is_empty() {
        ctx.shell().verbose("Using", format!("flavor {}", flavors.join(", ")));
    }
    let adb = Toolbox::locate(ctx.config, Some(ctx.ws.manifest())).adb()?;
    let task = format!("install{}", capitalize(&variant_name(&flavors, build_type)));
    if !gradle_with_env(ctx, std::slice::from_ref(&task), &[("ANDROID_SERIAL", &selection.serial)])? {
        bail!("installing the app on {} ({}) failed; the Gradle output above says why", selection.name, selection.serial);
    }
    let app_id = application_id(cfg, &flavors);
    ctx.shell().status("Launching", format!("{app_id} on {}", selection.name));
    let activity = android_device::launch(&adb, &selection.serial, &app_id)?;
    ctx.shell().verbose("Started", activity);
    let pid = android_device::wait_for_pid(&adb, &selection.serial, &app_id);
    Ok(Deployed { adb, serial: selection.serial, app_id, started_emulator: selection.started, pid })
}

/// `kargo run --target android`: installs on the chosen device, launches the app and follows its log until it exits
/// or the user presses Ctrl+C, which stops it.
pub fn run(ctx: &BuildContext, flavors: &[String], opts: &AndroidRun, selection: Selection) -> Result<i32> {
    let deployed = deploy(ctx, flavors, selection)?;
    let shell = ctx.shell();
    let code = match deployed.pid {
        _ if opts.no_logcat => 0,
        Some(pid) => android_device::follow(&deployed.adb, shell, &deployed.serial, &deployed.app_id, pid)?.code(),
        None => {
            shell.warn(format!(
                "could not find the process of {}; it may have exited right after starting, so there is no log to show",
                deployed.app_id
            ));
            1
        }
    };
    if opts.stop_emulator && deployed.started_emulator {
        android_device::stop_emulator(&deployed.adb, shell, &deployed.serial)?;
    }
    Ok(code)
}

/// Checks what `kargo run -t android` needs before anything builds: an application, known
/// `--flavor`s and, for a release, a signing configuration. Returns the flavors to install.
pub fn validate_run(ctx: &BuildContext, flavors: &[String]) -> Result<Vec<String>> {
    let cfg = android_config(ctx)?;
    if cfg.kind != "application" {
        bail!("an Android library cannot be run");
    }
    let flavors = default_flavors(cfg, flavors)?;
    if ctx.profile == ProfileKind::Release && !cfg.signing.contains_key("release") {
        bail!("the release build is unsigned and cannot be installed; add [target.android.signing.release] to kargo.toml");
    }
    check_signing(ctx)?;
    Ok(flavors)
}

/// The application id of the variant with `flavors`: `application-id` plus their suffixes.
fn application_id(cfg: &AndroidConfig, flavors: &[String]) -> String {
    let mut id = cfg.application_id.clone().unwrap_or_else(|| cfg.namespace.clone());
    for flavor in flavors {
        if let Some(suffix) = &cfg.flavors[flavor].application_id_suffix {
            if !suffix.starts_with('.') {
                id.push('.');
            }
            id.push_str(suffix);
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(extra: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("kargo.toml"),
            format!(
                "[package]\nname = \"droid\"\nversion = \"1.2.0\"\n[target.android]\nnamespace = \"com.example.droid\"\nmin-sdk = 26\n{extra}"
            ),
        )
        .unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        (dir, ws)
    }

    #[test]
    fn compose_resources_on_android() {
        let (dir, ws) = workspace("[frameworks]\ncompose = true\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        let without = build_gradle(&ws, &android).unwrap();
        assert!(!without.contains("components-resources") && !without.contains("compose-resources"));

        std::fs::create_dir_all(dir.path().join("composeResources/values")).unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        let generated = compose_resources_dir(&ws);
        assert!(
            text.contains(&format!(
                "implementation(\"org.jetbrains.compose.components:components-resources:{}\")",
                builtins::COMPOSE_VERSION
            ))
        );
        assert!(text.contains(&format!("kotlin.directories.add({})", kts_path(&generated.join("kotlin")))), "{text}");
        assert!(
            text.contains(&format!(
                "assets.setSrcDirs(listOf({}, {}))",
                kts_path(&dir.path().join("android/assets")),
                kts_path(&generated.join("resources"))
            )),
            "{text}"
        );
    }

    #[test]
    fn compose_compiler_options_on_android() {
        let (_dir, ws) = workspace("[frameworks]\ncompose = { strong-skipping = false, reports = true }\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        let reports = kargo_util::process::slash(&ws.work_dir("compose").join("android/reports"));
        let id = builtins::COMPOSE_PLUGIN_ID;
        assert!(
            text.contains(&format!(
                "freeCompilerArgs.addAll(\"-P\", \"plugin:{id}:featureFlag=-StrongSkipping\", \"-P\", {})",
                kts_string(&format!("plugin:{id}:reportsDestination={reports}"))
            )),
            "{text}"
        );
    }

    #[test]
    fn renders_android_application() {
        let (_d, ws) = workspace(
            "[dependencies]\n\"com.squareup.okhttp3:okhttp\" = { version = \"4.12.0\", exclude = [\"org.jetbrains.kotlin:*\"] }\n\"x:bom\" = { version = \"1\", bom = true }\n\"x:managed\" = {}\n[dev-dependencies]\n\"io.mockk:mockk\" = \"1.13.0\"\n[frameworks]\ncompose = true\nserialization = true\ncoroutines = true\n[kotlin]\nopt-in = [\"kotlin.RequiresOptIn\"]\n",
        );
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("id(\"com.android.application\") version \"9.4.1\""), "{text}");
        assert!(text.contains("id(\"org.jetbrains.kotlin.plugin.compose\")"));
        assert!(text.contains("id(\"org.jetbrains.kotlin.plugin.serialization\")"));
        assert!(text.contains("applicationId = \"com.example.droid\""));
        assert!(text.contains("versionName = \"1.2.0\""));
        assert!(text.contains(&format!("jniLibs.setSrcDirs(listOf({}))", kts_path(&ws.root().join("android/jniLibs")))), "{text}");
        assert!(text.contains("minSdk = 26"));
        assert!(text.contains("compose = true"));
        assert!(
            text.contains("implementation(\"com.squareup.okhttp3:okhttp:4.12.0\") { exclude(group = \"org.jetbrains.kotlin\") }"),
            "{text}"
        );
        assert!(text.contains("implementation(platform(\"x:bom:1\"))"));
        assert!(text.contains("implementation(\"x:managed\")"));
        assert!(text.contains("implementation(platform(\"androidx.compose:compose-bom:"));
        assert!(text.contains("kotlinx-coroutines-android"));
        assert!(text.contains("kotlinx-serialization-json"));
        assert!(text.contains("testImplementation(\"io.mockk:mockk:1.13.0\")"));
        assert!(text.contains("testImplementation(\"junit:junit:4.13.2\")"));
        assert!(text.contains("optIn.addAll(\"kotlin.RequiresOptIn\")"));
        assert!(!text.contains("kotlin-stdlib:"), "AGP adds the stdlib itself");
        assert!(text.contains("tasks.register(\"kargoDocClasspath\")"), "the task `kargo doc` runs");
        let kotlin = &ws.manifest().kotlin.version;
        assert!(text.contains(&format!("id(\"org.jetbrains.kotlin.android\") version \"{kotlin}\" apply false")), "{text}");
    }

    #[test]
    fn ksp_dependencies_apply_the_ksp_plugin() {
        let (_d, ws) = workspace("");
        let android = ws.manifest().targets.android.clone().unwrap();
        assert!(!build_gradle(&ws, &android).unwrap().contains("com.google.devtools.ksp"));

        let (_d, ws) = workspace(
            "[dependencies]\n\"androidx.room:room-compiler\" = { version = \"2.8.0\", scope = \"ksp\" }\n[dev-dependencies]\n\"x:test-processor\" = { version = \"1\", scope = \"ksp\" }\n",
        );
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains(&format!("id(\"com.google.devtools.ksp\") version \"{}\"", builtins::KSP_VERSION)), "{text}");
        assert!(text.contains("    ksp(\"androidx.room:room-compiler:2.8.0\")"), "{text}");
        assert!(text.contains("    kspTest(\"x:test-processor:1\")"), "{text}");

        let (_d, ws) = workspace("[dependencies]\n\"x:p\" = { version = \"1\", scope = \"ksp\" }\n[kotlin]\nksp = \"2.3.10\"\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        assert!(build_gradle(&ws, &android).unwrap().contains("id(\"com.google.devtools.ksp\") version \"2.3.10\""));
    }

    #[test]
    fn renders_release_signing_and_r8() {
        let (_d, ws) = workspace(
            "minify = true\nproguard-files = [\"android/proguard-rules.pro\"]\nbuild-config = { API_URL = \"https://x\", RETRIES = 3 }\n\
             [target.android.signing.release]\nstore-file = \"release.jks\"\nstore-password-env = \"KS_PASS\"\nkey-alias = \"upload\"\nkey-password-env = \"KEY_PASS\"\n",
        );
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        let signing = text.split("signingConfigs {").nth(1).unwrap().split("buildTypes {").next().unwrap();
        assert!(signing.contains("create(\"release\") {"), "{text}");
        assert!(signing.contains(&format!("storeFile = file({})", kts_path(&ws.root().join("release.jks")))), "{text}");
        assert!(signing.contains("storePassword = providers.environmentVariable(\"KS_PASS\").orNull"), "{text}");
        assert!(signing.contains("keyAlias = \"upload\""));
        assert!(signing.contains("keyPassword = providers.environmentVariable(\"KEY_PASS\").orNull"), "{text}");
        let release = text.split("getByName(\"release\") {").nth(1).unwrap().split('}').next().unwrap();
        assert!(release.contains("isMinifyEnabled = true"), "{text}");
        assert!(release.contains("isShrinkResources = true"), "on by default with minify for apps");
        let rules = kts_path(&ws.root().join("android/proguard-rules.pro"));
        assert!(
            release.contains(&format!("proguardFiles(getDefaultProguardFile(\"proguard-android-optimize.txt\"), file({rules}))")),
            "{text}"
        );
        assert!(release.contains("signingConfig = signingConfigs.getByName(\"release\")"));
        assert!(!text.contains("getByName(\"debug\") {"), "the debug build type keeps AGP's defaults");
        assert!(text.contains("buildConfigField(\"String\", \"API_URL\", \"\\\"https://x\\\"\")"), "{text}");
        assert!(text.contains("buildConfigField(\"int\", \"RETRIES\", \"3\")"), "{text}");
        assert!(text.contains("buildConfig = true"));

        // Without any of it, none of the blocks.
        let (_d, ws) = workspace("");
        let text = build_gradle(&ws, &ws.manifest().targets.android.clone().unwrap()).unwrap();
        for absent in ["signingConfigs", "buildTypes", "productFlavors", "buildConfig", "buildFeatures"] {
            assert!(!text.contains(absent), "{absent} in\n{text}");
        }

        // A debug keystore reuses AGP's `debug` signing config.
        let (_d, ws) = workspace(
            "shrink-resources = false\nminify = true\n[target.android.signing.debug]\nstore-file = \"debug.jks\"\nstore-password-env = \"P\"\nkey-alias = \"d\"\nstore-type = \"pkcs12\"\n",
        );
        let text = build_gradle(&ws, &ws.manifest().targets.android.clone().unwrap()).unwrap();
        assert!(text.contains("getByName(\"debug\") {\n            storeFile"), "{text}");
        assert!(text.contains("keyPassword = providers.environmentVariable(\"P\").orNull"), "defaults to the store password");
        assert!(text.contains("storeType = \"pkcs12\""));
        assert!(text.contains("getByName(\"debug\") {\n            signingConfig = signingConfigs.getByName(\"debug\")"), "{text}");
        assert!(!text.contains("isShrinkResources"), "{text}");
    }

    #[test]
    fn renders_library_consumer_rules() {
        let (_d, ws) = workspace("kind = \"library\"\nconsumer-proguard-files = [\"consumer-rules.pro\"]\n");
        let text = build_gradle(&ws, &ws.manifest().targets.android.clone().unwrap()).unwrap();
        assert!(text.contains(&format!("consumerProguardFiles({})", kts_path(&ws.root().join("consumer-rules.pro")))), "{text}");
    }

    #[test]
    fn renders_flavors_and_picks_variants() {
        let (_d, ws) = workspace(
            "flavor-dimensions = [\"tier\", \"env\"]\n\
             [target.android.flavors.paid]\ndimension = \"tier\"\n\
             [target.android.flavors.free]\ndimension = \"tier\"\napplication-id-suffix = \".free\"\nversion-name-suffix = \"-free\"\nbuild-config = { ADS = true }\n\
             [target.android.flavors.staging]\ndimension = \"env\"\napplication-id-suffix = \"staging\"\nmin-sdk = 28\nversion-code = 7\n\
             [target.android.flavors.prod]\ndimension = \"env\"\n",
        );
        let cfg = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &cfg).unwrap();
        assert!(text.contains("flavorDimensions += listOf(\"tier\", \"env\")"), "{text}");
        let free = text.split("create(\"free\") {").nth(1).unwrap().split('}').next().unwrap();
        assert!(free.contains("dimension = \"tier\""));
        assert!(free.contains("applicationIdSuffix = \".free\""));
        assert!(free.contains("versionNameSuffix = \"-free\""));
        assert!(free.contains("buildConfigField(\"boolean\", \"ADS\", \"true\")"), "{text}");
        let staging = text.split("create(\"staging\") {").nth(1).unwrap().split('}').next().unwrap();
        assert!(staging.contains("minSdk = 28") && staging.contains("versionCode = 7"), "{text}");
        assert!(text.contains("buildConfig = true"));
        // `kargo doc` reads the classpath of the default debug variant.
        assert!(text.contains("configurations.getByName(\"freeProdDebugCompileClasspath\")"), "{text}");

        let cfg = &cfg.config;
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_flavors(cfg, &[]).unwrap(), s(&["free", "prod"]));
        assert_eq!(default_flavors(cfg, &s(&["staging"])).unwrap(), s(&["free", "staging"]));
        assert_eq!(default_flavors(cfg, &s(&["staging", "paid"])).unwrap(), s(&["paid", "staging"]));
        let err = default_flavors(cfg, &s(&["free", "paid"])).unwrap_err().to_string();
        assert!(err.contains("pick one flavor of dimension `tier`"), "{err}");
        let err = default_flavors(cfg, &s(&["gold"])).unwrap_err().to_string();
        assert!(err.contains("unknown flavor `gold` (expected one of: free, paid, prod, staging)"), "{err}");
        assert_eq!(variant_name(&s(&["free", "staging"]), "release"), "freeStagingRelease");
        assert_eq!(variant_name(&[], "debug"), "debug");
        assert_eq!(
            flavor_combinations(cfg),
            [s(&["free", "prod"]), s(&["free", "staging"]), s(&["paid", "prod"]), s(&["paid", "staging"])]
        );
        assert_eq!(application_id(cfg, &s(&["free", "staging"])), "com.example.droid.free.staging");
        assert_eq!(application_id(cfg, &s(&["paid", "prod"])), "com.example.droid");

        let (_d, ws) = workspace("");
        let cfg = &ws.manifest().targets.android.as_ref().unwrap().config;
        assert_eq!(flavor_combinations(cfg), [Vec::<String>::new()]);
        assert!(default_flavors(cfg, &s(&["free"])).unwrap_err().to_string().contains("defines no flavors"));
    }

    #[test]
    fn renders_placeholders_ndk_and_locales() {
        let (dir, ws) = workspace(
            "build-tools = \"36.0.0\"\nlocales = [\"en\", \"es\"]\nmanifest-placeholders = { MAPS_KEY = \"abc\", DEBUGGABLE = true }\n\
             [target.android.ndk]\nversion = \"27.2.12479018\"\nabi-filters = [\"arm64-v8a\", \"x86_64\"]\n\
             cmake = \"cpp/CMakeLists.txt\"\ncmake-version = \"3.22.1\"\ncmake-arguments = [\"-DANDROID_STL=c++_shared\"]\ncpp-flags = [\"-std=c++17\"]\n\
             [target.android.flavors.free]\ndimension = \"tier\"\nmanifest-placeholders = { MAPS_KEY = \"free\" }\n",
        );
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("    buildToolsVersion = \"36.0.0\""), "{text}");
        assert!(text.contains("    ndkVersion = \"27.2.12479018\""), "{text}");
        assert!(text.contains("manifestPlaceholders[\"MAPS_KEY\"] = \"abc\""), "{text}");
        assert!(text.contains("manifestPlaceholders[\"DEBUGGABLE\"] = \"true\""), "{text}");
        let free = text.split("create(\"free\") {").nth(1).unwrap().split('}').next().unwrap();
        assert!(free.contains("manifestPlaceholders[\"MAPS_KEY\"] = \"free\""), "{text}");
        assert!(text.contains("abiFilters.addAll(listOf(\"arm64-v8a\", \"x86_64\"))"), "{text}");
        assert!(text.contains("arguments(\"-DANDROID_STL=c++_shared\")") && text.contains("cppFlags(\"-std=c++17\")"), "{text}");
        let cmake = kts_path(&dir.path().join("cpp/CMakeLists.txt"));
        assert!(text.contains(&format!("path = file({cmake})")) && text.contains("version = \"3.22.1\""), "{text}");
        assert!(text.contains("androidResources {\n        localeFilters.addAll(listOf(\"en\", \"es\"))"), "{text}");

        let (_d, ws) = workspace("[target.android.ndk]\nndk-build = \"jni/Android.mk\"\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("ndkBuild {"), "{text}");
        assert!(!text.contains("cmake {") && !text.contains("abiFilters") && !text.contains("ndkVersion"), "{text}");

        let (_d, ws) = workspace("");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        for absent in ["manifestPlaceholders", "ndk", "externalNativeBuild", "androidResources", "buildToolsVersion"] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
    }

    #[test]
    fn renders_the_lint_block() {
        let (dir, ws) = workspace("");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        let baseline = kts_path(&dir.path().join("lint-baseline.xml"));
        assert!(text.contains(&format!("val baselineFile = file({baseline})")), "{text}");
        // A baseline that does not exist is only used to create it: AGP fails a run that has to.
        assert!(text.contains("baselineFile.exists() || providers.gradleProperty(\"kargo.lint.createBaseline\").isPresent"), "{text}");
        assert!(!text.contains("lintConfig") && !text.contains("abortOnError") && !text.contains("warningsAsErrors"), "{text}");
        for (report, on) in [("html", true), ("xml", true), ("sarif", true), ("text", false)] {
            assert!(text.contains(&format!("{report}Report = {on}")), "{text}");
        }

        let (dir, ws) = workspace(
            "[target.android.lint]\nbaseline = \"config/baseline.xml\"\nabort-on-error = false\nwarnings-as-errors = true\n\
             check-dependencies = true\ndisable = [\"MissingTranslation\", \"UnusedResources\"]\nerror = [\"HardcodedText\"]\n\
             ignore = [\"IconDensities\"]\nreports = [\"xml\", \"text\"]\n",
        );
        std::fs::write(dir.path().join("lint.xml"), "<lint/>").unwrap();
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains(&format!("file({})", kts_path(&dir.path().join("config/baseline.xml")))), "{text}");
        assert!(text.contains(&format!("lintConfig = file({})", kts_path(&dir.path().join("lint.xml")))), "{text}");
        assert!(
            text.contains("abortOnError = false") && text.contains("warningsAsErrors = true") && text.contains("checkDependencies = true")
        );
        assert!(text.contains("disable.addAll(listOf(\"MissingTranslation\", \"UnusedResources\"))"), "{text}");
        assert!(
            text.contains("error.addAll(listOf(\"HardcodedText\"))") && text.contains("ignore.addAll(listOf(\"IconDensities\"))"),
            "{text}"
        );
        for (report, on) in [("html", false), ("xml", true), ("sarif", true), ("text", true)] {
            assert!(text.contains(&format!("{report}Report = {on}")), "{text}");
        }
    }

    #[test]
    fn pins_the_sdk_packages_of_the_ndk() {
        let (_d, ws) = workspace("");
        let cfg = &ws.manifest().targets.android.as_ref().unwrap().config;
        let plain = sdk_packages(cfg);
        assert_eq!(plain.len(), 3);
        let (_d, ws) = workspace(
            "build-tools = \"35.0.0\"\n[target.android.ndk]\nversion = \"27.2.12479018\"\ncmake = \"CMakeLists.txt\"\ncmake-version = \"3.22.1\"\n",
        );
        let cfg = &ws.manifest().targets.android.as_ref().unwrap().config;
        let packages = sdk_packages(cfg);
        assert_eq!(packages[1], "build-tools;35.0.0");
        assert_eq!(&packages[3..], ["ndk;27.2.12479018", "cmake;3.22.1"]);
    }

    #[test]
    fn reads_lint_issues_from_sarif() {
        let sarif = r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"Android Lint"}},"results":[
            {"ruleId":"HardcodedText","level":"warning","message":{"text":"Hardcoded string"},
             "locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///C:/my%20app/res/layout/main.xml"},
                           "region":{"startLine":12,"startColumn":5}}}]},
            {"ruleId":"MissingPermission","level":"error","message":{"text":"Missing permission"},
             "locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///home/me/app/src/A.kt"},"region":{"startLine":3}}}]},
            {"ruleId":"OldTargetApi","level":"note","message":{"text":"Target is old"}}]}]}"#;
        let issues = parse_sarif(sarif);
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].severity, "warning");
        assert_eq!(issues[0].message, "Hardcoded string [HardcodedText]");
        assert_eq!(issues[0].file.as_deref(), Some("C:/my app/res/layout/main.xml"));
        assert_eq!((issues[0].line, issues[0].column), (Some(12), Some(5)));
        assert_eq!(issues[0].rendered, "C:/my app/res/layout/main.xml:12:5: warning: Hardcoded string [HardcodedText]");
        assert_eq!(
            (issues[1].severity.as_str(), issues[1].file.as_deref(), issues[1].column),
            ("error", Some("/home/me/app/src/A.kt"), None)
        );
        assert_eq!((issues[2].severity.as_str(), issues[2].file.as_deref()), ("info", None));
        assert_eq!(issues[2].rendered, "info: Target is old [OldTargetApi]");
        assert!(parse_sarif("not json").is_empty());
    }

    #[test]
    fn registers_kotlin_source_dirs() {
        // AGP's built-in Kotlin ignores Kotlin files in the Java source dirs.
        let (_d, ws) = workspace("");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        let main = text.split("getByName(\"main\")").nth(1).unwrap().split("getByName(\"test\")").next().unwrap();
        let test = text.split("getByName(\"test\")").nth(1).unwrap();
        for (block, names) in [(main, ["src", "src-android"]), (test, ["tests", "tests-android"])] {
            for name in names {
                let line = format!("kotlin.directories.add({})", kts_path(&ws.root().join(name)));
                assert!(block.contains(&line), "missing {line} in\n{text}");
            }
        }
    }

    #[test]
    fn expect_actual_files_are_fragments_of_one_compilation() {
        let (dir, ws) = workspace("");
        let android = ws.manifest().targets.android.clone().unwrap();
        let write = |path: &str| {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, "").unwrap();
            kargo_util::process::slash(&file)
        };
        // Only `src/`: nothing to match, so no fragments.
        let common = write("src/Platform.kt");
        assert!(!build_gradle(&ws, &android).unwrap().contains("-Xfragments"));

        let actual = write("src-android/Platform.android.kt");
        let common_test = write("tests/PlatformTest.kt");
        let actual_test = write("tests-android/AndroidTest.kt");
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>()"), "{text}");
        assert!(text.contains("\"-Xmulti-platform\""), "{text}");
        assert!(text.contains("\"-Xfragments=common,android\""), "{text}");
        assert!(text.contains("\"-Xfragment-refines=android:common\""), "{text}");
        let sources = |files: [(&str, &str); 2]| {
            let list: Vec<String> = files.iter().map(|(fragment, file)| format!("{fragment}:{file}")).collect();
            format!("\"-Xfragment-sources={}\"", list.join(","))
        };
        assert!(text.contains(&sources([("common", &common), ("android", &actual)])), "{text}");
        assert!(text.contains(&sources([("common", &common_test), ("android", &actual_test)])), "{text}");
    }

    #[test]
    fn renders_library_and_settings() {
        let (_d, ws) = workspace("kind = \"library\"\n[repositories]\njitpack = \"https://jitpack.io\"\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("com.android.library"));
        assert!(!text.contains("applicationId"));
        let settings = settings_gradle(ws.manifest(), &Settings::default());
        assert!(settings.contains("rootProject.name = \"droid\""));
        assert!(settings.contains("url = uri(\"https://jitpack.io\")"));
        assert!(!settings.contains("dl.google.com"), "google() covers it");
        assert!(settings.contains("        google()\n        mavenCentral()\n        gradlePluginPortal()\n"), "{settings}");
    }

    #[test]
    fn settings_use_the_mirrors() {
        let (_d, ws) = workspace("kind = \"library\"\n[repositories]\njitpack = \"https://jitpack.io\"\n");
        let mut mirrors = Settings::default();
        mirrors
            .merge_str(
                "[mirrors]\n\"*\" = \"https://art.corp/maven\"\n[mirrors.credentials.\"https://art.corp\"]\nusername-env = \"ART_USER\"\npassword-env = \"ART_TOKEN\"\n",
                Path::new("/"),
            )
            .unwrap();
        let settings = settings_gradle(ws.manifest(), &mirrors);
        for public in ["google()", "mavenCentral()", "gradlePluginPortal()", "jitpack.io"] {
            assert!(!settings.contains(public), "{public} in {settings}");
        }
        assert_eq!(settings.matches("url = uri(\"https://art.corp/maven\")").count(), 2, "{settings}");
        assert!(settings.contains("providers.environmentVariable(\"ART_USER\")"), "{settings}");

        let mut google_only = Settings::default();
        google_only.merge_str("[mirrors]\ngoogle = \"https://nexus/google\"\n", Path::new("/")).unwrap();
        let settings = settings_gradle(ws.manifest(), &google_only);
        assert!(settings.contains("url = uri(\"https://nexus/google\")") && settings.contains("mavenCentral()"), "{settings}");
        assert!(!settings.contains("google()"), "{settings}");
    }

    #[test]
    fn settings_keep_the_content_rules() {
        let (_d, ws) = workspace(
            "kind = \"library\"\n[repositories.corp]\nurl = \"https://nexus.corp/maven\"\ninclude-groups = [\"com.corp\", \"com.corp.*\"]\n\
             [repositories.maven-central]\nexclude-groups = [\"com.corp.*\"]\n",
        );
        let settings = settings_gradle(ws.manifest(), &Settings::default());
        let expected = r#"        exclusiveContent {
            forRepository {
                maven {
                    url = uri("https://nexus.corp/maven")
                }
            }
            filter {
                includeGroup("com.corp")
                includeGroupByRegex("com\\.corp(\\..*)?")
            }
        }
"#;
        assert!(settings.contains(expected), "{settings}");
        assert!(settings.contains("        mavenCentral {\n            content {\n                excludeGroupByRegex("), "{settings}");

        let (_d, ws) = workspace("kind = \"library\"\n[repositories]\ndefaults = false\nmirror = \"https://mirror.corp/maven\"\n");
        let settings = settings_gradle(ws.manifest(), &Settings::default());
        let dependencies = settings.split("dependencyResolutionManagement").nth(1).unwrap();
        assert!(!dependencies.contains("google()") && !dependencies.contains("mavenCentral()"), "{settings}");
        assert!(dependencies.contains("url = uri(\"https://mirror.corp/maven\")"), "{settings}");
    }

    #[test]
    fn escapes_kotlin_strings() {
        assert_eq!(kts_string(r#"a"b\c$d"#), r#""a\"b\\c\$d""#);
        assert_eq!(kts_path(Path::new(r"C:\x\y")), "\"C:/x/y\"");
    }

    #[test]
    fn coordinate_rendering() {
        let mut d = Dependency::new("g:a".parse().unwrap(), Some("1".into()));
        d.classifier = Some("natives".into());
        d.extension = Some("zip".into());
        assert_eq!(coord(&d), "g:a:1:natives@zip");
        d.force = true;
        d.transitive = false;
        let line = dependency_line(&d, false);
        assert!(line.contains("isTransitive = false") && line.contains("strictly(\"1\")"), "{line}");
        assert_eq!(configuration(Scope::Provided, false, false), "compileOnly");
        assert_eq!(configuration(Scope::Runtime, true, false), "testImplementation");
        assert_eq!(configuration(Scope::Processor, false, false), "annotationProcessor");
        assert_eq!(configuration(Scope::Processor, true, false), "testAnnotationProcessor");
        assert_eq!(configuration(Scope::Ksp, false, false), "ksp");
        assert_eq!(configuration(Scope::Ksp, true, false), "kspTest");
    }

    #[test]
    fn profile_dependencies_go_to_their_build_type() {
        let leak = Dependency::new("com.squareup.leakcanary:leakcanary-android".parse().unwrap(), Some("2.14".into()));
        let line = profile_dependency_line(&leak, "dev", "debug").unwrap();
        assert!(line.starts_with("    debugImplementation(\"com.squareup.leakcanary:leakcanary-android:2.14\")"), "{line}");
        let mut provided = leak.clone();
        provided.scope = Scope::Provided;
        assert!(profile_dependency_line(&provided, "release", "release").unwrap().starts_with("    releaseCompileOnly("));
        let mut processor = leak;
        processor.scope = Scope::Processor;
        let err = profile_dependency_line(&processor, "dev", "debug").unwrap_err();
        assert!(format!("{err:#}").contains("[profile.dev.dependencies]"), "{err:#}");
    }

    #[test]
    fn chooses_gradle_by_precedence() {
        use kargo_util::shell::{Shell, Verbosity};
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);

        // By default, the Gradle the AGP version needs.
        let (_d, ws) = workspace("");
        let managed = GradleChoice::Managed { version: "9.6.0".to_owned(), pinned: false };
        assert_eq!(choose_gradle(&ws, &config).unwrap(), managed);
        let (_d, ws) = workspace("agp-version = \"8.13.0\"\n");
        assert_eq!(choose_gradle(&ws, &config).unwrap(), GradleChoice::Managed { version: "8.13".to_owned(), pinned: false });
        let described = describe_gradle(&ws, &config).unwrap();
        assert!(described.starts_with("8.13 (default for AGP 8.13.0; not installed yet"), "{described}");

        // `gradle-version` pins it, if AGP can run on it.
        let (_d, ws) = workspace("gradle-version = \"9.7\"\n");
        assert_eq!(choose_gradle(&ws, &config).unwrap(), GradleChoice::Managed { version: "9.7".to_owned(), pinned: true });
        assert!(describe_gradle(&ws, &config).unwrap().contains("gradle-version in kargo.toml"));
        let (_d, ws) = workspace("agp-version = \"9.4.1\"\ngradle-version = \"9.1.0\"\n");
        let err = choose_gradle(&ws, &config).unwrap_err().to_string();
        assert!(err.contains("Gradle 9.1.0 is too old for Android Gradle Plugin 9.4.1, which needs Gradle 9.6.0 or newer"), "{err}");

        // A project wrapper wins over everything, even an incompatible pair.
        let wrapper = ws.root().join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
        std::fs::write(&wrapper, "").unwrap();
        assert_eq!(choose_gradle(&ws, &config).unwrap(), GradleChoice::Project(wrapper));
        assert!(describe_gradle(&ws, &config).unwrap().starts_with("project wrapper at"));
    }

    fn context_for(path: &Path, home: &Path, run: impl FnOnce(&BuildContext)) {
        use crate::workspace::Project;
        use kargo_util::shell::{Shell, Verbosity};
        let project = Project::load(path).unwrap();
        let config = Config::new(home.to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
        let ctx = BuildContext::new(&config, &project, project.current().unwrap_or(0), ProfileKind::Dev).unwrap();
        run(&ctx);
    }

    #[test]
    fn generate_requires_android_manifest() {
        let (dir, _ws) = workspace("");
        let home = tempfile::tempdir().unwrap();
        context_for(&dir.path().join("kargo.toml"), home.path(), |ctx| {
            let err = generate(ctx).unwrap_err().to_string();
            assert!(err.contains("AndroidManifest.xml"), "{err}");
        });
        std::fs::create_dir_all(dir.path().join("android")).unwrap();
        std::fs::write(dir.path().join("android/AndroidManifest.xml"), "<manifest/>").unwrap();
        context_for(&dir.path().join("kargo.toml"), home.path(), |ctx| match generate(ctx) {
            Ok(out) => {
                assert!(out.join("build.gradle.kts").is_file());
                assert!(out.join("local.properties").is_file());
            }
            Err(e) => assert!(e.to_string().contains("Android SDK not found") || e.to_string().contains("lacks"), "{e}"),
        });
    }

    #[test]
    fn modules_have_their_own_r_class() {
        assert!(GRADLE_PROPERTIES.contains("android.nonTransitiveRClass=true\n"), "{GRADLE_PROPERTIES}");
    }

    #[test]
    fn android_path_dependencies_are_subprojects() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("kargo.toml", "[workspace]\nmembers = [\"app\", \"ui\", \"model\"]\n[workspace.kotlin]\nversion = \"2.3.0\"\n");
        write(
            "app/kargo.toml",
            "[package]\nname = \"app\"\nversion = \"1.0.0\"\n[target.android]\nnamespace = \"com.example.app\"\n[dependencies]\nui = { path = \"../ui\" }\n\"com.squareup.okhttp3:okhttp\" = \"4.12.0\"\n[dev-dependencies]\nmodel = { path = \"../model\" }\n",
        );
        write(
            "ui/kargo.toml",
            "[package]\nname = \"ui\"\nversion = \"1.0.0\"\n[target.android]\nnamespace = \"com.example.ui\"\nkind = \"library\"\n[frameworks]\nserialization = true\n[dependencies]\nmodel = { path = \"../model\", exported = true }\n",
        );
        write("model/kargo.toml", "[package]\nname = \"model\"\nversion = \"1.0.0\"\n[target.jvm]\n");
        for pkg in ["app", "ui"] {
            write(&format!("{pkg}/android/AndroidManifest.xml"), "<manifest/>");
        }
        // The jar of `model` would need a compiler; pretend it is built.
        context_for(&dir.path().join("app/kargo.toml"), home.path(), |ctx| {
            let model = ctx.project.packages().iter().position(|p| p.name() == "model").unwrap();
            let jar = ctx.project.package(model).output_dir(&ProfileKind::Dev, TargetKind::Jvm).join("model-1.0.0.jar");
            ctx.built_artifact("model/jvm".to_owned(), || Ok(Some(jar.clone()))).unwrap();

            let mut subprojects = Vec::new();
            let app = path_dependency_lines(ctx, ctx.package, &mut subprojects).unwrap();
            assert_eq!(app[0], "    implementation(project(\":ui\"))");
            assert_eq!(app[1], format!("    testImplementation(files({}))", kts_path(&jar)));
            let ui = path_dependency_lines(ctx, subprojects[0], &mut subprojects).unwrap();
            assert_eq!(ui, [format!("    api(files({}))", kts_path(&jar))]);

            let ui_ws = ctx.project.package(subprojects[0]);
            assert_eq!(android_subprojects(ctx, ctx.package), subprojects);
            let build_dir = ui_ws.work_dir("android").join("build");
            let module =
                GradleModule { subproject: true, dependencies: ui.clone(), build_dir: Some(build_dir.clone()), ..Default::default() };
            let text = build_gradle_with(ui_ws, ui_ws.manifest().targets.android.as_ref().unwrap(), &module).unwrap();
            assert!(text.contains("    id(\"com.android.library\")\n"), "{text}");
            assert!(text.contains("    id(\"org.jetbrains.kotlin.plugin.serialization\")\n"), "{text}");
            assert!(!text.contains("version"), "the root declares the versions: {text}");
            assert!(text.contains(&ui[0]));
            // The library builds into its own directory, where `kargo build -p ui` puts its AAR.
            assert!(text.contains(&format!("layout.buildDirectory.set(file({}))", kts_path(&build_dir))), "{text}");

            // Building both, the app's Gradle run assembles the library too.
            let ui_ctx = ctx.for_package(subprojects[0]);
            assert_eq!(built_by(&ui_ctx), None, "alone, the library builds itself");
            ctx.select(&[subprojects[0], ctx.package]);
            assert_eq!(built_by(&ui_ctx), Some(ctx.package));
            assert_eq!(built_by(ctx), None, "an application is never built by another package");

            let settings = settings_gradle_with(ctx.ws.manifest(), &[("ui", ui_ws.manifest())], &Settings::default());
            assert!(settings.contains("include(\":ui\")\nproject(\":ui\").projectDir = file(\"modules/ui\")"), "{settings}");
            let root = GradleModule {
                subproject_plugins: plugins(ui_ws.manifest(), ui_ws.manifest().targets.android.as_ref().unwrap()),
                dependencies: app,
                ..Default::default()
            };
            let text = build_gradle_with(ctx.ws, ctx.ws.manifest().targets.android.as_ref().unwrap(), &root).unwrap();
            assert!(text.contains(&format!("id(\"com.android.library\") version \"{AGP_VERSION}\" apply false")), "{text}");
            assert!(text.contains("id(\"org.jetbrains.kotlin.plugin.serialization\") version \"2.3.0\" apply false"), "{text}");
            assert!(text.contains("    implementation(project(\":ui\"))"), "{text}");
        });

        // One build has one Android Gradle Plugin.
        write(
            "ui/kargo.toml",
            "[package]\nname = \"ui\"\nversion = \"1.0.0\"\n[target.android]\nnamespace = \"com.example.ui\"\nkind = \"library\"\nagp-version = \"9.0.0\"\n",
        );
        context_for(&dir.path().join("app/kargo.toml"), home.path(), |ctx| {
            let model = ctx.project.packages().iter().position(|p| p.name() == "model").unwrap();
            let jar = ctx.project.package(model).output_dir(&ProfileKind::Dev, TargetKind::Jvm).join("model-1.0.0.jar");
            ctx.built_artifact("model/jvm".to_owned(), || Ok(Some(jar.clone()))).unwrap();
            let err = generate(ctx).unwrap_err().to_string();
            assert!(err.contains("its dependency `ui` with 9.0.0") && err.contains("agp-version"), "{err}");
        });

        // An Android application can't be a dependency.
        write("ui/kargo.toml", "[package]\nname = \"ui\"\nversion = \"1.0.0\"\n[target.android]\nnamespace = \"com.example.ui\"\n");
        context_for(&dir.path().join("app/kargo.toml"), home.path(), |ctx| {
            let err = path_dependency_lines(ctx, ctx.package, &mut Vec::new()).unwrap_err().to_string();
            assert!(err.contains("`app` depends on `ui`, an Android application"), "{err}");
        });
    }
}
