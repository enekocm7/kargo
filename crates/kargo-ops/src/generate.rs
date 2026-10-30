//! `[[generate]]`: steps that write sources or resources before the compilers run.
//!
//! A step writes into `target/generated/<profile>/<name>/` (`KARGO_GEN_DIR`), and that directory
//! joins the sources, the test sources or the resources of the targets the step names. A step runs
//! a shell command (`run`), the main class of a Maven artifact (`tool`) or one of Kargo's own
//! generators (`builtin`: `build-config` and `protobuf`).
//!
//! A step with `inputs` runs only when they, its definition or Kargo changed since it last
//! succeeded. Each run starts in an empty directory, and afterwards a file that has the same bytes
//! as before keeps its old file, so that an unchanged output does not make the compilers do
//! their work again, and a file the step no longer writes is gone.
//!
//! [`ensure`] runs the steps a target needs once per invocation; [`sources`] only reads what they
//! wrote, so that `kargo metadata` never starts a generator.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::env::{EnvOptions, build_config_package, kotlin_string};
use crate::ops::context::BuildContext;
use crate::ops::new::RESERVED_WORDS;
use crate::tasks;
use crate::workspace::{ProfileKind, Workspace};
use kargo_manifest::{
    BuildConfigGenerator, BuildConfigValue, GeneratedKind, Generator, GeneratorAction, Manifest, ProtobufGenerator, TargetKind,
};
use kargo_maven::{DependencyRequest, ModuleId, Platform};
use kargo_util::fs::{collect_files, remove_dir_all_if_exists, rename_with_retries, write_string_atomic};
use kargo_util::hash::{Fingerprint, sha256_file};
use kargo_util::process;

/// The `protoc` version `builtin = "protobuf"` downloads, matching `protobuf-java` and `protobuf-kotlin` of the same number.
pub const PROTOC_VERSION: &str = "4.31.1";
/// The version of `protoc-gen-grpc-java`, which `grpc = true` downloads.
pub const GRPC_JAVA_VERSION: &str = "1.73.0";
/// The version of `protoc-gen-grpc-kotlin`, which `grpc = true` with `kotlin = true` downloads.
pub const GRPC_KOTLIN_VERSION: &str = "1.4.3";

/// What `kargo generate` and the build run.
#[derive(Debug, Clone, Default)]
pub struct GenerateOptions {
    /// The steps to run, with the ones they wait for; empty means every step the targets use.
    pub names: Vec<String>,
    /// The targets whose steps run; empty means every target.
    pub targets: Vec<TargetKind>,
    /// Run steps that are up to date too (`--force`).
    pub force: bool,
}

/// What happened to one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub name: String,
    /// Whether the step ran; `false` when its output was up to date.
    pub ran: bool,
}

/// The directory of one profile's generated files: `target/generated/debug`.
pub fn root_dir(ws: &Workspace, profile: &ProfileKind) -> PathBuf {
    ws.generated_dir(profile)
}

/// `KARGO_GEN_DIR` of a step.
pub fn output_dir(ws: &Workspace, profile: &ProfileKind, name: &str) -> PathBuf {
    root_dir(ws, profile).join(name)
}

fn stamp_path(ws: &Workspace, profile: &ProfileKind, name: &str) -> PathBuf {
    root_dir(ws, profile).join(".stamps").join(name)
}

/// The output directories of the steps of `kind` that `target` uses, in declaration order.
pub fn dirs(ws: &Workspace, profile: &ProfileKind, target: TargetKind, kind: GeneratedKind) -> Vec<PathBuf> {
    ws.manifest().generators.iter().filter(|g| g.kind == kind && g.applies_to(target)).flat_map(|g| roots(ws, profile, g)).collect()
}

/// The directories of a step's output that are source or resource roots: its `roots`, else the output directory.
pub fn roots(ws: &Workspace, profile: &ProfileKind, g: &Generator) -> Vec<PathBuf> {
    let dir = output_dir(ws, profile, &g.name);
    if g.roots.is_empty() {
        return vec![dir];
    }
    g.roots.iter().map(|root| root.split('/').fold(dir.clone(), |path, part| path.join(part))).collect()
}

/// The generated source files `target` compiles: main ones, or with `tests` the test ones. They
/// are whatever the steps wrote last; [`ensure`] brings them up to date first.
pub fn sources(ws: &Workspace, profile: &ProfileKind, target: TargetKind, tests: bool, extensions: &[&str]) -> Vec<PathBuf> {
    let kind = if tests { GeneratedKind::TestSources } else { GeneratedKind::Sources };
    dirs(ws, profile, target, kind).iter().flat_map(|dir| collect_files(dir, extensions)).collect()
}

/// The generated resource directories of `target` that exist.
pub fn resource_dirs(ws: &Workspace, profile: &ProfileKind, target: TargetKind) -> Vec<PathBuf> {
    dirs(ws, profile, target, GeneratedKind::Resources).into_iter().filter(|d| d.is_dir()).collect()
}

/// The step whose `BuildConfig` object also holds the `[env] expose` constants of `target`: the first
/// `builtin = "build-config"` step that applies to it. `[env] expose` writes none of its own then.
pub fn build_config_owner(manifest: &Manifest, target: TargetKind) -> Option<&Generator> {
    manifest.generators.iter().find(|g| matches!(g.action, GeneratorAction::BuildConfig(_)) && g.applies_to(target))
}

/// Runs the steps `target` uses, unless this invocation did already. A path dependency's context
/// shares the guard, so each step of each package runs once however many targets and packages need it.
pub fn ensure(ctx: &BuildContext, target: TargetKind) -> Result<()> {
    ensure_all(ctx, &[target])
}

/// [`ensure`] for several targets at once, so that steps for different targets run side by side.
pub fn ensure_all(ctx: &BuildContext, targets: &[TargetKind]) -> Result<()> {
    let manifest = ctx.ws.manifest();
    if !manifest.generators.iter().any(|g| targets.iter().any(|t| g.applies_to(*t))) {
        return Ok(());
    }
    for level in plan(manifest, &[], targets)? {
        let results = crate::ops::parallel_jobs(&level, |g| {
            let key = format!("{}/generate/{}/{}", ctx.ws.name(), ctx.profile.name(), g.name);
            ctx.built_artifact(key, || run_step(ctx, g, false).map(|_| None)).map(|_| ())
        });
        for result in results {
            result?;
        }
    }
    Ok(())
}

