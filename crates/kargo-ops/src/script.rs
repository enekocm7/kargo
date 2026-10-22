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

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use kargo_compiler::mainclass::find_entry_point_in;
use kargo_compiler::{ResolvedPlugin, compiler_jvm_options, plugin_args};
use kargo_manifest::script::{self, Settings};
use kargo_manifest::{DEFAULT_KOTLIN_VERSION, JdkRequest, Manifest, TargetKind};
use kargo_maven::{DependencyRequest, MavenClient, ModuleId, Platform, Repository, Resolver};
use kargo_toolchain::kotlin::{JVM_COMPILER_MAIN, KOTLIN_GROUP, KotlinCompiler, check_compiler_jdk, fetch_jar};
use kargo_toolchain::{Jdk, jdks};
use kargo_util::config::Config;
use kargo_util::fs::{join_paths, remove_dir_all_if_exists, write_string_atomic};
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash, write_argfile};
use kargo_util::shell::Shell;

use crate::builtins::{self, FRAMEWORK_NAMES};
use crate::ops::context::{MIN_JDK, to_request};

/// Written into every cache key; bump it when what an entry holds changes.
const FORMAT: &str = "3";
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

/// Runs the script with `args` and returns its exit code.
pub fn run(config: &Config, source: &Source, args: &[String]) -> Result<i32> {
    let shell = config.shell();
    let script = load(source)?;
    let kotlin = kotlin_version(&script.settings)?;
    let frameworks = frameworks(&script.settings)?;
    if script.is_script {
        check_script_frameworks(&script.name, &kotlin, &frameworks)?;
    }
    let jdk_request = script.settings.jdk.clone().unwrap_or_default();
    let repositories = repositories(&script.settings);
    let entry = config.scripts_cache().join(cache_key(&script, &kotlin, &jdk_request, &repositories, &frameworks));

    let needs = jdks::JdkNeeds { max: kargo_toolchain::kotlin::max_compiler_jdk(&kotlin), ..Default::default() };
    let jdk = jdks::resolve_for(config, &jdk_request, needs)?;
    if let Some(cached) = Cached::read(&entry) {
        shell.verbose("Cached", format!("`{}`", script.name));
        return launch(config, &jdk, &cached.classes, &cached.classpath, &cached.main_class, args);
    }
    jdk.require_version(MIN_JDK)?;
    check_compiler_jdk(&kotlin, &jdk)?;

    shell.status("Compiling", format!("`{}`", script.name));
    let built = build(config, &script, &kotlin, &jdk, &repositories, &frameworks, &entry, args)?;
    match built.ran {
        Some(code) => Ok(code),
        None => launch(config, &jdk, &entry.join("classes"), &built.classpath, &built.main_class, args),
    }
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
            bail!("unknown framework `{name}`; expected one of {}", FRAMEWORK_NAMES.join(", "));
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
/// compiling it: a malformed annotation, an unknown framework or one that needs a compiler plugin.
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
    let settings =
        script::parse_kts(text).and_then(|parsed| script::settings_from_annotations(text, &parsed.annotations)).unwrap_or_default();
    repositories(&settings)
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
fn cache_key(script: &Script, kotlin: &str, jdk: &JdkRequest, repositories: &[Repository], frameworks: &[&str]) -> String {
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
    let status = kotlinc(shell, jdk, &compiler_classpath, &entry.join("kotlinc.args"), &options, &trailing)?;

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
        let status = kotlinc(shell, jdk, &compiler_classpath, &entry.join("launcher.args"), &options, &[])?;
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
) -> Result<ExitStatus> {
    write_argfile(argfile, options)?;
    let mut cmd = Command::new(jdk.java());
    cmd.args(compiler_jvm_options(jdk)).arg("-cp").arg(join_paths(compiler_classpath)).arg(JVM_COMPILER_MAIN);
    cmd.arg(format!("@{}", argfile.display())).args(trailing);
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
fn launch(config: &Config, jdk: &Jdk, classes: &Path, classpath: &[PathBuf], main: &str, args: &[String]) -> Result<i32> {
    let mut path = vec![classes.to_path_buf()];
    path.extend(classpath.iter().cloned());
    let mut cmd = Command::new(jdk.java());
    cmd.args(jdk.default_run_args()).arg("-cp").arg(join_paths(&path)).arg(main).args(args);
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
        let key = |s: &Script, kotlin: &str| cache_key(s, kotlin, &JdkRequest::System, &repos, &[]);
        assert_eq!(key(&script("1"), "2.4.21"), key(&script("1"), "2.4.21"));
        assert_ne!(key(&script("1"), "2.4.21"), key(&script("2"), "2.4.21"));
        assert_ne!(key(&script("1"), "2.4.21"), key(&script("1"), "2.3.0"));
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
    }

    #[test]
    fn declares_repositories_after_maven_central() {
        let repos = declared_repositories("@file:Repository(\"https://jitpack.io/\")\n@file:Repository(\"mavenCentral\")\n");
        let urls: Vec<&str> = repos.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, [kargo_maven::repo::MAVEN_CENTRAL, "https://jitpack.io"]);
    }

    #[test]
    fn maps_jvm_targets() {
        assert_eq!(jvm_target(8), "1.8");
        assert_eq!(jvm_target(21), "21");
    }
}
