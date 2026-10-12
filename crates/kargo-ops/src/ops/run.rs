//! `kargo run`: builds and runs the program, a `[[bin]]` (`--bin`), another
//! `main` (`--main`) or an example from `examples/` (`--example`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::build::{
    BuildOptions, BuildResults, JvmBuild, announce, bin_main_class, build, compiling, find_bin, finished, hook_env, jvm_classpaths,
    jvm_javac_args, jvm_processor_path, native_cinterop, native_cinterop_defs, native_entry_point, native_libraries, sources,
};
use super::context::BuildContext;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::mainclass::find_entry_point_in;
use kargo_compiler::native::NativeCompilation;
use kargo_compiler::{SourceSet, common_kotlin_args};
use kargo_manifest::{Hook, NativeKind, Scope, TargetKind};
use kargo_maven::Platform;
use kargo_toolchain::native::Host;
use kargo_util::fs::join_paths;
use kargo_util::process;

/// Which program `kargo run` starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Program {
    /// The project's own: `main-class` / `entry-point`, or the single `main` in the sources.
    #[default]
    Default,
    /// `--main`: a JVM main class or a Kotlin/Native entry function.
    Main(String),
    /// `--bin`: a `[[bin]]` from kargo.toml.
    Bin(String),
    /// `--example`: `examples/<name>.kt`.
    Example(String),
}

impl Program {
    fn flag(&self) -> &'static str {
        match self {
            Program::Default => "",
            Program::Main(_) => "--main",
            Program::Bin(_) => "--bin",
            Program::Example(_) => "--example",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub target: Option<TargetKind>,
    pub release: bool,
    pub program: Program,
    pub args: Vec<String>,
}

/// Picks the target to run: explicit, else JVM, Native, Wasm, Android in that order.
pub fn runnable_target(enabled: &[TargetKind], requested: Option<TargetKind>) -> Result<TargetKind> {
    if let Some(t) = requested {
        if !enabled.contains(&t) {
            bail!("target `{t}` is not enabled in kargo.toml");
        }
        return Ok(t);
    }
    [TargetKind::Jvm, TargetKind::Native, TargetKind::Wasm, TargetKind::Android]
        .into_iter()
        .find(|t| enabled.contains(t))
        .context("no runnable target is enabled")
}

/// The source file of `examples/<name>.kt`, or an error listing the examples.
fn example_file(ctx: &BuildContext, name: &str) -> Result<PathBuf> {
    let file = ctx.ws.examples_dir().join(format!("{name}.kt"));
    if file.is_file() {
        return Ok(file);
    }
    let examples = ctx.ws.examples();
    if examples.is_empty() {
        bail!("no example named `{name}`: add examples/{name}.kt with a `main` function");
    }
    bail!("no example named `{name}` in examples/; available examples: {}", examples.join(", "))
}

/// Fails early, before anything compiles, when the selected program cannot exist.
fn validate(ctx: &BuildContext, target: TargetKind, program: &Program) -> Result<()> {
    if *program != Program::Default && !matches!(target, TargetKind::Jvm | TargetKind::Native) {
        bail!("`kargo run {}` only works for the jvm and native targets, not {target}", program.flag());
    }
    match program {
        Program::Bin(name) => {
            let bin = find_bin(ctx.ws.manifest(), name)?;
            if target == TargetKind::Native && bin.entry_point.is_none() && bin.path.is_none() {
                bail!("[[bin]] `{name}` has no Kotlin/Native entry point; set `entry-point` or `path` in kargo.toml");
            }
            if target == TargetKind::Jvm && bin.main.is_none() && bin.path.is_none() {
                bail!("[[bin]] `{name}` has no JVM main class; set `main` or `path` in kargo.toml");
            }
        }
        Program::Example(name) => {
            example_file(ctx, name)?;
        }
        Program::Main(_) | Program::Default => {}
    }
    // An example links its own executable even when the project is a library.
    if target == TargetKind::Native
        && matches!(program, Program::Main(_) | Program::Bin(_))
        && ctx.ws.manifest().targets.native.as_ref().is_some_and(|n| n.kind != NativeKind::Executable)
    {
        bail!("the native target builds a library, not an executable");
    }
    Ok(())
}

/// The hint appended when the program to run is ambiguous and bins exist.
fn bin_hint(ctx: &BuildContext) -> String {
    let bins = &ctx.ws.manifest().bins;
    if bins.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = bins.iter().map(|b| b.name.as_str()).collect();
    format!("\nrun a [[bin]] with `kargo run --bin <name>`; available bins: {}", names.join(", "))
}

/// Builds and runs; returns the program's exit code.
pub fn run(ctx: &BuildContext, opts: &RunOptions) -> Result<i32> {
    let target = runnable_target(&ctx.ws.manifest().targets.enabled(), opts.target)?;
    validate(ctx, target, &opts.program)?;
    // `--main` and `--example` on Kotlin/Native link their own executable instead of the project's.
    let separate_native = target == TargetKind::Native && matches!(opts.program, Program::Main(_) | Program::Example(_));
    let results = if separate_native {
        BuildResults::default()
    } else {
        build(ctx, &BuildOptions { targets: vec![target], release: opts.release, keep_going: false })?
    };

    let root = ctx.ws.root();
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut label;
    let mut cmd = match target {
        TargetKind::Jvm => {
            let jvm = results.jvm.context("JVM build produced no output")?;
            let (main, classpath) = jvm_program(ctx, &jvm, &opts.program)?;
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx))?;
            let jdk = ctx.jdk()?;
            let mut cmd = Command::new(jdk.java());
            cmd.args(&jvm.jvm_args).arg("-cp").arg(join_paths(&classpath)).arg(&main);
            label = format!("java {main}");
            cmd
        }
        TargetKind::Wasm => {
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx))?;
            let wasm = results.wasm.context("there are no Wasm sources to run")?;
            let mut cmd = Command::new(kargo_toolchain::node()?);
            cmd.arg(&wasm.module);
            label = format!("node {}", rel(&wasm.module));
            cmd
        }
        TargetKind::Native => {
            let artifact =
                if separate_native { build_native_program(ctx, &opts.program)? } else { native_artifact(ctx, &results, &opts.program)? };
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx))?;
            label = rel(&artifact);
            Command::new(&artifact)
        }
        TargetKind::Android => {
            crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreRun, &hook_env(ctx))?;
            return crate::gradle::run(ctx);
        }
    };
    cmd.args(&opts.args).current_dir(ctx.ws.root());
    for arg in &opts.args {
        label.push(' ');
        label.push_str(arg);
    }
    ctx.unlock()?;
    ctx.shell().status("Running", format!("`{label}`"));
    ctx.shell().verbose("Command", process::describe(&cmd));
    let status = cmd.status().with_context(|| format!("failed to start `{}`", cmd.get_program().to_string_lossy()))?;
    Ok(status.code().unwrap_or(1))
}