/// `kargo generate`: runs the selected steps and reports what each did.
pub fn generate(ctx: &BuildContext, opts: &GenerateOptions) -> Result<Vec<StepReport>> {
    let manifest = ctx.ws.manifest();
    if manifest.generators.is_empty() {
        bail!("kargo.toml declares no [[generate]] steps; add one to generate sources before they are compiled");
    }
    let levels = plan(manifest, &opts.names, &opts.targets)?;
    let mut reports = Vec::new();
    for level in levels {
        let results =
            crate::ops::parallel_jobs(&level, |g| run_step(ctx, g, opts.force).map(|ran| StepReport { name: g.name.clone(), ran }));
        for result in results {
            reports.push(result?);
        }
    }
    Ok(reports)
}

/// The steps to run, grouped so that every step comes after the ones it waits for and the steps of a
/// group are independent. With `names`, those steps and the ones they wait for; otherwise the steps
/// that apply to one of `targets` (any target when empty), and the ones they wait for.
fn plan<'a>(manifest: &'a Manifest, names: &[String], targets: &[TargetKind]) -> Result<Vec<Vec<&'a Generator>>> {
    let mut wanted: BTreeSet<&str> = BTreeSet::new();
    if names.is_empty() {
        for g in &manifest.generators {
            if targets.is_empty() || targets.iter().any(|t| g.applies_to(*t)) {
                wanted.insert(&g.name);
            }
        }
    } else {
        for name in names {
            let Some(g) = manifest.generator(name) else {
                let known: Vec<&str> = manifest.generators.iter().map(|g| g.name.as_str()).collect();
                bail!("no [[generate]] step named `{name}`; the steps are: {}", known.join(", "));
            };
            if !targets.is_empty() && !targets.iter().any(|t| g.applies_to(*t)) {
                let used: Vec<&str> = g.targets.iter().map(|t| t.as_str()).collect();
                bail!(
                    "the [[generate]] step `{name}` is for the {} target{} only",
                    used.join(", "),
                    if used.len() == 1 { "" } else { "s" }
                );
            }
            wanted.insert(&g.name);
        }
    }
    // Whatever a wanted step waits for runs too, however it is scoped.
    let mut queue: Vec<&str> = wanted.iter().copied().collect();
    while let Some(name) = queue.pop() {
        if let Some(g) = manifest.generator(name) {
            for dep in &g.after {
                if wanted.insert(dep) {
                    queue.push(dep);
                }
            }
        }
    }
    let mut pending: Vec<&Generator> = manifest.generators.iter().filter(|g| wanted.contains(g.name.as_str())).collect();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut levels = Vec::new();
    while !pending.is_empty() {
        let (ready, rest): (Vec<&Generator>, Vec<&Generator>) =
            pending.into_iter().partition(|g| g.after.iter().all(|d| done.contains(d.as_str())));
        // The manifest rejects cycles, so some step is always ready.
        assert!(!ready.is_empty(), "[[generate]] steps wait for each other");
        done.extend(ready.iter().map(|g| g.name.as_str()));
        levels.push(ready);
        pending = rest;
    }
    Ok(levels)
}

/// Runs one step if it is out of date (or `force`). Returns whether it ran.
fn run_step(ctx: &BuildContext, g: &Generator, force: bool) -> Result<bool> {
    let ws = ctx.ws;
    let dir = output_dir(ws, &ctx.profile, &g.name);
    let stamp = stamp_path(ws, &ctx.profile, &g.name);
    let files = input_files(ws, &g.inputs)?;
    let wanted = fingerprint(ctx, g, &files)?;
    // A step without inputs cannot tell what changed, so it always runs.
    let fresh = !force && !g.inputs.is_empty() && dir.is_dir() && std::fs::read_to_string(&stamp).is_ok_and(|s| s == wanted);
    if fresh {
        ctx.shell().verbose("Fresh", format!("generated files of `{}`", g.name));
        return Ok(false);
    }
    if !g.inputs.is_empty() && files.is_empty() {
        ctx.shell().warn(format!("no file matches the `inputs` of [[generate]] `{}`: {}", g.name, g.inputs.join(", ")));
    }
    if matches!(g.action, GeneratorAction::BuildConfig(_)) {
        ctx.shell().verbose("Generating", format!("`{}`", g.name));
    } else {
        ctx.shell().status("Generating", format!("`{}`{}", g.name, describe(g)));
    }
    let old = root_dir(ws, &ctx.profile).join(".previous").join(&g.name);
    let _ = std::fs::remove_file(&stamp);
    begin(&dir, &old)?;
    match produce(ctx, g, &dir, &files) {
        Ok(()) => finish(&dir, &old)?,
        Err(e) => {
            // Keep what the last good run wrote, so the next build can still tell what changed.
            let _ = rollback(&dir, &old);
            return Err(e.context(format!("generator `{}` failed", g.name)));
        }
    }
    if !g.inputs.is_empty() {
        write_string_atomic(&stamp, &wanted)?;
    }
    Ok(true)
}

fn describe(g: &Generator) -> String {
    match &g.action {
        GeneratorAction::Run { .. } => String::new(),
        GeneratorAction::Tool { module, version, .. } => format!(" with {module}:{version}"),
        GeneratorAction::BuildConfig(_) => " as build-config".to_owned(),
        GeneratorAction::Protobuf(_) => " with protoc".to_owned(),
    }
}

