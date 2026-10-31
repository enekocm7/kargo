//! `kargo script`: runs one Kotlin file without a project. A `.kts` script takes its dependencies,
//! repositories, Kotlin version, JDK and frameworks from its `@file:` annotations; a `.kt` file
//! takes them from a `/* kargo ... */` front matter. Both resolve with Kargo's resolver and use
//! its JDKs and caches.
//!
//! A `.kts` script is compiled with the Kotlin scripting compiler, which runs it as it compiles.
//! The compiled classes go to a cache entry, and later runs start them with `java` without
//! compiling again. A `.kt` file is compiled once and started the same way. Each entry is keyed by
//! the script's code and everything that changes what it compiles to, and lives in
//! `~/.kargo/cache/scripts`.
//!
//! A `.kt` file also runs as a native executable with `--target native`: it is compiled with the
//! cached Kotlin/Native toolchain into a binary, and a second run starts the cached binary without
//! rebuilding. `.kts` scripts and `-e` code stay on the JVM: the scripting compiler is JVM-only.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use kargo_compiler::mainclass::find_entry_point_in;
use kargo_compiler::native::{NativeCompilation, NativeLink};
use kargo_compiler::{ResolvedPlugin, SourceSet, compiler_jvm_options, plugin_args};
use kargo_manifest::script::{self, ScriptTarget, Settings};
use kargo_manifest::{DEFAULT_KOTLIN_VERSION, JdkRequest, Manifest, NativeKind, TargetKind};
use kargo_maven::{DependencyRequest, MavenClient, ModuleId, Platform, Repository, Resolver};
use kargo_toolchain::kotlin::{JVM_COMPILER_MAIN, KOTLIN_GROUP, KotlinCompiler, check_compiler_jdk, fetch_jar};
use kargo_toolchain::native::Host;
use kargo_toolchain::{Jdk, NativeToolchain, jdks};
use kargo_util::config::Config;
use kargo_util::fs::{join_paths, remove_dir_all_if_exists, write_string_atomic};
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash, write_argfile};
use kargo_util::shell::Shell;

use crate::builtins::{self, FRAMEWORK_NAMES};
use crate::ops::context::{MIN_JDK, to_request};

/// Written into every cache key; bump it when what an entry holds changes.
const FORMAT: &str = "4";
/// The script template that gives a script its `args`.
const SCRIPT_TEMPLATE: &str = "kotlin.script.templates.standard.ScriptTemplateWithArgs";
/// The Kotlin scripting runtime a compiled script needs at run time.
const SCRIPTING_RUNTIME: [&str; 3] = ["kotlin-script-runtime", "kotlin-scripting-common", "kotlin-scripting-jvm"];
/// The compiler's exit code when the script threw while it ran.
const SCRIPT_THREW: i32 = 3;
/// The name of the script `kargo script -e` runs.
const EXPRESSION: &str = "expression.kts";
/// The launcher source compiled beside a `.kts` script, and the class it gives.
const LAUNCHER_SOURCE: &str = "KargoLaunch.kt";
const LAUNCHER_CLASS: &str = "KargoLaunchKt";

/// What `kargo script` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A `.kts` script, or a `.kt` file with a front matter.
    File(PathBuf),
    /// Kotlin code given on the command line (`kargo script -e`).
    Expression(String),
}

/// A script ready to build.
struct Script {
    /// The file name, shown in messages; a `.kts` name also names its class.
    name: String,
    /// Compiled as a script (`.kts`) rather than as a Kotlin file (`.kt`).
    is_script: bool,
    /// The code to compile: a `.kts` file without its annotations, or a `.kt` file.
    code: String,
    /// The `.kt` file, compiled where it is so compiler messages name it.
    file: Option<PathBuf>,
    settings: Settings,
}