/// The main class and classpath of the JVM program to run.
fn jvm_program(ctx: &BuildContext, jvm: &JvmBuild, program: &Program) -> Result<(String, Vec<PathBuf>)> {
    let mut classpath = vec![jvm.classes.clone()];
    classpath.extend(jvm.runtime_classpath.iter().cloned());
    let main = match program {
        Program::Default => match &jvm.main_class {
            Some(main) => main.clone(),
            None => {
                let reason = jvm.main_class_error.as_deref().unwrap_or("set `[target.jvm] main-class` in kargo.toml");
                bail!("could not determine the main class: {reason}{}", bin_hint(ctx));
            }
        },
        Program::Main(main) => main.clone(),
        Program::Bin(name) => bin_main_class(ctx.ws.root(), find_bin(ctx.ws.manifest(), name)?)?,
        Program::Example(name) => {
            let file = example_file(ctx, name)?;
            let main = find_entry_point_in(&file, false)?.jvm_class;
            let (classes, example_classpath) = compile_jvm_example(ctx, name, &file, &jvm.classes)?;
            classpath = vec![classes];
            classpath.extend(example_classpath);
            main
        }
    };
    Ok((main, classpath))
}

/// Compiles `examples/<name>.kt` against the main classes and the dev-dependencies into
/// `target/<profile>/jvm/examples/<name>`. Returns the output and the classpath it needs.
fn compile_jvm_example(ctx: &BuildContext, name: &str, file: &Path, main_classes: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let manifest = ctx.ws.manifest();
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let all_scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut dev_cp = super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &all_scopes, true)?;
    dev_cp.extend(ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&all_scopes, true), &setup.boms)?.files());
    let jdk = ctx.compiler_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(ctx.profile), &setup.contributions.compiler_args);
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let javac_args = jvm_javac_args(&setup, jvm);

    let mut classpath = vec![main_classes.to_path_buf()];
    classpath.extend(dev_cp);
    let output = ctx.ws.output_dir(ctx.profile, TargetKind::Jvm).join("examples").join(name);
    // Each example is its own module, so examples may each declare `fun main` in the same package.
    let module = format!("{}-example-{name}", manifest.package.name);
    let unit = JvmCompilation {
        name: &module,
        sources: SourceSet::new(Vec::new(), vec![file.to_path_buf()], TargetKind::Jvm.as_str()),
        classpath: classpath.clone(),
        friend_paths: Vec::new(),
        output: output.clone(),
        jvm_target: &jvm.target_for(jdk.version),
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &javac_args,
        processor_path: &processor_path,
        resources: Vec::new(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm);
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| announce(ctx, "Compiling", TargetKind::Jvm, &format!(", example {name}")))?;
    Ok((output, classpath))
}