/// Moves the output of the last run to `old` and makes `dir` an empty directory for the next one.
fn begin(dir: &Path, old: &Path) -> Result<()> {
    remove_dir_all_if_exists(old)?;
    if dir.exists() {
        if let Some(parent) = old.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        rename_with_retries(dir, old).with_context(|| format!("failed to move {} aside", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))
}

/// Puts back, over each file `dir` has now, the file of the last run when the bytes are the same,
/// which keeps its modification time, and drops the rest of the last run's output.
fn finish(dir: &Path, old: &Path) -> Result<()> {
    if old.is_dir() {
        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            let Ok(relative) = entry.path().strip_prefix(dir) else { continue };
            let before = old.join(relative);
            if before.is_file() && same_bytes(&before, entry.path()) {
                std::fs::remove_file(entry.path()).with_context(|| format!("failed to replace {}", entry.path().display()))?;
                rename_with_retries(&before, entry.path()).with_context(|| format!("failed to restore {}", entry.path().display()))?;
            }
        }
    }
    remove_dir_all_if_exists(old)
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    ma.len() == mb.len() && matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

/// Brings back the output of the last run after a failure.
fn rollback(dir: &Path, old: &Path) -> Result<()> {
    remove_dir_all_if_exists(dir)?;
    if old.is_dir() {
        rename_with_retries(old, dir).with_context(|| format!("failed to restore {}", dir.display()))?;
    }
    Ok(())
}

/// Hashes everything the output depends on: the step, the project it is part of, Kargo's version
/// and the contents of the input files.
fn fingerprint(ctx: &BuildContext, g: &Generator, files: &[PathBuf]) -> Result<String> {
    let manifest = ctx.ws.manifest();
    let mut fp = Fingerprint::new();
    fp.str(env!("CARGO_PKG_VERSION"));
    fp.str(ctx.profile.name());
    fp.str(&manifest.package.name);
    fp.str(&manifest.package.version);
    fp.str(manifest.package.group.as_deref().unwrap_or_default());
    fp.str(&format!("{g:?}"));
    for file in files {
        fp.str(&process::slash(file.strip_prefix(ctx.ws.root()).unwrap_or(file)));
        fp.str(&sha256_file(file).with_context(|| format!("failed to read {}", file.display()))?);
    }
    Ok(fp.finish())
}

/// The files `inputs` match, sorted. A glob is gitignore syntax relative to the project root, and a
/// directory stands for the files under it. Hidden directories and `target/` are never searched.
fn input_files(ws: &Workspace, inputs: &[String]) -> Result<Vec<PathBuf>> {
    let root = ws.root();
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in inputs {
        let entry = entry.replace('\\', "/");
        let entry = entry.trim_start_matches("./");
        let mut matcher = GitignoreBuilder::new(root);
        matcher
            .add_line(None, &format!("/{}", entry.trim_start_matches('/')))
            .with_context(|| format!("invalid glob `{entry}` in `inputs`"))?;
        let matcher: Gitignore = matcher.build().with_context(|| format!("invalid glob `{entry}` in `inputs`"))?;
        // Search from the directory before the first wildcard.
        let literal: PathBuf =
            Path::new(entry).components().take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '['])).collect();
        let start = root.join(&literal);
        let target_dir = ws.target_dir();
        let walk = walkdir::WalkDir::new(&start)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || (!e.file_name().to_string_lossy().starts_with('.') && e.path() != target_dir));
        for found in walk.filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            if let Ok(relative) = found.path().strip_prefix(root)
                && matcher.matched_path_or_any_parents(relative, false).is_ignore()
            {
                files.insert(found.into_path());
            }
        }
    }
    Ok(files.into_iter().collect())
}

/// The environment of a step: the one hooks get, `KARGO_GEN_*`, then the step's own `env`.
fn step_env(ctx: &BuildContext, g: &Generator, dir: &Path) -> Result<Vec<(String, String)>> {
    let mut env = crate::ops::build::hook_env(ctx, &EnvOptions::default())?;
    env.push(("KARGO_GEN_DIR".to_owned(), dir.to_string_lossy().into_owned()));
    env.push(("KARGO_GEN_ROOT".to_owned(), root_dir(ctx.ws, &ctx.profile).to_string_lossy().into_owned()));
    env.push(("KARGO_GEN_NAME".to_owned(), g.name.clone()));
    env.push(("KARGO_GEN_KIND".to_owned(), g.kind.as_str().to_owned()));
    env.extend(g.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(env)
}

/// Writes the step's output into `dir`.
fn produce(ctx: &BuildContext, g: &Generator, dir: &Path, files: &[PathBuf]) -> Result<()> {
    let root = ctx.ws.root();
    let env = step_env(ctx, g, dir)?;
    match &g.action {
        GeneratorAction::Run { run, windows_run, shell } => {
            let commands = windows_run.as_ref().filter(|_| cfg!(windows)).unwrap_or(run);
            let shell = shell.unwrap_or_else(kargo_manifest::TaskShell::platform_default);
            for script in commands {
                let mut cmd = tasks::shell_command(shell, script).with_context(|| format!("cannot run generator `{}`", g.name))?;
                cmd.current_dir(root).envs(env.iter().map(|(k, v)| (k, v)));
                exec(ctx, &mut cmd, g, script)?;
            }
            Ok(())
        }
        GeneratorAction::Tool { module, version, main_class, args } => {
            let jdk = ctx.jdk()?;
            let resolution = resolve_tools(ctx, g)?.context("a tool step resolves to its tool")?;
            let class = match main_class {
                Some(class) => class.clone(),
                None => {
                    let jar = resolution.node(module).and_then(|n| n.files.first()).context("the tool resolved to no jar")?;
                    main_class_of(jar)?.with_context(|| {
                        format!("{module}:{version} names no Main-Class in its manifest; set `main-class` in [[generate]] `{}`", g.name)
                    })?
                }
            };
            let mut cmd = Command::new(jdk.java());
            cmd.arg("-cp").arg(kargo_util::fs::join_paths(&resolution.files())).arg(class);
            cmd.args(args.iter().map(|a| expand(a, &env)));
            cmd.current_dir(root).envs(env.iter().map(|(k, v)| (k, v)));
            exec(ctx, &mut cmd, g, &format!("{module}:{version}"))
        }
        GeneratorAction::BuildConfig(config) => {
            let contents = build_config_source(ctx, g, config)?;
            let path = dir.join(format!("{}.kt", config.object));
            std::fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
        }
        GeneratorAction::Protobuf(proto) => {
            let protos: Vec<&PathBuf> = files.iter().filter(|f| f.extension().is_some_and(|e| e == "proto")).collect();
            if protos.is_empty() {
                return Ok(());
            }
            let mut cmd = protobuf_command(ctx, g, proto, &protos, dir)?;
            cmd.current_dir(root).envs(env.iter().map(|(k, v)| (k, v)));
            exec(ctx, &mut cmd, g, "protoc")
        }
    }
}

/// Runs `cmd` to the end, holding back its output: it is shown with `-v`, and when the command fails.
fn exec(ctx: &BuildContext, cmd: &mut Command, g: &Generator, what: &str) -> Result<()> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(cmd)));
    let output = cmd.stdin(Stdio::null()).output().with_context(|| format!("failed to start `{what}`"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let text = text.trim();
    if !output.status.success() {
        let shown = if text.is_empty() { String::new() } else { format!("\n{text}") };
        bail!("`{what}` failed ({}){shown}", process::describe_status(output.status));
    }
    for line in text.lines() {
        ctx.shell().verbose(&g.name, line);
    }
    Ok(())
}

/// Replaces `$NAME` and `${NAME}` with the value of `NAME` in `env`; other text stays as it is.
pub fn expand(text: &str, env: &[(String, String)]) -> String {
    let lookup = |name: &str| env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, length) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], end + 2),
                None => ("", 0),
            }
        } else {
            let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
            (&after[..end], end)
        };
        match lookup(name).filter(|_| !name.is_empty()) {
            Some(value) => {
                out.push_str(value);
                rest = &after[length..];
            }
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `Main-Class` of a jar's manifest, if it has one.
fn main_class_of(jar: &Path) -> Result<Option<String>> {
    let file = std::fs::File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("{} is not a jar", jar.display()))?;
    let mut text = String::new();
    match archive.by_name("META-INF/MANIFEST.MF") {
        Ok(mut entry) => {
            use std::io::Read;
            entry.read_to_string(&mut text).with_context(|| format!("failed to read the manifest of {}", jar.display()))?;
        }
        Err(_) => return Ok(None),
    }
    Ok(manifest_main_class(&text))
}