/// Where a script runs and what its process environment adds to Kargo's own: the working directory
/// and variables of a task or hook plugin. The default is the directory and environment `kargo` runs in.
#[derive(Debug, Clone, Default)]
pub struct Surroundings {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl Surroundings {
    fn apply(&self, cmd: &mut Command) {
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
    }
}

/// Runs the script with `args` and returns its exit code.
pub fn run(config: &Config, source: &Source, args: &[String]) -> Result<i32> {
    run_in(config, source, args, &Surroundings::default())
}

/// Runs the script for `target`, overriding what its header declares, and returns its exit code.
pub fn run_with(config: &Config, source: &Source, args: &[String], target: Option<ScriptTarget>) -> Result<i32> {
    run_in_with(config, source, args, &Surroundings::default(), target)
}

/// Like [`run`], in the working directory and with the environment of `around`. The first run of a
/// `.kts` script happens inside the Kotlin compiler, which gets them too. Pass `source` as an absolute
/// path when `around` changes the directory.
pub fn run_in(config: &Config, source: &Source, args: &[String], around: &Surroundings) -> Result<i32> {
    run_in_with(config, source, args, around, None)
}

/// Like [`run_in`], but `target` overrides what the script's header declares.
pub fn run_in_with(config: &Config, source: &Source, args: &[String], around: &Surroundings, target: Option<ScriptTarget>) -> Result<i32> {
    let script = load(source)?;
    let requested = target.or(script.settings.target).unwrap_or_default();
    match requested {
        ScriptTarget::Jvm => run_jvm(config, &script, args, around),
        ScriptTarget::Native => run_native(config, &script, args, around),
    }
}

fn run_jvm(config: &Config, script: &Script, args: &[String], around: &Surroundings) -> Result<i32> {
    let shell = config.shell();
    let kotlin = kotlin_version(&script.settings)?;
    let frameworks = frameworks(&script.settings)?;
    if script.is_script {
        check_script_frameworks(&script.name, &kotlin, &frameworks)?;
    }
    let jdk_request = script.settings.jdk.clone().unwrap_or_default();
    let repositories = repositories(&script.settings);
    let entry = config.scripts_cache().join(cache_key(script, &kotlin, &jdk_request, &repositories, &frameworks, ScriptTarget::Jvm, None));

    let needs = jdks::JdkNeeds { max: kargo_toolchain::kotlin::max_compiler_jdk(&kotlin), ..Default::default() };
    let jdk = jdks::resolve_for(config, &jdk_request, needs)?;
    if let Some(cached) = Cached::read(&entry) {
        shell.verbose("Cached", format!("`{}`", script.name));
        return launch(config, &jdk, &cached.classes, &cached.classpath, &cached.main_class, args, around);
    }
    jdk.require_version(MIN_JDK)?;
    check_compiler_jdk(&kotlin, &jdk)?;

    shell.status("Compiling", format!("`{}`", script.name));
    let built = build(config, script, &kotlin, &jdk, &repositories, &frameworks, &entry, args, around)?;
    match built.ran {
        Some(code) => Ok(code),
        None => launch(config, &jdk, &entry.join("classes"), &built.classpath, &built.main_class, args, around),
    }
}

/// Runs the script as a native executable for this machine, building it first unless the cache
/// already holds the binary. Only `.kt` files run natively: the Kotlin scripting compiler is
/// JVM-only, and a `.kts` file's top-level statements have no `fun main` to link.
fn run_native(config: &Config, script: &Script, args: &[String], around: &Surroundings) -> Result<i32> {
    let shell = config.shell();
    if script.is_script {
        let what = if script.name == EXPRESSION { "`-e` code".to_owned() } else { format!("`{}`", script.name) };
        bail!(
            "{what} is a .kts script, but `--target native` needs a .kt file with a `fun main`; \
             move the code to a .kt file with a `/* kargo ... */` front matter"
        );
    }
    let file = script.file.as_ref().with_context(|| format!("`{}` has no file to compile", script.name))?;
    let kotlin = kotlin_version(&script.settings)?;
    let frameworks = frameworks(&script.settings)?;
    let jdk_request = script.settings.jdk.clone().unwrap_or_default();
    let repositories = repositories(&script.settings);
    let konan_target = Host::current()?.konan_target.to_owned();
    let entry = config.scripts_cache().join(cache_key(
        script,
        &kotlin,
        &jdk_request,
        &repositories,
        &frameworks,
        ScriptTarget::Native,
        Some(&konan_target),
    ));

    if let Some(cached) = NativeCached::read(&entry, &konan_target) {
        shell.verbose("Cached", format!("`{}`", script.name));
        return launch_native(config, &cached.binary, args, around);
    }

    shell.status("Compiling", format!("`{}`", script.name));
    let binary = build_native(config, script, file, &kotlin, &repositories, &frameworks, &konan_target, &entry)?;
    launch_native(config, &binary, args, around)
}

/// Compiles the `.kt` script `file` into a native binary in `entry`, and marks the entry complete.
/// Returns the binary.
#[allow(clippy::too_many_arguments)]
fn build_native(
    config: &Config,
    script: &Script,
    file: &Path,
    kotlin: &str,
    repositories: &[Repository],
    frameworks: &[&str],
    konan_target: &str,
    entry: &Path,
) -> Result<PathBuf> {
    let client = MavenClient::new(config, repositories.to_vec());
    let manifest = framework_manifest(kotlin, frameworks)?;
    let contributions = builtins::contributions(&manifest, TargetKind::Native, entry)?;

    let mut roots: Vec<DependencyRequest> = script
        .settings
        .dependencies
        .iter()
        .map(|d| DependencyRequest::new(ModuleId::new(&d.group, &d.artifact), Some(d.version.clone())))
        .collect();
    roots.extend(contributions.dependencies.iter().map(to_request));
    let libraries = Resolver::new(&client, Platform::Native(konan_target.to_owned()))
        .resolve(&roots)
        .with_context(|| format!("failed to resolve the dependencies of `{}` for {konan_target}", script.name))?
        .files();

    let toolchain = NativeToolchain::ensure(config, &client, kotlin).with_context(|| format!("failed to obtain Kotlin/Native {kotlin}"))?;
    toolchain.ensure_dependencies(config, konan_target)?;

    let mut plugins = Vec::new();
    for p in &contributions.plugins {
        let version = p.version.as_deref().unwrap_or(kotlin);
        let jar = fetch_jar(&client, &p.group, &p.artifact, version)
            .with_context(|| format!("failed to fetch compiler plugin {}:{}:{version}", p.group, p.artifact))?;
        plugins.push(ResolvedPlugin { jar, options: builtins::plugin_option_args(p) });
    }

    let entry_point = find_entry_point_in(file, true)?.native_entry();
    let sources = SourceSet::new(vec![file.to_path_buf()], Vec::new(), "native");
    let unit = NativeCompilation {
        name: "script",
        konan_target: konan_target.to_owned(),
        kind: NativeKind::Executable,
        sources,
        libraries,
        plugins: &plugins,
        kotlin_args: &contributions.compiler_args,
        entry_point: Some(entry_point),
        link: NativeLink::default(),
        output_dir: entry.to_path_buf(),
        debug: true,
        optimize: false,
        test_runner: false,
        exported: Vec::new(),
        bundle_id: None,
    };
    // Remove a binary a failed build left behind, so a cache hit never runs it.
    let artifact = unit.artifact();
    let _ = std::fs::remove_file(&artifact);
    std::fs::create_dir_all(entry).with_context(|| format!("failed to create {}", entry.display()))?;
    unit.compile(config.shell(), &toolchain, entry, &|| {})?;
    if !artifact.is_file() {
        bail!("could not compile `{}` for {konan_target} due to previous errors", script.name);
    }
    write_string_atomic(&entry.join("complete"), FORMAT)?;
    Ok(artifact)
}

/// A finished native cache entry: the binary a previous run linked.
struct NativeCached {
    binary: PathBuf,
}

impl NativeCached {
    /// The entry at `dir` for `konan_target`, if a build finished there and the binary is still present.
    fn read(dir: &Path, konan_target: &str) -> Option<Self> {
        if !dir.join("complete").is_file() {
            return None;
        }
        let binary = dir.join(format!("script.{}", kargo_toolchain::native::executable_extension(konan_target)));
        binary.is_file().then_some(Self { binary })
    }
}

/// Runs a native script binary with `args`.
fn launch_native(config: &Config, binary: &Path, args: &[String], around: &Surroundings) -> Result<i32> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    around.apply(&mut cmd);
    config.shell().verbose("Running", format!("`{}`", binary.display()));
    let status = cmd.status().with_context(|| format!("failed to start {}", binary.display()))?;
    Ok(kargo_util::process::exit_code_of(status))
}

