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

use crate::builtins;
use crate::ops::build::{announce, compiling};
use crate::ops::context::BuildContext;
use crate::ops::test::{emit_report_event, emit_summary, human_out};
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::testreport::{self, TestSummary};
use kargo_manifest::{AndroidConfig, AndroidTarget, BuildConfigValue, Dependency, Manifest, Scope, TargetKind};
use kargo_toolchain::gradle::{self as toolchain_gradle, AgpGradle};
use kargo_toolchain::{GradleDistribution, Jdk};
use kargo_util::config::Config;
use kargo_util::process;

pub const AGP_VERSION: &str = "9.4.1";
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
    let conf = configuration(d.scope, test, d.exported);
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

/// Renders `settings.gradle.kts` for a single-project build.
pub fn settings_gradle(manifest: &Manifest) -> String {
    settings_gradle_with(manifest, &[])
}

/// Renders `settings.gradle.kts`: the root project named after `manifest`'s package, and the
/// `modules` (name and manifest) as subprojects in `modules/<name>`. The repositories of all of them apply.
pub fn settings_gradle_with(manifest: &Manifest, modules: &[(&str, &Manifest)]) -> String {
    let mut repos = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for repo in manifest.repositories.iter().chain(modules.iter().flat_map(|(_, m)| &m.repositories)) {
        if repo.url == kargo_maven::repo::MAVEN_CENTRAL || repo.url == kargo_maven::repo::GOOGLE || seen.contains(&repo.url.as_str()) {
            continue;
        }
        seen.push(&repo.url);
        let _ = write!(repos, "        maven {{\n            url = uri({})\n", kts_string(&repo.url));
        if repo.credentials.is_some() {
            let _ = writeln!(
                repos,
                "            credentials {{\n                username = providers.environmentVariable(\"KARGO_REPO_USER_{0}\").orNull\n                password = providers.environmentVariable(\"KARGO_REPO_PASSWORD_{0}\").orNull\n            }}",
                repo.name.to_uppercase().replace('-', "_")
            );
        }
        repos.push_str("        }\n");
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
        google()
        mavenCentral()
        gradlePluginPortal()
    }}
}}

dependencyResolutionManagement {{
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {{
        google()
        mavenCentral()
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
    let _ = writeln!(s, "android {{");
    let _ = writeln!(s, "    namespace = {}", kts_string(&cfg.namespace));
    let _ = writeln!(s, "    compileSdk = {}", cfg.compile_sdk);
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
    let _ = writeln!(s, "    }}");
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
    let src_dirs = [root.join("src"), root.join(TargetKind::Android.source_dir_name())];
    let test_dirs = [root.join("tests"), root.join("tests-android")];
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
    let _ = writeln!(s, "            assets.setSrcDirs(listOf({}))", kts_path(&root.join("android/assets")));
    let _ = writeln!(s, "            resources.setSrcDirs(listOf({}))", kts_path(&ws.resources()));
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

fn has_build_config(cfg: &AndroidConfig) -> bool {
    !cfg.build_config.is_empty() || cfg.flavors.values().any(|f| !f.build_config.is_empty())
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

const GRADLE_PROPERTIES: &str =
    "# Generated by Kargo.\norg.gradle.jvmargs=-Xmx2g -Dfile.encoding=UTF-8\nandroid.useAndroidX=true\nkotlin.code.style=official\n";

/// Locates the Android SDK.
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

/// Writes the Gradle project; returns its directory. The Android libraries the package depends
/// on through `path` are subprojects of it, in `modules/<name>`.
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
        for p in plugins(pkg.manifest(), pkg_android) {
            if !subproject_plugins.iter().any(|s| s.id == p.id) {
                subproject_plugins.push(GradlePlugin { applied: false, ..p });
            }
        }
        let module = GradleModule { subproject: true, subproject_plugins: Vec::new(), dependencies: lines.clone() };
        write(&format!("modules/{}/build.gradle.kts", pkg.name()), &build_gradle_with(pkg, pkg_android, &module)?)?;
    }
    let names: Vec<(&str, &Manifest)> =
        modules.iter().map(|(i, _)| (ctx.project.package(*i).name(), ctx.project.package(*i).manifest())).collect();
    write("settings.gradle.kts", &settings_gradle_with(ws.manifest(), &names))?;
    let root = GradleModule { subproject: false, subproject_plugins, dependencies: root_lines };
    write("build.gradle.kts", &build_gradle_with(ws, android, &root)?)?;
    write("gradle.properties", GRADLE_PROPERTIES)?;
    if let Some(sdk) = android_sdk() {
        let escaped = sdk.to_string_lossy().replace('\\', "\\\\").replace(':', "\\:");
        write("local.properties", &format!("sdk.dir={escaped}\n"))?;
    } else {
        bail!("Android SDK not found; set ANDROID_HOME to your SDK directory");
    }
    Ok(dir)
}

/// The AGP build type of `profile`.
fn build_type(profile: ProfileKind) -> &'static str {
    match profile {
        ProfileKind::Dev => "debug",
        ProfileKind::Release => "release",
    }
}

fn android_config<'a>(ctx: &'a BuildContext) -> Result<&'a AndroidConfig> {
    Ok(&ctx.ws.manifest().targets.android.as_ref().context("the Android target is not enabled")?.config)
}