/// The `Main-Class` attribute of manifest text; a long value continues on lines that start with a space.
fn manifest_main_class(text: &str) -> Option<String> {
    let mut value: Option<String> = None;
    for line in text.lines() {
        match (&mut value, line.strip_prefix(' ')) {
            (Some(v), Some(continuation)) => v.push_str(continuation),
            (Some(_), None) => break,
            (None, _) => {
                if let Some(v) = line.strip_prefix("Main-Class:") {
                    value = Some(v.trim().to_owned());
                }
            }
        }
    }
    value.filter(|v| !v.is_empty())
}

// ---- builtin = "build-config" ----

/// The Kotlin source of a `builtin = "build-config"` step.
fn build_config_source(ctx: &BuildContext, g: &Generator, config: &BuildConfigGenerator) -> Result<String> {
    let manifest = ctx.ws.manifest();
    let package = config.package.clone().unwrap_or_else(|| build_config_package(manifest.package.group.as_deref(), &manifest.package.name));
    let git_sha = git_sha(ctx.ws.root()).unwrap_or_else(|| "unknown".to_owned());
    let mut lines = vec![
        format!("// Generated by Kargo ([[generate]] `{}` in kargo.toml). The next build overwrites it.", g.name),
        format!("package {package}"),
        String::new(),
        format!("object {} {{", config.object),
        format!("    const val VERSION: String = {}", kotlin_string(&manifest.package.version)),
        format!("    const val GROUP: String = {}", kotlin_string(manifest.package.group.as_deref().unwrap_or_default())),
        format!("    const val NAME: String = {}", kotlin_string(&manifest.package.name)),
        format!("    const val GIT_SHA: String = {}", kotlin_string(&git_sha)),
    ];
    if config.build_time {
        lines.push(format!("    const val BUILD_TIME: String = {}", kotlin_string(&build_time())));
    }
    lines.push(format!("    const val PROFILE: String = {}", kotlin_string(ctx.profile.name())));
    for (name, value) in &config.fields {
        let (kind, literal) = match value {
            BuildConfigValue::Bool(b) => ("Boolean", b.to_string()),
            BuildConfigValue::Int(i) if i32::try_from(*i).is_ok() => ("Int", i.to_string()),
            BuildConfigValue::Int(i) => ("Long", format!("{i}L")),
            BuildConfigValue::String(s) => ("String", kotlin_string(s)),
        };
        lines.push(format!("    const val {}: {kind} = {literal}", identifier(name)));
    }
    // `[env] expose` joins the first object, as it would be a second `BuildConfig` otherwise.
    let owner = build_config_owner(manifest, TargetKind::ALL.into_iter().find(|t| g.applies_to(*t)).unwrap_or(TargetKind::Jvm));
    let mut exposed: Vec<&String> = config.expose.iter().collect();
    if owner.is_some_and(|o| o.name == g.name) {
        exposed.extend(manifest.env.expose.iter().filter(|n| !config.expose.contains(n)));
    }
    if !exposed.is_empty() {
        let env = ctx.resolve_env(&EnvOptions::default())?;
        for name in exposed {
            lines.push(match env.vars.get(name) {
                Some(var) => format!("    const val {}: String = {}", identifier(name), kotlin_string(&var.value)),
                None => format!("    val {}: String? = null", identifier(name)),
            });
        }
    }
    lines.push("}".to_owned());
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn identifier(name: &str) -> String {
    if RESERVED_WORDS.contains(&name) { format!("`{name}`") } else { name.to_owned() }
}

/// The commit `HEAD` of the repository around `root` points to; `None` without Git or a commit.
fn git_sha(root: &Path) -> Option<String> {
    let output =
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(root).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !sha.is_empty()).then_some(sha)
}

/// `SOURCE_DATE_EPOCH`, else now, as an RFC 3339 time in UTC.
fn build_time() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let seconds = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(now);
    rfc3339(seconds)
}

pub(crate) fn rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", of_day / 3600, of_day % 3600 / 60, of_day % 60)
}

// ---- builtin = "protobuf" ----