/// Reads the script and the settings it declares.
fn load(source: &Source) -> Result<Script> {
    match source {
        Source::Expression(code) => {
            let parsed = script::parse_kts(code)?;
            let settings = script::settings_from_annotations(code, &parsed.annotations)?;
            Ok(Script { name: EXPRESSION.to_owned(), is_script: true, code: parsed.code, file: None, settings })
        }
        Source::File(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            match path.extension().and_then(|e| e.to_str()) {
                Some("kts") => {
                    let parsed = script::parse_kts(&text).with_context(|| format!("invalid header in {}", path.display()))?;
                    let settings = script::settings_from_annotations(&text, &parsed.annotations)
                        .with_context(|| format!("invalid header in {}", path.display()))?;
                    Ok(Script { name, is_script: true, code: parsed.code, file: None, settings })
                }
                Some("kt") => {
                    let settings = script::parse_kt_front_matter(&text)
                        .with_context(|| format!("invalid front matter in {}", path.display()))?
                        .unwrap_or_default();
                    Ok(Script { name, is_script: false, code: text, file: Some(path.clone()), settings })
                }
                _ => bail!("`kargo script` runs a .kts script or a .kt file, not {}", path.display()),
            }
        }
    }
}

fn kotlin_version(settings: &Settings) -> Result<String> {
    let version = settings.kotlin.clone().unwrap_or_else(|| DEFAULT_KOTLIN_VERSION.to_owned());
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        bail!("invalid Kotlin version `{version}` in @file:Kotlin");
    }
    Ok(version)
}

/// The framework names the script enables, checked against the ones Kargo knows.
fn frameworks(settings: &Settings) -> Result<Vec<&'static str>> {
    let mut out: Vec<&'static str> = Vec::new();
    for name in &settings.frameworks {
        let Some(known) = FRAMEWORK_NAMES.iter().find(|f| *f == name) else {
            return Err(kargo_util::help::unknown(
                format!("unknown framework `{name}`; expected one of {}", FRAMEWORK_NAMES.join(", ")),
                name,
                FRAMEWORK_NAMES,
            ));
        };
        if !out.contains(known) {
            out.push(known);
        }
    }
    Ok(out)
}