/// The host executable of the project or of a `[[bin]]`, from a finished build.
fn native_artifact(ctx: &BuildContext, results: &BuildResults, program: &Program) -> Result<PathBuf> {
    let host = Host::current()?.konan_target;
    let for_host: Vec<_> = results.native.iter().filter(|b| b.konan_target == host).collect();
    if for_host.is_empty() {
        bail!("the native target has no build for this host ({host}); add \"host\" to `platforms`");
    }
    if for_host.iter().any(|b| b.kind != NativeKind::Executable) {
        bail!("the native target builds a library, not an executable");
    }
    let bin = match program {
        Program::Bin(name) => Some(name.as_str()),
        _ => None,
    };
    if let Some(build) = for_host.iter().find(|b| b.bin.as_deref() == bin) {
        return Ok(build.artifact.clone());
    }
    // The project's own executable is skipped when its `main` is ambiguous; say why.
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    let reason = native_entry_point(native, &sources(ctx, TargetKind::Native, false, &["kt"])).err().map(|e| e.to_string());
    bail!("could not determine the entry point: {}{}", reason.unwrap_or_else(|| "no executable was built".to_owned()), bin_hint(ctx))
}

/// Links a host executable outside `kargo build`: the main sources with another entry
/// function (`--main`), or an example linked against the main sources as a library (`--example`).
fn build_native_program(ctx: &BuildContext, program: &Program) -> Result<PathBuf> {
    let start = Instant::now();
    let env = hook_env(ctx);
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PreBuild, &env)?;
    let manifest = ctx.ws.manifest();
    let native = manifest.targets.native.as_ref().context("the Native target is not enabled")?;
    let host = Host::current()?.konan_target.to_owned();
    let setup = ctx.setup(TargetKind::Native)?;
    let main_sources = sources(ctx, TargetKind::Native, false, &["kt"]);
    let example = match program {
        Program::Example(name) => {
            let file = example_file(ctx, name)?;
            Some((name.as_str(), find_entry_point_in(&file, true)?.native_entry(), file))
        }
        _ => None,
    };
    native_cinterop_defs(ctx)?;
    let toolchain = ctx.native_toolchain()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let profile = ctx.ws.profile(ctx.profile);
    let kotlin_args = common_kotlin_args(manifest, profile, &setup.contributions.compiler_args);
    let mut libraries = native_libraries(ctx, &setup, &host, example.is_some())?;
    libraries.extend(native_cinterop(ctx, &host)?);

    let out = ctx.ws.output_dir(ctx.profile, TargetKind::Native).join(&host);
    let unit = |name, kind, sources, libraries, entry_point, dir: PathBuf| NativeCompilation {
        name,
        konan_target: host.clone(),
        kind,
        sources,
        libraries,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        entry_point,
        linker_options: &native.linker_options,
        output_dir: out.join(dir),
        debug: profile.debug,
        optimize: profile.optimize,
        test_runner: false,
    };
    let artifact = match (program, example) {
        (Program::Main(entry), _) => {
            let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join("main");
            std::fs::create_dir_all(&fp_dir)?;
            let exe =
                unit(&manifest.package.name, NativeKind::Executable, main_sources, libraries, Some(entry.clone()), PathBuf::from("main"));
            let what = format!(", {host}, main {entry}");
            exe.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            exe.artifact()
        }
        (_, Some((name, entry, file))) => {
            let fp_dir = ctx.fingerprint_dir(TargetKind::Native).join("examples");
            std::fs::create_dir_all(&fp_dir)?;
            let mut libraries = libraries;
            // The main sources become a klib the example links against, as any user of the library would.
            if !main_sources.kotlin_files().is_empty() {
                let lib_dir = Path::new("examples").join("lib");
                let lib = unit(&manifest.package.name, NativeKind::Library, main_sources, libraries.clone(), None, lib_dir);
                let what = format!(", {host}");
                let lib_fp_dir = fp_dir.join("lib");
                std::fs::create_dir_all(&lib_fp_dir)?;
                lib.compile(ctx.shell(), toolchain, &lib_fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
                libraries.push(lib.artifact());
            }
            let sources = SourceSet::new(Vec::new(), vec![file], TargetKind::Native.as_str());
            let exe = unit(name, NativeKind::Executable, sources, libraries, Some(entry), PathBuf::from("examples"));
            let what = format!(", {host}, example {name}");
            exe.compile(ctx.shell(), toolchain, &fp_dir, &|| compiling(ctx, TargetKind::Native, &what))?;
            exe.artifact()
        }
        _ => unreachable!("only `--main` and `--example` link a separate executable"),
    };
    crate::plugins::run_hook(ctx.shell(), ctx.ws, Hook::PostBuild, &env)?;
    finished(ctx, start);
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_preference() {
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], None).unwrap(), TargetKind::Jvm);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Native], None).unwrap(), TargetKind::Native);
        assert_eq!(runnable_target(&[TargetKind::Wasm, TargetKind::Jvm], Some(TargetKind::Wasm)).unwrap(), TargetKind::Wasm);
        assert!(runnable_target(&[TargetKind::Jvm], Some(TargetKind::Native)).is_err());
        assert!(runnable_target(&[], None).is_err());
    }
}