/// The platform part of the classifier of a native Maven artifact such as `protoc`.
fn native_classifier() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch_64",
        ("macos", "x86_64") => "osx-x86_64",
        ("macos", "aarch64") => "osx-aarch_64",
        (os, arch) => bail!("protoc has no download for {os} on {arch}; run it yourself with `run = \"protoc ...\"`"),
    })
}

/// What a step downloads, as the roots of its `generate/<name>` resolution: the tool of a `tool` step, `protoc` and the
/// gRPC plugins of a `protobuf` one. Other steps download nothing.
fn tool_requests(g: &Generator) -> Result<Vec<DependencyRequest>> {
    Ok(match &g.action {
        GeneratorAction::Tool { module, version, .. } => vec![DependencyRequest::new(module.clone(), Some(version.clone()))],
        GeneratorAction::Protobuf(proto) => {
            let classifier = native_classifier()?;
            let mut requests = vec![plugin_request(
                "com.google.protobuf",
                "protoc",
                proto.version.as_deref().unwrap_or(PROTOC_VERSION),
                classifier,
                "exe",
            )];
            if proto.grpc {
                let grpc_java = proto.grpc_java_version.as_deref().unwrap_or(GRPC_JAVA_VERSION);
                requests.push(plugin_request("io.grpc", "protoc-gen-grpc-java", grpc_java, classifier, "exe"));
                if proto.kotlin {
                    let grpc_kotlin = proto.grpc_kotlin_version.as_deref().unwrap_or(GRPC_KOTLIN_VERSION);
                    requests.push(plugin_request("io.grpc", "protoc-gen-grpc-kotlin", grpc_kotlin, "jdk8", "jar"));
                }
            }
            requests
        }
        _ => Vec::new(),
    })
}

/// Resolves and downloads what a step runs, and pins it in `kargo.lock` as `generate/<name>`. `None` for a step
/// that downloads nothing.
fn resolve_tools(ctx: &BuildContext, g: &Generator) -> Result<Option<kargo_maven::Resolution>> {
    let requests = tool_requests(g)?;
    if requests.is_empty() {
        return Ok(None);
    }
    let what = match &g.action {
        GeneratorAction::Tool { module, version, .. } => format!("the tool {module}:{version}"),
        _ => "protoc and its plugins".to_owned(),
    };
    ctx.resolve(&format!("generate/{}", g.name), Platform::Jvm, &requests, &[])
        .map(Some)
        .with_context(|| format!("failed to download {what} for [[generate]] `{}`", g.name))
}

/// Downloads and locks what the steps for `targets` (every step when empty) run, without running them: `kargo fetch`
/// and `kargo update` do, so that `--offline` and `--locked` builds find it.
pub fn fetch_tools(ctx: &BuildContext, targets: &[TargetKind]) -> Result<()> {
    for g in ctx.ws.manifest().generators.iter().filter(|g| targets.is_empty() || targets.iter().any(|t| g.applies_to(*t))) {
        resolve_tools(ctx, g)?;
    }
    Ok(())
}

/// A request for one native executable (`@exe`) or a plugin jar, followed no further.
fn plugin_request(group: &str, artifact: &str, version: &str, classifier: &str, extension: &str) -> DependencyRequest {
    let mut request = DependencyRequest::new(ModuleId::new(group, artifact), Some(version.to_owned()));
    request.classifier = Some(classifier.to_owned());
    request.extension = Some(extension.to_owned());
    request.transitive = false;
    request
}