/// Fails for a framework that needs a Kotlin compiler plugin: scripts are compiled by the scripting
/// compiler, which does not run compiler plugins, so such a framework only works in a `.kt` file.
fn check_script_frameworks(name: &str, kotlin: &str, frameworks: &[&str]) -> Result<()> {
    let contributions = builtins::contributions(&framework_manifest(kotlin, frameworks)?, TargetKind::Jvm, Path::new(""))?;
    match contributions.plugins.first() {
        Some(plugin) => bail!(
            "`{name}` needs the Kotlin compiler plugin {}, and scripts cannot run compiler plugins; \
             move the code to a .kt file with a `/* kargo ... */` front matter that lists the framework",
            plugin.artifact
        ),
        None => Ok(()),
    }
}

/// A problem in a script's header, with the bytes it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub range: Range<usize>,
    pub message: String,
}

/// The problems `kargo script` would report for the header of a `.kts` script, found without
/// compiling it: a malformed annotation, an unknown framework or one that needs a compiler plugin,
/// or a native target (only `.kt` files run natively).
/// The language server shows them as diagnostics.
pub fn problems(text: &str) -> Vec<Problem> {
    let parsed = match script::parse_kts(text) {
        Ok(parsed) => parsed,
        Err(error) => {
            let message = format!("{error:#}");
            let line = message.strip_prefix("line ").and_then(|rest| rest.split(':').next()?.parse().ok()).unwrap_or(1);
            return vec![Problem { range: line_span(text, line), message }];
        }
    };
    let mut out = Vec::new();
    for annotation in &parsed.annotations {
        let checked = script::settings_from_annotations(text, std::slice::from_ref(annotation)).and_then(|settings| {
            if settings.target == Some(ScriptTarget::Native) {
                bail!(
                    "`@file:Target(\"native\")` needs a .kt file with a `fun main`; \
                     move the code to a .kt file with a `/* kargo ... */` front matter"
                );
            }
            let kotlin = kotlin_version(&settings)?;
            let frameworks = frameworks(&settings)?;
            check_script_frameworks("this script", &kotlin, &frameworks)
        });
        if let Err(error) = checked {
            out.push(Problem { range: annotation.span.clone(), message: format!("{error:#}") });
        }
    }
    out
}

/// The byte range of the 1-based `line` of `text`, without its line break.
fn line_span(text: &str, line: usize) -> Range<usize> {
    let mut start = 0;
    for (index, row) in text.split('\n').enumerate() {
        if index + 1 == line {
            return start..start + row.len();
        }
        start += row.len() + 1;
    }
    text.len()..text.len()
}

/// The repositories a `.kts` script declares, after Maven Central, for the language server's
/// version completion. A script that does not parse declares none.
pub fn declared_repositories(text: &str) -> Vec<Repository> {
    let settings = settings_of(text).unwrap_or_default();
    repositories(&settings)
}

/// The settings of a `.kts` script's annotations, which an error from a malformed one fails.
fn settings_of(text: &str) -> Result<Settings> {
    let parsed = script::parse_kts(text)?;
    script::settings_from_annotations(text, &parsed.annotations)
}

/// What a `.kts` script declares for its dependencies, for the language server to compare when the
/// script changes: its `@file:DependsOn` coordinates, then its `@file:Repository` URLs. Empty for a
/// script that declares neither, or that does not parse (its problems say why).
pub fn declared_dependencies(text: &str) -> Vec<String> {
    let Ok(settings) = settings_of(text) else { return Vec::new() };
    let coordinates = settings.dependencies.iter().map(ToString::to_string);
    coordinates
        .chain(settings.repositories.iter().filter(|url| url.as_str() != "mavenCentral").map(|url| format!("repository {url}")))
        .collect()
}

/// The libraries a `.kts` script's `@file:DependsOn` declarations resolve to, for the language server.
/// They are listed as `kargo metadata` lists a package's dependencies (one JVM target whose `compile`
/// scope holds every resolved file, with the sources jar of its module when one is available), so the
/// server reads them as it reads a project's. Sources jars are downloaded as `kargo fetch --sources`
/// does when `fetch_sources` is set, or read from the cache when it is not or the network is not
/// reachable. A script without dependencies gets an empty list.
pub fn dependency_metadata(config: &Config, text: &str, fetch_sources: bool) -> Result<Value> {
    let settings = settings_of(text).context("invalid script header")?;
    let mut entries = Vec::new();
    if !settings.dependencies.is_empty() {
        let client = MavenClient::new(config, repositories(&settings));
        let roots: Vec<DependencyRequest> = settings
            .dependencies
            .iter()
            .map(|d| DependencyRequest::new(ModuleId::new(&d.group, &d.artifact), Some(d.version.clone())))
            .collect();
        let resolution = Resolver::new(&client, Platform::Jvm).resolve(&roots).context("failed to resolve the script's dependencies")?;
        for node in &resolution.nodes {
            let sources = if fetch_sources {
                client.fetch_sources(&node.module, &node.version).ok().flatten()
            } else {
                client.cached_sources(&node.module, &node.version, &node.files)
            };
            // Without sources, the Javadoc jar documents the library's compiled declarations.
            let javadoc = match &sources {
                Some(_) => None,
                None if fetch_sources => client.fetch_javadoc(&node.module, &node.version).ok().flatten(),
                None => client.cached_javadoc(&node.module, &node.version, &node.files),
            };
            for file in &node.files {
                entries.push(json!({
                    "module": node.module.to_string(),
                    "version": node.version,
                    "path": file.display().to_string(),
                    "sources": sources.as_ref().map(|path| path.display().to_string()),
                    "javadoc": javadoc.as_ref().map(|path| path.display().to_string()),
                }));
            }
        }
    }
    Ok(json!({ "packages": [{ "targets": [{ "kind": "jvm", "dependencies": [{ "compile": entries, "test": [] }] }] }] }))
}