/// The variant `check`, `test` and `doc` use: the build type of the profile with the
/// first flavor of each dimension, capitalized for task names (`FreeRelease`).
fn default_variant(ctx: &BuildContext) -> Result<String> {
    let cfg = android_config(ctx)?;
    Ok(capitalize(&variant_name(&default_flavors(cfg, &[])?, build_type(ctx.profile))))
}

/// Checks, before Gradle runs, what signing the build type of `ctx` needs: the keystore
/// exists and the environment variables with its passwords are set.
fn check_signing(ctx: &BuildContext) -> Result<()> {
    let cfg = android_config(ctx)?;
    let build_type = build_type(ctx.profile);
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

/// `kargo fetch` for Android: downloads the Gradle the build uses and prepares its
/// wrapper. Gradle itself fetches the dependencies during the build.
pub fn fetch(ctx: &BuildContext) -> Result<()> {
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
    cmd.env("JAVA_HOME", &jdk.home);
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

/// `kargo build --target android`: assembles the APK/AAR of every flavor.
pub fn build(ctx: &BuildContext) -> Result<PathBuf> {
    let cfg = android_config(ctx)?;
    compiling(ctx, TargetKind::Android, ", gradle");
    check_signing(ctx)?;
    let build_type = build_type(ctx.profile);
    let task = format!("assemble{}", capitalize(build_type));
    if !gradle(ctx, std::slice::from_ref(&task))? {
        bail!("Gradle `{task}` failed");
    }
    warn_unsigned(ctx)?;
    let dir = ctx.ws.work_dir("android").join("build/outputs");
    let out = match (cfg.kind == "application", cfg.flavors.is_empty()) {
        (true, true) => dir.join("apk").join(build_type),
        // Gradle writes apk/<flavor>/<build type>/ per flavor.
        (true, false) => dir.join("apk"),
        (false, _) => dir.join("aar"),
    };
    Ok(out)
}

/// `kargo package --target android`: builds the APKs and Android App Bundles (AABs) of
/// an application, or the AARs of a library, for every flavor, and copies them to
/// `target/<profile>/android`. Returns the copied files, App Bundles first.
pub fn package(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let cfg = android_config(ctx)?;
    let is_app = cfg.kind == "application";
    compiling(ctx, TargetKind::Android, ", gradle");
    check_signing(ctx)?;
    let build_type = build_type(ctx.profile);
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
    let dest = ctx.ws.output_dir(ctx.profile, TargetKind::Android);
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

/// `kargo run --target android`: installs on a connected device and launches. `flavors`
/// (`--flavor`) picks the flavor of a dimension; the others use their first flavor.
pub fn run(ctx: &BuildContext, flavors: &[String]) -> Result<i32> {
    let cfg = android_config(ctx)?;
    let flavors = validate_run(ctx, flavors)?;
    let build_type = build_type(ctx.profile);
    if !flavors.is_empty() {
        ctx.shell().verbose("Using", format!("flavor {}", flavors.join(", ")));
    }
    let task = format!("install{}", capitalize(&variant_name(&flavors, build_type)));
    if !gradle(ctx, std::slice::from_ref(&task))? {
        bail!("installing the app failed; is a device or emulator connected (`adb devices`)?");
    }
    let adb = android_sdk()
        .map(|sdk| sdk.join("platform-tools").join(if cfg!(windows) { "adb.exe" } else { "adb" }))
        .filter(|p| p.is_file())
        .or_else(|| process::find_on_path("adb"))
        .context("`adb` not found; install the Android SDK platform-tools")?;
    let app_id = application_id(cfg, &flavors);
    let mut cmd = Command::new(adb);
    cmd.args(["shell", "monkey", "-p", &app_id, "-c", "android.intent.category.LAUNCHER", "1"]);
    ctx.shell().status("Launching", &app_id);
    let status = process::status(ctx.shell(), &mut cmd, "adb")?;
    Ok(status.code().unwrap_or(1))
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
    fn renders_library_and_settings() {
        let (_d, ws) = workspace("kind = \"library\"\n[repositories]\njitpack = \"https://jitpack.io\"\n");
        let android = ws.manifest().targets.android.clone().unwrap();
        let text = build_gradle(&ws, &android).unwrap();
        assert!(text.contains("com.android.library"));
        assert!(!text.contains("applicationId"));
        let settings = settings_gradle(ws.manifest());
        assert!(settings.contains("rootProject.name = \"droid\""));
        assert!(settings.contains("url = uri(\"https://jitpack.io\")"));
        assert!(!settings.contains("dl.google.com"), "google() covers it");
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
            Err(e) => assert!(e.to_string().contains("Android SDK not found"), "{e}"),
        });
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
            let jar = ctx.project.package(model).output_dir(ProfileKind::Dev, TargetKind::Jvm).join("model-1.0.0.jar");
            ctx.built_artifact("model/jvm".to_owned(), || Ok(Some(jar.clone()))).unwrap();

            let mut subprojects = Vec::new();
            let app = path_dependency_lines(ctx, ctx.package, &mut subprojects).unwrap();
            assert_eq!(app[0], "    implementation(project(\":ui\"))");
            assert_eq!(app[1], format!("    testImplementation(files({}))", kts_path(&jar)));
            let ui = path_dependency_lines(ctx, subprojects[0], &mut subprojects).unwrap();
            assert_eq!(ui, [format!("    api(files({}))", kts_path(&jar))]);

            let ui_ws = ctx.project.package(subprojects[0]);
            let module = GradleModule { subproject: true, dependencies: ui.clone(), ..Default::default() };
            let text = build_gradle_with(ui_ws, ui_ws.manifest().targets.android.as_ref().unwrap(), &module).unwrap();
            assert!(text.contains("    id(\"com.android.library\")\n"), "{text}");
            assert!(text.contains("    id(\"org.jetbrains.kotlin.plugin.serialization\")\n"), "{text}");
            assert!(!text.contains("version"), "the root declares the versions: {text}");
            assert!(text.contains(&ui[0]));

            let settings = settings_gradle_with(ctx.ws.manifest(), &[("ui", ui_ws.manifest())]);
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

        // An Android application can't be a dependency.
        write("ui/kargo.toml", "[package]\nname = \"ui\"\nversion = \"1.0.0\"\n[target.android]\nnamespace = \"com.example.ui\"\n");
        context_for(&dir.path().join("app/kargo.toml"), home.path(), |ctx| {
            let err = path_dependency_lines(ctx, ctx.package, &mut Vec::new()).unwrap_err().to_string();
            assert!(err.contains("`app` depends on `ui`, an Android application"), "{err}");
        });
    }
}