/// Makes a downloaded executable runnable.
fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).with_context(|| format!("failed to read {}", path.display()))?.permissions();
        if permissions.mode() & 0o111 == 0 {
            permissions.set_mode(permissions.mode() | 0o755);
            std::fs::set_permissions(path, permissions).with_context(|| format!("failed to make {} executable", path.display()))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A script that starts a plugin jar, since `protoc` runs plugins as programs.
fn jar_launcher(dir: &Path, name: &str, java: &Path, jar: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let (path, script) = if cfg!(windows) {
        (dir.join(format!("{name}.bat")), format!("@echo off\r\n\"{}\" -jar \"{}\" %*\r\n", java.display(), jar.display()))
    } else {
        (dir.join(name), format!("#!/bin/sh\nexec '{}' -jar '{}' \"$@\"\n", java.display(), jar.display()))
    };
    write_string_atomic(&path, &script)?;
    make_executable(&path)?;
    Ok(path)
}

/// The `protoc` command line of a `builtin = "protobuf"` step.
fn protobuf_command(ctx: &BuildContext, g: &Generator, proto: &ProtobufGenerator, protos: &[&PathBuf], dir: &Path) -> Result<Command> {
    let resolution = resolve_tools(ctx, g)?.context("a protobuf step resolves to protoc")?;
    let file_of = |artifact: &str| -> Result<PathBuf> {
        resolution
            .nodes
            .iter()
            .find(|n| n.module.artifact == artifact)
            .and_then(|n| n.files.first().cloned())
            .with_context(|| format!("{artifact} resolved to no file"))
    };
    let protoc = file_of("protoc")?;
    make_executable(&protoc)?;

    let mut cmd = Command::new(&protoc);
    for path in &proto.proto_path {
        cmd.arg(format!("-I{path}"));
    }
    let out = dir.to_string_lossy().into_owned();
    if proto.java {
        cmd.arg(format!("--java_out={out}"));
    }
    if proto.kotlin {
        cmd.arg(format!("--kotlin_out={out}"));
    }
    if proto.grpc {
        let grpc_java = file_of("protoc-gen-grpc-java")?;
        make_executable(&grpc_java)?;
        cmd.arg(format!("--plugin=protoc-gen-grpc={}", grpc_java.display())).arg(format!("--grpc_out={out}"));
        if proto.kotlin {
            let jar = file_of("protoc-gen-grpc-kotlin")?;
            let tools = root_dir(ctx.ws, &ctx.profile).join(".tools");
            let launcher = jar_launcher(&tools, "protoc-gen-grpckt", &ctx.jdk()?.java(), &jar)?;
            cmd.arg(format!("--plugin=protoc-gen-grpckt={}", launcher.display())).arg(format!("--grpckt_out={out}"));
        }
    }
    cmd.args(&proto.args);
    let root = ctx.ws.root();
    cmd.args(protos.iter().map(|p| process::slash(p.strip_prefix(root).unwrap_or(p))));
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Project;
    use kargo_util::config::Config;
    use kargo_util::shell::{Shell, Verbosity};

    #[test]
    fn expands_variables_in_arguments() {
        let env = vec![("KARGO_GEN_DIR".to_owned(), "/gen".to_owned()), ("A".to_owned(), "1".to_owned()), ("A".to_owned(), "2".to_owned())];
        assert_eq!(expand("-o=$KARGO_GEN_DIR/x", &env), "-o=/gen/x");
        assert_eq!(expand("${KARGO_GEN_DIR}x $A", &env), "/genx 2", "the last definition wins");
        assert_eq!(expand("$MISSING ${MISSING} $ $$A ${", &env), "$MISSING ${MISSING} $ $2 ${");
        assert_eq!(expand("no variables", &env), "no variables");
    }

    #[test]
    fn reads_the_main_class_of_a_manifest() {
        assert_eq!(
            manifest_main_class("Manifest-Version: 1.0\r\nMain-Class: org.example.Main\r\n\r\n").as_deref(),
            Some("org.example.Main")
        );
        let wrapped = "Manifest-Version: 1.0\nMain-Class: org.openapitools.codegen.OpenAPIGenerat\n or\nCreated-By: x\n";
        assert_eq!(manifest_main_class(wrapped).as_deref(), Some("org.openapitools.codegen.OpenAPIGenerator"));
        assert_eq!(manifest_main_class("Manifest-Version: 1.0\n"), None);
    }

    #[test]
    fn formats_times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn unchanged_files_keep_their_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let (out, old) = (dir.path().join("out"), dir.path().join("prev").join("out"));
        std::fs::create_dir_all(out.join("sub")).unwrap();
        std::fs::write(out.join("same.kt"), "a").unwrap();
        std::fs::write(out.join("changed.kt"), "b").unwrap();
        std::fs::write(out.join("sub").join("gone.kt"), "c").unwrap();
        let before = std::fs::metadata(out.join("same.kt")).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));

        begin(&out, &old).unwrap();
        assert!(out.is_dir() && std::fs::read_dir(&out).unwrap().next().is_none(), "the next run starts empty");
        std::fs::write(out.join("same.kt"), "a").unwrap();
        std::fs::write(out.join("changed.kt"), "B").unwrap();
        finish(&out, &old).unwrap();

        assert_eq!(std::fs::metadata(out.join("same.kt")).unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read_to_string(out.join("changed.kt")).unwrap(), "B");
        assert!(!out.join("sub").exists(), "a file the step no longer writes is gone");
        assert!(!old.exists());
    }

    #[test]
    fn a_failed_run_restores_the_last_output() {
        let dir = tempfile::tempdir().unwrap();
        let (out, old) = (dir.path().join("out"), dir.path().join("prev").join("out"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("A.kt"), "a").unwrap();
        begin(&out, &old).unwrap();
        std::fs::write(out.join("partial.kt"), "p").unwrap();
        rollback(&out, &old).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("A.kt")).unwrap(), "a");
        assert!(!out.join("partial.kt").exists());
    }

    fn project(dir: &Path, extra: &str) -> Project {
        std::fs::write(dir.join("kargo.toml"), format!("[package]\nname = \"app\"\nversion = \"1.2.3\"\ngroup = \"com.example\"\n{extra}"))
            .unwrap();
        Project::load(&dir.join("kargo.toml")).unwrap()
    }

    #[test]
    fn plans_steps_in_levels() {
        let dir = tempfile::tempdir().unwrap();
        let project = project(
            dir.path(),
            "[[generate]]\nname = \"a\"\nrun = \"x\"\n\
             [[generate]]\nname = \"b\"\nrun = \"x\"\nafter = [\"a\"]\ntargets = [\"jvm\"]\n\
             [[generate]]\nname = \"c\"\nrun = \"x\"\nafter = [\"a\"]\ntargets = [\"wasm\"]\n\
             [[generate]]\nname = \"d\"\nrun = \"x\"\nafter = [\"b\", \"c\"]\n",
        );
        let m = project.package(0).manifest();
        let names = |levels: Vec<Vec<&Generator>>| -> Vec<Vec<String>> {
            levels.iter().map(|l| l.iter().map(|g| g.name.clone()).collect()).collect()
        };
        assert_eq!(names(plan(m, &[], &[]).unwrap()), [vec!["a"], vec!["b", "c"], vec!["d"]]);
        // Steps that wait for others bring them along, whatever their targets.
        assert_eq!(names(plan(m, &["d".to_owned()], &[]).unwrap()), [vec!["a"], vec!["b", "c"], vec!["d"]]);
        assert_eq!(names(plan(m, &["b".to_owned()], &[TargetKind::Jvm]).unwrap()), [vec!["a"], vec!["b"]]);
        let err = plan(m, &["zzz".to_owned()], &[]).unwrap_err().to_string();
        assert!(err.contains("no [[generate]] step named `zzz`") && err.contains("a, b, c, d"), "{err}");
        let err = plan(m, &["b".to_owned()], &[TargetKind::Wasm]).unwrap_err().to_string();
        assert!(err.contains("`b` is for the jvm target only"), "{err}");
    }

    #[test]
    fn input_globs_match_files_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let project = project(dir.path(), "");
        let root = dir.path();
        for file in [
            "proto/a.proto",
            "proto/sub/b.proto",
            "proto/readme.md",
            "other/c.proto",
            "api.yaml",
            "sub/api.yaml",
            "target/d.proto",
            ".hidden/e.proto",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file).unwrap();
        }
        let names = |inputs: &[&str]| -> Vec<String> {
            let inputs: Vec<String> = inputs.iter().map(|s| s.to_string()).collect();
            input_files(project.package(0), &inputs).unwrap().iter().map(|p| process::slash(p.strip_prefix(root).unwrap())).collect()
        };
        assert_eq!(names(&["proto/**/*.proto"]), ["proto/a.proto", "proto/sub/b.proto"]);
        assert_eq!(names(&["proto"]), ["proto/a.proto", "proto/readme.md", "proto/sub/b.proto"], "a directory stands for its files");
        assert_eq!(names(&["api.yaml"]), ["api.yaml"], "a name is relative to the root, not a pattern for any depth");
        assert_eq!(names(&["./api.yaml", "proto/a.proto"]), ["api.yaml", "proto/a.proto"]);
        assert_eq!(
            names(&["**/*.proto"]),
            ["other/c.proto", "proto/a.proto", "proto/sub/b.proto"],
            "target/ and hidden directories are skipped"
        );
        assert!(names(&["nothing/**"]).is_empty());
    }

    fn context<'a>(config: &'a Config, project: &'a Project) -> BuildContext<'a> {
        BuildContext::new(config, project, 0, ProfileKind::Dev).unwrap()
    }

    #[test]
    fn writes_the_build_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let project = project(
            dir.path(),
            "[env]\nAPI = { value = \"https://x.test/$a\" }\nexpose = [\"API\", \"OPTIONAL\"]\n\
             [[generate]]\nname = \"config\"\nbuiltin = \"build-config\"\nexpose = [\"API\"]\n\
             fields = { RETRIES = 3, BIG = 5000000000, DEBUG = true, GREETING = \"hi \\\"you\\\"\", object = \"x\" }\n",
        );
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = context(&config, &project);
        let reports = generate(&ctx, &GenerateOptions::default()).unwrap();
        assert_eq!(reports, [StepReport { name: "config".to_owned(), ran: true }]);
        let path = output_dir(ctx.ws, &ctx.profile, "config").join("BuildConfig.kt");
        let text = std::fs::read_to_string(&path).unwrap();
        for expected in [
            "package com.example.app\n",
            "object BuildConfig {",
            "const val VERSION: String = \"1.2.3\"",
            "const val GROUP: String = \"com.example\"",
            "const val NAME: String = \"app\"",
            "const val PROFILE: String = \"dev\"",
            "const val RETRIES: Int = 3",
            "const val BIG: Long = 5000000000L",
            "const val DEBUG: Boolean = true",
            "const val GREETING: String = \"hi \\\"you\\\"\"",
            "const val `object`: String = \"x\"",
            "const val API: String = \"https://x.test/\\$a\"",
            "val OPTIONAL: String? = null",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in\n{text}");
        }
        assert!(text.contains("const val GIT_SHA: String = \""), "{text}");
        assert!(!text.contains("BUILD_TIME"), "{text}");
        assert_eq!(text.matches("const val API:").count(), 1, "an exposed name is written once");
    }

    /// A `[[generate]]` step that writes `file` with one line of Kotlin.
    fn write_step(name: &str, file: &str, extra: &str) -> String {
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        let posix = format!("echo val {name} = 1 > \"$KARGO_GEN_DIR/{file}\"");
        let windows = format!(r#"echo val {name} = 1> "%KARGO_GEN_DIR%\{file}""#);
        format!("[[generate]]\nname = \"{name}\"\nrun = '{posix}'\nwindows-run = '{windows}'\nshell = \"{shell}\"\n{extra}\n")
    }

    #[test]
    fn generated_files_join_the_sources_of_their_targets() {
        use crate::ops::build::{existing_sources, sources};
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let steps = write_step("jvmonly", "JvmOnly.kt", "targets = [\"jvm\"]")
            + &write_step("everywhere", "Everywhere.kt", "")
            + &write_step("fixtures", "Fixtures.kt", "kind = \"test-sources\"")
            + &write_step("assets", "assets.txt", "kind = \"resources\"");
        let project = project(
            dir.path(),
            &format!(
                "[target.jvm]
[target.wasm]
{steps}"
            ),
        );
        std::fs::create_dir_all(dir.path().join("src-wasm")).unwrap();
        std::fs::write(dir.path().join("src-wasm").join("Wasm.kt"), "").unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = context(&config, &project);
        let names =
            |files: &[PathBuf]| -> Vec<String> { files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect() };

        // Reading the layout starts no step.
        let before = existing_sources(&ctx, TargetKind::Jvm, false, &["kt"]).unwrap();
        assert!(before.common.is_empty() && before.platform.is_empty());
        assert!(!root_dir(ctx.ws, &ctx.profile).exists());

        // Nothing in src-jvm/, so the JVM compiles one fragment.
        let jvm = sources(&ctx, TargetKind::Jvm, false, &["kt"]).unwrap();
        assert_eq!(names(&jvm.common), ["JvmOnly.kt", "Everywhere.kt"]);
        assert!(jvm.platform.is_empty());
        assert_eq!(names(&sources(&ctx, TargetKind::Jvm, true, &["kt"]).unwrap().common), ["Fixtures.kt"]);
        // Wasm has code of its own, so the generated files are part of that fragment.
        let wasm = sources(&ctx, TargetKind::Wasm, false, &["kt"]).unwrap();
        assert!(wasm.common.is_empty());
        assert_eq!(names(&wasm.platform), ["Wasm.kt", "Everywhere.kt"]);
        // Resources are directories, and only the JVM and Android have any.
        let resources = resource_dirs(ctx.ws, &ctx.profile, TargetKind::Jvm);
        assert_eq!(resources, [output_dir(ctx.ws, &ctx.profile, "assets")]);
    }

    #[test]
    fn source_sets_of_several_targets_join_the_sources_of_their_targets() {
        use crate::ops::build::{existing_sources, sources};
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let project = project(
            dir.path(),
            &format!(
                "[target.jvm]
[target.wasm]
[source-sets.shared]
targets = [\"jvm\", \"wasm\"]
{}",
                write_step("everywhere", "Everywhere.kt", "")
            ),
        );
        for file in ["src/Common.kt", "src-shared/Shared.kt", "tests-shared/SharedTest.kt", "src-wasm/Wasm.kt"] {
            std::fs::create_dir_all(dir.path().join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = context(&config, &project);
        let names =
            |files: &[PathBuf]| -> Vec<String> { files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect() };

        let jvm = existing_sources(&ctx, TargetKind::Jvm, false, &["kt"]).unwrap();
        assert_eq!(names(&jvm.common), ["Common.kt"]);
        assert_eq!(jvm.groups.len(), 1);
        assert_eq!((jvm.groups[0].name.as_str(), names(&jvm.groups[0].files)), ("shared", vec!["Shared.kt".to_owned()]));
        assert!(jvm.platform.is_empty() && jvm.has_platform_kotlin());
        // The set's code and the target's own are fragments of one compilation.
        let wasm = existing_sources(&ctx, TargetKind::Wasm, false, &["kt"]).unwrap();
        assert!(wasm.fragment_args().contains(&"-Xfragments=common,shared,wasm".to_owned()));
        // Its tests are compiled with the target's.
        let tests = existing_sources(&ctx, TargetKind::Jvm, true, &["kt"]).unwrap();
        assert_eq!(names(&tests.groups[0].files), ["SharedTest.kt"]);
        // The generated files join the platform side, as the set already makes it multiplatform.
        let generated = sources(&ctx, TargetKind::Jvm, false, &["kt"]).unwrap();
        assert_eq!(names(&generated.platform), ["Everywhere.kt"]);
        assert_eq!(names(&generated.common), ["Common.kt"]);
    }

    #[test]
    fn source_sets_of_some_native_platforms_only_compile_for_those_platforms() {
        use crate::ops::build::{existing_sources, for_platform};
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let project = project(
            dir.path(),
            "[target.native]
platforms = [\"linux_x64\", \"macos_arm64\", \"ios_arm64\"]
             [source-sets.apple]
platforms = [\"apple\"]
[source-sets.ios]
platforms = [\"ios\"]
             [source-sets.linux]
platforms = [\"linux\"]
",
        );
        for file in [
            "src/Common.kt",
            "src-native/Native.kt",
            "src-apple/Apple.kt",
            "src-ios/Ios.kt",
            "src-linux/Linux.kt",
            "tests-apple/AppleTest.kt",
        ] {
            std::fs::create_dir_all(dir.path().join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = context(&config, &project);
        let names =
            |files: &[PathBuf]| -> Vec<String> { files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect() };

        let all = existing_sources(&ctx, TargetKind::Native, false, &["kt"]).unwrap();
        assert_eq!(names(&all.platform), ["Native.kt"]);
        assert_eq!(all.groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["apple", "ios", "linux"]);
        // The code of a platform's sets joins the compilation of the platforms they hold, and no others.
        let groups = |konan: &str| -> Vec<(String, Vec<String>)> {
            for_platform(&ctx, all.clone(), konan).groups.iter().map(|g| (g.name.clone(), g.refines.clone())).collect()
        };
        assert_eq!(groups("ios_arm64"), [("apple".into(), vec!["native".into()]), ("ios".into(), vec!["apple".into()])]);
        assert_eq!(groups("macos_arm64"), [("apple".into(), vec!["native".into()])]);
        assert_eq!(groups("linux_x64"), [("linux".into(), vec!["native".into()])]);
        assert!(groups("mingw_x64").is_empty());
        let linux = for_platform(&ctx, all.clone(), "linux_x64").fragment_args();
        assert_eq!(linux[1], "-Xfragments=common,native,linux", "{linux:?}");
        assert!(linux.contains(&"-Xfragment-refines=linux:native".to_owned()), "{linux:?}");
        let tests = existing_sources(&ctx, TargetKind::Native, true, &["kt"]).unwrap();
        assert_eq!(names(&tests.groups[0].files), ["AppleTest.kt"]);
    }

    #[test]
    fn steps_run_only_when_their_inputs_change() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let run = "echo run >> \"$KARGO_MANIFEST_DIR/runs.txt\"; cat \"$KARGO_MANIFEST_DIR/in.txt\" > \"$KARGO_GEN_DIR/Out.kt\"";
        let windows_run = r#"echo run >> "%KARGO_MANIFEST_DIR%\runs.txt" & type "%KARGO_MANIFEST_DIR%\in.txt" > "%KARGO_GEN_DIR%\Out.kt""#;
        std::fs::write(dir.path().join("in.txt"), "one").unwrap();
        let project = project(
            dir.path(),
            &format!(
                "[[generate]]\nname = \"copy\"\ninputs = [\"in.txt\"]\nrun = '{run}'\nwindows-run = '{windows_run}'\nshell = {}\n",
                if cfg!(windows) { "\"cmd\"" } else { "\"sh\"" }
            ),
        );
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let runs = || std::fs::read_to_string(dir.path().join("runs.txt")).map(|t| t.lines().count()).unwrap_or(0);
        let out = || output_dir(project.package(0), &ProfileKind::Dev, "copy").join("Out.kt");
        {
            let ctx = context(&config, &project);
            ensure(&ctx, TargetKind::Jvm).unwrap();
            ensure(&ctx, TargetKind::Jvm).unwrap();
            assert_eq!(runs(), 1, "a step runs once per invocation");
            assert_eq!(std::fs::read_to_string(out()).unwrap().trim(), "one");
        }
        let modified = std::fs::metadata(out()).unwrap().modified().unwrap();
        {
            let ctx = context(&config, &project);
            ensure(&ctx, TargetKind::Jvm).unwrap();
            assert_eq!(runs(), 1, "the inputs did not change");
        }
        {
            let ctx = context(&config, &project);
            let report = generate(&ctx, &GenerateOptions { force: true, ..GenerateOptions::default() }).unwrap();
            assert_eq!(report, [StepReport { name: "copy".to_owned(), ran: true }]);
            assert_eq!(runs(), 2);
            assert_eq!(std::fs::metadata(out()).unwrap().modified().unwrap(), modified, "the same bytes keep the old file");
        }
        std::fs::write(dir.path().join("in.txt"), "two").unwrap();
        {
            let ctx = context(&config, &project);
            ensure(&ctx, TargetKind::Jvm).unwrap();
            assert_eq!(runs(), 3);
            assert_eq!(std::fs::read_to_string(out()).unwrap().trim(), "two");
        }
        // Removing the output makes the step run again.
        std::fs::remove_dir_all(output_dir(project.package(0), &ProfileKind::Dev, "copy")).unwrap();
        {
            let ctx = context(&config, &project);
            ensure(&ctx, TargetKind::Jvm).unwrap();
            assert_eq!(runs(), 4);
        }
    }
}