/// Maven Central, then the script's own repositories in declaration order.
fn repositories(settings: &Settings) -> Vec<Repository> {
    let mut repos = vec![Repository::maven_central()];
    for url in settings.repositories.iter().filter(|url| url.as_str() != "mavenCentral") {
        if !repos.iter().any(|r| r.url == *url) {
            repos.push(Repository::new(format!("script-{}", repos.len()), url.clone()));
        }
    }
    repos
}

/// The directory of the cache entry for a script: a hash of everything that changes its output.
fn cache_key(
    script: &Script,
    kotlin: &str,
    jdk: &JdkRequest,
    repositories: &[Repository],
    frameworks: &[&str],
    target: ScriptTarget,
    konan_target: Option<&str>,
) -> String {
    let mut fp = Fingerprint::new();
    fp.str(FORMAT).str(&script.name).str(if script.is_script { "kts" } else { "kt" }).str(&script.code);
    fp.str(kotlin).str(&jdk.to_string());
    for repo in repositories {
        fp.str(&repo.url);
    }
    for dep in &script.settings.dependencies {
        fp.str(&dep.to_string());
    }
    for framework in frameworks {
        fp.str(framework);
    }
    fp.str(&target.to_string());
    if let Some(konan) = konan_target {
        fp.str(konan);
    }
    fp.finish()
}

/// The class the scripting compiler gives a script: its file name without `.kts`, with the first
/// letter capitalised and every character that cannot be in a class name replaced by `_`.
pub fn script_class(name: &str) -> String {
    let stem = name.strip_suffix(".kts").unwrap_or(name);
    let mut class: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if let Some(first) = class.chars().next() {
        let upper: String = first.to_uppercase().collect();
        class.replace_range(..first.len_utf8(), &upper);
    }
    class
}

/// A finished cache entry.
struct Cached {
    classes: PathBuf,
    classpath: Vec<PathBuf>,
    main_class: String,
}

impl Cached {
    /// The entry at `dir`, if a build finished there and its files are still present.
    fn read(dir: &Path) -> Option<Self> {
        if !dir.join("complete").is_file() {
            return None;
        }
        let main_class = std::fs::read_to_string(dir.join("main-class")).ok()?.trim().to_owned();
        let classpath: Vec<PathBuf> =
            std::fs::read_to_string(dir.join("classpath")).ok()?.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        let classes = dir.join("classes");
        if !classes.is_dir() || classpath.iter().any(|p| !p.is_file()) {
            return None;
        }
        Some(Self { classes, classpath, main_class })
    }
}

/// What a build produced.
struct Built {
    main_class: String,
    classpath: Vec<PathBuf>,
    /// The exit code of the script when it already ran during compilation (`.kts`), else `None`.
    ran: Option<i32>,
}

/// Compiles the script into `entry`, running it too when it is a `.kts` script.
#[allow(clippy::too_many_arguments)]
fn build(
    config: &Config,
    script: &Script,
    kotlin: &str,
    jdk: &Jdk,
    repositories: &[Repository],
    frameworks: &[&str],
    entry: &Path,
    args: &[String],
    around: &Surroundings,
) -> Result<Built> {
    let shell = config.shell();
    let client = MavenClient::new(config, repositories.to_vec());
    let manifest = framework_manifest(kotlin, frameworks)?;
    let contributions = builtins::contributions(&manifest, TargetKind::Jvm, entry)?;

    let mut roots: Vec<DependencyRequest> = script
        .settings
        .dependencies
        .iter()
        .map(|d| DependencyRequest::new(ModuleId::new(&d.group, &d.artifact), Some(d.version.clone())))
        .collect();
    roots.extend(contributions.dependencies.iter().map(to_request));
    if script.is_script {
        roots.extend(SCRIPTING_RUNTIME.iter().map(|a| DependencyRequest::new(ModuleId::new(KOTLIN_GROUP, *a), Some(kotlin.to_owned()))));
    }
    let classpath = Resolver::new(&client, Platform::Jvm)
        .resolve(&roots)
        .with_context(|| format!("failed to resolve the dependencies of `{}`", script.name))?
        .files();

    let compiler = KotlinCompiler::resolve(&client, kotlin)?;
    let mut compiler_classpath = compiler.classpath.clone();
    if script.is_script {
        let scripting =
            DependencyRequest::new(ModuleId::new(KOTLIN_GROUP, "kotlin-scripting-compiler-embeddable"), Some(kotlin.to_owned()));
        compiler_classpath.extend(Resolver::new(&client, Platform::Jvm).resolve(&[scripting])?.files());
    }

    let mut plugins = Vec::new();
    for p in &contributions.plugins {
        let version = p.version.as_deref().unwrap_or(kotlin);
        let jar = fetch_jar(&client, &p.group, &p.artifact, version)
            .with_context(|| format!("failed to fetch compiler plugin {}:{}:{version}", p.group, p.artifact))?;
        plugins.push(ResolvedPlugin { jar, options: builtins::plugin_option_args(p) });
    }

    // A `.kt` file's entry point is found first, so a file without one fails before compiling.
    let main_class = match &script.file {
        Some(file) => find_entry_point_in(file, false)?.jvm_class,
        None => script_class(&script.name),
    };

    std::fs::create_dir_all(entry).with_context(|| format!("failed to create {}", entry.display()))?;
    let staging = entry.join(format!("staging-{}", std::process::id()));
    remove_dir_all_if_exists(&staging)?;
    std::fs::create_dir_all(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    let source = match &script.file {
        Some(file) => file.clone(),
        None => {
            let src = entry.join("src");
            std::fs::create_dir_all(&src)?;
            let path = src.join(&script.name);
            // The compiler waits for threads the script leaves running when it returns normally, so
            // the copy ends by exiting, as an exception already does. Line numbers are unchanged.
            write_string_atomic(&path, &format!("{}\nkotlin.system.exitProcess(0)\n", script.code))?;
            path
        }
    };

    let mut options = kotlinc_options(&staging, &classpath, jdk);
    options.extend(plugin_args(&plugins));
    options.extend(contributions.compiler_args.iter().cloned());
    // Script arguments follow `--`, so they are never read as compiler options.
    let mut trailing = Vec::new();
    if script.is_script {
        options.extend(["-script-templates".to_owned(), SCRIPT_TEMPLATE.to_owned(), "-script".to_owned(), slash(&source)]);
        trailing.push("--".to_owned());
        trailing.extend(args.iter().cloned());
    } else {
        options.push(slash(&source));
    }
    let status = kotlinc(shell, jdk, &compiler_classpath, &entry.join("kotlinc.args"), &options, &trailing, around)?;

    // The compiler writes classes only when the code compiles, so their presence means a
    // successful build even when the script then exited with a failure.
    if !staging.join(format!("{main_class}.class")).is_file() {
        remove_dir_all_if_exists(&staging)?;
        bail!("could not compile `{}` due to previous errors", script.name);
    }
    let ran = script.is_script.then(|| match status.code() {
        Some(SCRIPT_THREW) | None => 1,
        Some(code) => code,
    });

    let main_class = if script.is_script {
        // The scripting compiler's own `main` needs metadata it keeps only in memory, so the
        // cached classes are started through a launcher that calls the script's constructor,
        // which runs its top-level code. Like the `kotlin` runner, the launcher exits when the script
        // returns, even if the script left threads running, and a failure inside it exits with 1.
        let launcher = entry.join("src").join(LAUNCHER_SOURCE);
        let source = format!(
            "import kotlin.system.exitProcess\n\nfun main(args: Array<String>) {{\n    val code = try {{\n        Class.forName(\"{main_class}\").getConstructor(Array<String>::class.java).newInstance(args)\n        0\n    }} catch (e: java.lang.reflect.InvocationTargetException) {{\n        e.targetException.printStackTrace()\n        1\n    }}\n    System.out.flush()\n    exitProcess(code)\n}}\n"
        );
        write_string_atomic(&launcher, &source)?;
        let mut launch_classpath = vec![staging.clone()];
        launch_classpath.extend(classpath.iter().cloned());
        let mut options = kotlinc_options(&staging, &launch_classpath, jdk);
        options.push(slash(&launcher));
        let status = kotlinc(shell, jdk, &compiler_classpath, &entry.join("launcher.args"), &options, &[], around)?;
        if !status.success() || !staging.join(format!("{LAUNCHER_CLASS}.class")).is_file() {
            remove_dir_all_if_exists(&staging)?;
            bail!("could not build the launcher of `{}`", script.name);
        }
        LAUNCHER_CLASS.to_owned()
    } else {
        main_class
    };

    // The new classes replace any older ones, and the entry is marked complete last, so an
    // interrupted build is never read back.
    let classes = entry.join("classes");
    remove_dir_all_if_exists(&classes)?;
    std::fs::rename(&staging, &classes).with_context(|| format!("failed to move the build to {}", classes.display()))?;
    let classpath_text: String = classpath.iter().map(|p| format!("{}\n", p.display())).collect();
    write_string_atomic(&entry.join("classpath"), &classpath_text)?;
    write_string_atomic(&entry.join("main-class"), &main_class)?;
    write_string_atomic(&entry.join("complete"), FORMAT)?;

    Ok(Built { main_class, classpath, ran })
}

/// The arguments every compile of the script's classes shares: the output, the class path and the JDK.
fn kotlinc_options(output: &Path, classpath: &[PathBuf], jdk: &Jdk) -> Vec<String> {
    vec![
        "-d".to_owned(),
        slash(output),
        "-no-stdlib".to_owned(),
        "-no-reflect".to_owned(),
        "-jvm-target".to_owned(),
        jvm_target(jdk.version),
        "-jdk-home".to_owned(),
        slash(&jdk.home),
        "-cp".to_owned(),
        join_paths(classpath),
    ]
}

/// Runs the Kotlin compiler on `options`, passed through an argument file, then `trailing`.
fn kotlinc(
    shell: &Shell,
    jdk: &Jdk,
    compiler_classpath: &[PathBuf],
    argfile: &Path,
    options: &[String],
    trailing: &[String],
    around: &Surroundings,
) -> Result<ExitStatus> {
    write_argfile(argfile, options)?;
    let mut cmd = Command::new(jdk.java());
    cmd.args(compiler_jvm_options(jdk)).arg("-cp").arg(join_paths(compiler_classpath)).arg(JVM_COMPILER_MAIN);
    cmd.arg(format!("@{}", argfile.display())).args(trailing);
    around.apply(&mut cmd);
    process::status(shell, &mut cmd, "the Kotlin compiler")
}

/// A manifest enabling the frameworks, so `builtins::contributions` can describe them.
fn framework_manifest(kotlin: &str, frameworks: &[&str]) -> Result<Manifest> {
    let mut text = format!("[package]\nname = \"script\"\nversion = \"0.0.0\"\n\n[kotlin]\nversion = \"{kotlin}\"\n\n[frameworks]\n");
    for framework in frameworks {
        text.push_str(&format!("{framework} = true\n"));
    }
    Manifest::parse(&text)
}

/// The `-jvm-target` for a JDK: its feature version, or 1.8 for JDK 8.
fn jvm_target(version: u32) -> String {
    if version >= 9 { version.to_string() } else { "1.8".to_owned() }
}

/// Runs a built program's main class, with the build's classes first on the class path.
fn launch(
    config: &Config,
    jdk: &Jdk,
    classes: &Path,
    classpath: &[PathBuf],
    main: &str,
    args: &[String],
    around: &Surroundings,
) -> Result<i32> {
    let mut path = vec![classes.to_path_buf()];
    path.extend(classpath.iter().cloned());
    let mut cmd = Command::new(jdk.java());
    cmd.args(jdk.default_run_args()).arg("-cp").arg(join_paths(&path)).arg(main).args(args);
    around.apply(&mut cmd);
    config.shell().verbose("Running", format!("`{main}`"));
    let status = cmd.status().with_context(|| format!("failed to start {}", jdk.java().display()))?;
    Ok(kargo_util::process::exit_code_of(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_script_class_like_the_compiler() {
        assert_eq!(script_class("hello.kts"), "Hello");
        assert_eq!(script_class("my-tool.kts"), "My_tool");
        assert_eq!(script_class("expression.kts"), "Expression");
    }

    #[test]
    fn rejects_unknown_frameworks_and_keeps_order() {
        let settings = Settings { frameworks: vec!["ktor".into(), "serialization".into(), "ktor".into()], ..Settings::default() };
        assert_eq!(frameworks(&settings).unwrap(), ["ktor", "serialization"]);
        let settings = Settings { frameworks: vec!["flask".into()], ..Settings::default() };
        assert!(format!("{:#}", frameworks(&settings).unwrap_err()).contains("unknown framework `flask`"));
    }

    #[test]
    fn keys_depend_on_the_code_and_the_settings() {
        let script =
            |code: &str| Script { name: "a.kts".into(), is_script: true, code: code.into(), file: None, settings: Settings::default() };
        let repos = [Repository::maven_central()];
        let key = |s: &Script, kotlin: &str| cache_key(s, kotlin, &JdkRequest::System, &repos, &[], ScriptTarget::Jvm, None);
        assert_eq!(key(&script("1"), "2.4.21"), key(&script("1"), "2.4.21"));
        assert_ne!(key(&script("1"), "2.4.21"), key(&script("2"), "2.4.21"));
        assert_ne!(key(&script("1"), "2.4.21"), key(&script("1"), "2.3.0"));
    }

    #[test]
    fn keys_separate_jvm_and_native_and_hosts() {
        let script =
            |code: &str| Script { name: "a.kt".into(), is_script: false, code: code.into(), file: None, settings: Settings::default() };
        let repos = [Repository::maven_central()];
        let jvm = cache_key(&script("1"), "2.4.21", &JdkRequest::System, &repos, &[], ScriptTarget::Jvm, None);
        let native = cache_key(&script("1"), "2.4.21", &JdkRequest::System, &repos, &[], ScriptTarget::Native, Some("linux_x64"));
        let other_host = cache_key(&script("1"), "2.4.21", &JdkRequest::System, &repos, &[], ScriptTarget::Native, Some("macos_arm64"));
        assert_ne!(jvm, native);
        assert_ne!(native, other_host);
    }

    #[test]
    fn native_cache_reads_only_finished_binaries() {
        let dir = tempfile::tempdir().unwrap();
        assert!(NativeCached::read(dir.path(), "linux_x64").is_none());
        std::fs::write(dir.path().join("complete"), FORMAT).unwrap();
        assert!(NativeCached::read(dir.path(), "linux_x64").is_none(), "no binary yet");
        let binary = dir.path().join("script.kexe");
        std::fs::write(&binary, "fake").unwrap();
        assert_eq!(NativeCached::read(dir.path(), "linux_x64").unwrap().binary, binary);
        // Windows binaries have another extension.
        assert!(NativeCached::read(dir.path(), "mingw_x64").is_none());
        let win = tempfile::tempdir().unwrap();
        std::fs::write(win.path().join("complete"), FORMAT).unwrap();
        std::fs::write(win.path().join("script.exe"), "fake").unwrap();
        assert!(NativeCached::read(win.path(), "mingw_x64").is_some());
        assert!(NativeCached::read(win.path(), "linux_x64").is_none());
    }

    #[test]
    fn native_needs_a_kt_file() {
        let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let script = Script { name: "a.kts".into(), is_script: true, code: "println(1)".into(), file: None, settings: Settings::default() };
        let err = format!("{:#}", run_native(&config, &script, &[], &Surroundings::default()).unwrap_err());
        assert!(err.contains("--target native") && err.contains(".kt file"), "{err}");
    }

    #[test]
    fn reports_header_problems_without_compiling() {
        let found = problems("@file:DependsOn(\"a:b\")\n@file:Framework(\"serialization\")\n@file:Kotlin(\"2.3.0\")\nprintln(1)\n");
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].range, 0..22);
        assert!(found[0].message.contains("version pinned"), "{}", found[0].message);
        assert!(found[1].message.contains("needs the Kotlin compiler plugin"), "{}", found[1].message);
        assert!(problems("@file:Kotlin(\"2.3.0\")\n@file:Framework(\"coroutines\")\nprintln(1)\n").is_empty());
        // Only the header counts: an annotation after the first statement is not a script annotation.
        assert!(problems("println(1)\n@file:Jdk(\"x\n").is_empty());
        let bad = problems("\n@file:Jdk(\"x\nprintln(1)\n");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].message.starts_with("line 2:"), "{}", bad[0].message);
        assert_eq!(bad[0].range, 1..13);
        let native = problems("@file:Target(\"native\")\nprintln(1)\n");
        assert_eq!(native.len(), 1, "{native:?}");
        assert!(native[0].message.contains("needs a .kt file"), "{}", native[0].message);
        assert!(problems("@file:Target(\"jvm\")\nprintln(1)\n").is_empty());
        let bad_target = problems("@file:Target(\"wasm\")\nprintln(1)\n");
        assert_eq!(bad_target.len(), 1, "{bad_target:?}");
        assert!(bad_target[0].message.contains("invalid target"), "{}", bad_target[0].message);
    }

    #[test]
    fn declares_repositories_after_maven_central() {
        let repos = declared_repositories("@file:Repository(\"https://jitpack.io/\")\n@file:Repository(\"mavenCentral\")\n");
        let urls: Vec<&str> = repos.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, [kargo_maven::repo::MAVEN_CENTRAL, "https://jitpack.io"]);
    }

    #[test]
    fn declares_dependencies_for_the_language_server() {
        let text = "@file:DependsOn(\"g:a:1\")
@file:Repository(\"https://jitpack.io\")
@file:Repository(\"mavenCentral\")
println(1)
";
        assert_eq!(declared_dependencies(text), ["g:a:1", "repository https://jitpack.io"]);
        assert!(
            declared_dependencies(
                "println(1)
"
            )
            .is_empty()
        );
        assert!(
            declared_dependencies(
                "@file:DependsOn(\"g:a\")
"
            )
            .is_empty(),
            "a script that does not parse declares none"
        );
        // A script without dependencies resolves to an empty list, without the network.
        let config = Config::new(std::env::temp_dir(), kargo_util::shell::Shell::new(kargo_util::shell::Verbosity::Quiet));
        let meta = dependency_metadata(
            &config,
            "println(1)
",
            false,
        )
        .unwrap();
        assert_eq!(meta["packages"][0]["targets"][0]["dependencies"][0]["compile"], json!([]));
    }

    #[test]
    fn maps_jvm_targets() {
        assert_eq!(jvm_target(8), "1.8");
        assert_eq!(jvm_target(21), "21");
    }
}
