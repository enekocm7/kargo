//! `kargo bench`: compiles the benchmarks of the JVM target (`benches/`, `benches-jvm/`), has JMH
//! generate its harness for them, and runs it. Results are printed as a table, compared with a
//! saved baseline (`--baseline`), and stored under `target/bench/` (`--save`).
//!
//! The benchmarks use kotlinx-benchmark's annotations, which are aliases of JMH's. JMH needs
//! generated code for each benchmark (`Bench_jmhTest.java` and a `META-INF/BenchmarkList`), which
//! Gradle builds get from a plugin: here the JMH bytecode generator runs over the compiled classes,
//! `javac` compiles what it writes, and `org.openjdk.jmh.Main` runs the result.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::build::{
    BuildOptions, JvmBuild, announce, build_jvm, finished, jvm_classpaths, jvm_javac_args, jvm_processor_path, select_targets,
};
use super::context::{BuildContext, TargetSetup, to_request};
use crate::bench::{self, BenchResult, Scale};
use crate::builtins;
use crate::env::EnvOptions;
use kargo_compiler::jvm::JvmCompilation;
use kargo_compiler::{Outcome, SourceSet, common_kotlin_args};
use kargo_manifest::{BenchMode, BenchRounds, BenchSettings, Scope, TargetKind};
use kargo_maven::{DependencyRequest, ModuleId, Platform, Resolution};
use kargo_util::fs::{collect_files, join_paths, remove_dir_all_if_exists};
use kargo_util::process;
use kargo_util::shell::Shell;

/// The class that writes JMH's harness for compiled benchmarks.
const GENERATOR_MAIN: &str = "org.openjdk.jmh.generators.bytecode.JmhBytecodeGenerator";

/// Kargo's settings when neither the command line, `[bench]` nor an annotation in the benchmarks sets one.
/// They keep a run to seconds per benchmark; JMH's own defaults take minutes.
const DEFAULT_WARMUP: BenchRounds = BenchRounds { count: 3, time: std::time::Duration::from_secs(1) };
const DEFAULT_ITERATIONS: BenchRounds = BenchRounds { count: 5, time: std::time::Duration::from_secs(1) };
const DEFAULT_FORKS: u32 = 2;
const DEFAULT_MODE: BenchMode = BenchMode::AverageTime;

#[derive(Debug, Clone, Default)]
pub struct BenchOptions {
    pub build: BuildOptions,
    /// The `.env` files the benchmarks' environment is read from (`--env-file`, `--no-env-file`).
    pub env: EnvOptions,
    /// Only run benchmarks whose `package.Class.method` name contains this string.
    pub filter: Option<String>,
    /// Compile the benchmarks without running them.
    pub no_run: bool,
    /// Store the results as the baseline of this name (`--save`).
    pub save: Option<String>,
    /// Compare the results with the baseline of this name (`--baseline`).
    pub baseline: Option<String>,
    /// Print JMH's JSON results on stdout instead of the table (`--format json`).
    pub json: bool,
    /// Settings from the command line, which win over `[bench]`.
    pub overrides: BenchOverrides,
}

/// The JMH settings that `kargo bench` has flags for.
#[derive(Debug, Clone, Default)]
pub struct BenchOverrides {
    pub warmup: Option<BenchRounds>,
    pub iterations: Option<BenchRounds>,
    pub forks: Option<u32>,
    pub mode: Option<BenchMode>,
    pub time_unit: Option<String>,
}

/// What JMH runs with. A setting that is `None` is left to the annotations of the benchmarks, then to JMH.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
    warmup: Option<BenchRounds>,
    iterations: Option<BenchRounds>,
    forks: Option<u32>,
    mode: Option<BenchMode>,
    time_unit: Option<String>,
    /// Whether scores are shown in whichever unit reads best, which they are unless a unit was asked for.
    scale: Scale,
}

/// The settings of a run, strongest first: the flags, `[bench]`, an annotation in the benchmarks
/// (`annotations` is their source), and Kargo's defaults.
fn settings(manifest: &BenchSettings, flags: &BenchOverrides, annotations: &str) -> Settings {
    let set_in_code = |annotation: &str| annotations.contains(annotation);
    let time_unit = flags.time_unit.clone().or_else(|| manifest.time_unit.clone());
    Settings {
        warmup: flags.warmup.or(manifest.warmup).or((!set_in_code("@Warmup")).then_some(DEFAULT_WARMUP)),
        iterations: flags.iterations.or(manifest.iterations).or((!set_in_code("@Measurement")).then_some(DEFAULT_ITERATIONS)),
        forks: flags.forks.or(manifest.forks).or((!set_in_code("@Fork")).then_some(DEFAULT_FORKS)),
        mode: flags.mode.or(manifest.mode).or((!set_in_code("@BenchmarkMode")).then_some(DEFAULT_MODE)),
        scale: if time_unit.is_some() || set_in_code("@OutputTimeUnit") { Scale::AsReported } else { Scale::Auto },
        time_unit,
    }
}

/// JMH's command line options for `settings`, writing the results as JSON to `results`.
fn jmh_args(settings: &Settings, filter: Option<&str>, results: &Path) -> Vec<String> {
    let mut args =
        vec!["-rf".to_owned(), "json".to_owned(), "-rff".to_owned(), process::slash(results), "-foe".to_owned(), "true".to_owned()];
    for (rounds, count, time) in [(settings.warmup, "-wi", "-w"), (settings.iterations, "-i", "-r")] {
        if let Some(rounds) = rounds {
            args.extend([count.to_owned(), rounds.count.to_string(), time.to_owned(), rounds.jmh_time()]);
        }
    }
    if let Some(forks) = settings.forks {
        args.extend(["-f".to_owned(), forks.to_string()]);
    }
    if let Some(mode) = settings.mode {
        args.extend(["-bm".to_owned(), mode.to_string()]);
    }
    if let Some(unit) = &settings.time_unit {
        args.extend(["-tu".to_owned(), unit.clone()]);
    }
    // JMH takes regular expressions, matched against `package.Class.method`; the filter is plain text, with `#` for a method as in `kargo test`.
    if let Some(filter) = filter {
        let text: Vec<String> = filter.replace('#', ".").split('.').map(regex_escape).collect();
        args.push(format!(".*{}.*", text.join("\\.")));
    }
    args
}

fn regex_escape(text: &str) -> String {
    text.chars().flat_map(|c| if "\\+*?()|[]{}^$".contains(c) { vec!['\\', c] } else { vec![c] }).collect()
}

/// Runs the benchmarks; the results are printed, compared and saved as the options say.
pub fn bench(ctx: &BuildContext, opts: &BenchOptions) -> Result<()> {
    let start = Instant::now();
    if let Some(other) = opts.build.targets.iter().find(|target| **target != TargetKind::Jvm) {
        bail!("benchmarks of the {other} target are not supported yet; `kargo bench` runs the JVM target's benchmarks (`-t jvm`)");
    }
    let targets = select_targets(ctx, &opts.build.targets)?;
    if !targets.contains(&TargetKind::Jvm) {
        bail!("`kargo bench` runs the benchmarks of the JVM target, which is not enabled; add it with `kargo target add jvm`");
    }
    for other in targets.iter().filter(|target| **target != TargetKind::Jvm) {
        if !collect_files(&ctx.ws.target_benches(*other), &["kt"]).is_empty() {
            ctx.shell().warn(format!("benchmarks of the {other} target are not supported yet; only the JVM benchmarks run"));
        }
    }
    if let Some(name) = &opts.save {
        bench::validate_baseline_name(name)?;
    }
    // Before anything compiles: a baseline that is not there is not worth a benchmark run.
    let baseline = opts.baseline.as_deref().map(|name| read_baseline(ctx, name)).transpose()?;

    let sources = bench_sources(ctx);
    if sources.is_empty() {
        ctx.shell().note("no benchmarks found for the jvm target (put them in benches/ or benches-jvm/)");
        return Ok(());
    }
    let env = ctx.project_env(&opts.env)?;

    let main = build_jvm(ctx)?;
    let compiled = compile_benches(ctx, &main, sources.clone())?;
    let harness = generate_harness(ctx, &compiled)?;
    if opts.no_run {
        finished(ctx, start);
        return Ok(());
    }

    let annotations: String = sources.all().filter_map(|file| std::fs::read_to_string(file).ok()).collect::<Vec<_>>().join("\n");
    let settings = settings(&ctx.ws.manifest().bench, &opts.overrides, &annotations);
    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm);
    let results_path = out.join("bench-results.json");
    let _ = std::fs::remove_file(&results_path);
    let jdk = ctx.compiler_jdk()?;
    let jvm = ctx.ws.manifest().targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let profile = ctx.ws.profile(&ctx.profile)?;
    let mut run_cp = vec![harness.classes.clone(), compiled.classes.clone()];
    run_cp.extend(compiled.classpath.iter().cloned());

    let mut cmd = Command::new(jdk.java());
    cmd.args(jdk.default_run_args());
    // JMH 1.37 reads memory through `sun.misc.Unsafe`, which newer JDKs warn about on every start; the forks inherit this.
    if jdk.version >= 23 {
        cmd.arg("--sun-misc-unsafe-memory-access=allow");
    }
    cmd.args(&jvm.jvm_args)
        .args(&profile.jvm_args)
        .args(&ctx.ws.manifest().bench.jvm_args)
        .arg("-cp")
        .arg(join_paths(&run_cp))
        .arg("org.openjdk.jmh.Main")
        .args(jmh_args(&settings, opts.filter.as_deref(), &results_path))
        .current_dir(ctx.ws.root())
        .envs(env.for_child())
        .stdin(Stdio::null());
    ctx.shell().status("Running", format!("benchmarks (JMH {})", compiled.jmh_version));
    run_jmh(ctx, cmd, opts.filter.as_deref())?;

    let json = std::fs::read_to_string(&results_path)
        .with_context(|| format!("JMH did not write its results to {}; no benchmark ran", results_path.display()))?;
    let results = bench::parse_results(&json)?;
    if results.is_empty() {
        match &opts.filter {
            Some(filter) => bail!("no benchmark matches `{filter}`; the filter matches `package.Class.method` names"),
            None => bail!("JMH found no benchmarks; mark functions of a `@State` class in benches/ with `@Benchmark`"),
        }
    }
    report(&results, baseline.as_deref(), settings.scale, opts.json, &json)?;
    if let Some(name) = &opts.save {
        save_baseline(ctx, name, &json)?;
    }
    finished(ctx, start);
    Ok(())
}

/// The benchmark sources of the JVM target.
fn bench_sources(ctx: &BuildContext) -> SourceSet {
    let extensions = ["kt", "java"];
    SourceSet::new(
        collect_files(&ctx.ws.common_benches(), &extensions),
        collect_files(&ctx.ws.target_benches(TargetKind::Jvm), &extensions),
        TargetKind::Jvm.as_str(),
    )
}

/// The libraries the benchmarks compile and run against: those of the tests, kotlinx-benchmark and JMH.
fn bench_resolution(ctx: &BuildContext, setup: &TargetSetup) -> Result<Resolution> {
    let scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut requests = setup.requests(&scopes, true);
    for dependency in builtins::benchmark_dependencies(ctx.ws.manifest()) {
        let request = to_request(&dependency);
        if !requests.iter().any(|existing| existing.same_artifact(&request)) {
            requests.push(request);
        }
    }
    ctx.resolve("jvm/bench", Platform::Jvm, &requests, &setup.boms)
}

/// The version of JMH in `resolution`, which the harness generator must match.
fn jmh_version(resolution: &Resolution) -> String {
    resolution
        .node(&ModuleId::new("org.openjdk.jmh", "jmh-core"))
        .map(|node| node.version.clone())
        .unwrap_or_else(|| builtins::JMH_VERSION.to_owned())
}

/// JMH's harness generator and its dependencies.
fn generator_tools(ctx: &BuildContext, jmh_version: &str) -> Result<Resolution> {
    let generator = DependencyRequest::new(ModuleId::new("org.openjdk.jmh", "jmh-generator-bytecode"), Some(jmh_version.to_owned()));
    ctx.resolve("jvm/bench-tools", Platform::Jvm, &[generator], &[])
}

/// Downloads and locks what `kargo bench` needs, for `kargo fetch`. Does nothing without benchmarks.
pub(crate) fn fetch(ctx: &BuildContext) -> Result<()> {
    if bench_sources(ctx).is_empty() {
        return Ok(());
    }
    let setup = ctx.setup(TargetKind::Jvm)?;
    let resolution = bench_resolution(ctx, &setup)?;
    generator_tools(ctx, &jmh_version(&resolution))?;
    ctx.plugin_jars(&[builtins::benchmark_plugin()])?;
    Ok(())
}

/// Compiled benchmarks and what they run against.
struct CompiledBenches {
    classes: PathBuf,
    /// The main classes followed by the libraries of the benchmarks: dependencies, dev-dependencies and JMH.
    classpath: Vec<PathBuf>,
    jmh_version: String,
    outcome: Outcome,
}

/// Compiles the benchmarks against `main` and the libraries tests use, plus kotlinx-benchmark and JMH,
/// into `target/<profile>/jvm/bench-classes`.
fn compile_benches(ctx: &BuildContext, main: &JvmBuild, sources: SourceSet) -> Result<CompiledBenches> {
    let manifest = ctx.ws.manifest();
    let setup = ctx.setup(TargetKind::Jvm)?;
    // Make sure the main classpaths are locked too.
    let _ = jvm_classpaths(ctx, &setup)?;
    let scopes = [Scope::Compile, Scope::Runtime, Scope::Provided];
    let mut libraries = super::local::test_fixtures(ctx)?;
    libraries.extend(super::local::artifacts(ctx, super::local::LocalTarget::Jvm, &scopes, true)?);
    let resolution = bench_resolution(ctx, &setup)?;
    libraries.extend(resolution.files());
    let jmh_version = jmh_version(&resolution);

    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    // JMH writes subclasses of the `@State` classes, so they must not be final.
    let mut plugin_specs = setup.contributions.plugins.clone();
    let opens_state = builtins::benchmark_plugin();
    match plugin_specs.iter_mut().find(|p| p.group == opens_state.group && p.artifact == opens_state.artifact) {
        Some(existing) => existing.options.extend(opens_state.options),
        None => plugin_specs.push(opens_state),
    }
    let plugins = ctx.plugin_jars(&plugin_specs)?;
    let kotlin_args = common_kotlin_args(manifest, ctx.ws.profile(&ctx.profile)?, &setup.contributions.compiler_args);
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let javac_args = jvm_javac_args(&setup, jvm);
    let processor_path = jvm_processor_path(ctx, &setup, true)?;
    let jvm_target = jvm.target_for(jdk.version)?;

    let classes = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("bench-classes");
    let mut classpath = vec![main.classes.clone()];
    classpath.extend(libraries);
    let unit = JvmCompilation {
        name: &format!("{}-bench", manifest.package.name),
        sources,
        classpath: classpath.clone(),
        // Benchmarks may use the package's `internal` declarations, like its tests.
        friend_paths: vec![main.classes.clone()],
        output: classes.clone(),
        jvm_target: &jvm_target,
        plugins: &plugins,
        kotlin_args: &kotlin_args,
        javac_args: &javac_args,
        processor_path: &processor_path,
        resources: vec![ctx.ws.test_resources()],
    };
    let outcome = unit.compile(ctx.shell(), jdk, compiler, &ctx.fingerprint_dir(TargetKind::Jvm), &|| {
        announce(ctx, "Compiling", TargetKind::Jvm, ", benches")
    })?;
    Ok(CompiledBenches { classes, classpath, jmh_version, outcome })
}

/// JMH's harness for the compiled benchmarks.
struct Harness {
    /// The compiled harness, with `META-INF/BenchmarkList` listing the benchmarks.
    classes: PathBuf,
}

/// Runs JMH's bytecode generator over the benchmarks and compiles what it writes. The generator loads
/// the classes (its `reflection` mode) rather than reading them with its own copy of ASM, which is
/// older than the newest class file versions.
fn generate_harness(ctx: &BuildContext, compiled: &CompiledBenches) -> Result<Harness> {
    let manifest = ctx.ws.manifest();
    let jdk = ctx.jvm_jdk()?;
    let out = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm);
    let generated = out.join("bench-generated");
    let (java_dir, resource_dir) = (generated.join("java"), generated.join("resources"));
    let fingerprints = ctx.fingerprint_dir(TargetKind::Jvm);
    let harness_classes = out.join("bench-harness-classes");

    // The harness only changes when the benchmarks do.
    if compiled.outcome != Outcome::Fresh || !java_dir.is_dir() {
        let tools = generator_tools(ctx, &compiled.jmh_version)?;
        remove_dir_all_if_exists(&generated)?;
        for dir in [&java_dir, &resource_dir] {
            std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        announce(ctx, "Generating", TargetKind::Jvm, ", JMH harness");
        let mut generator_cp = tools.files();
        generator_cp.push(compiled.classes.clone());
        generator_cp.extend(compiled.classpath.iter().cloned());
        let mut cmd = Command::new(jdk.java());
        if jdk.version >= 23 {
            cmd.arg("--sun-misc-unsafe-memory-access=allow");
        }
        cmd.arg("-cp")
            .arg(join_paths(&generator_cp))
            .arg(GENERATOR_MAIN)
            .arg(&compiled.classes)
            .arg(&java_dir)
            .arg(&resource_dir)
            .arg("reflection")
            .current_dir(ctx.ws.root());
        ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
        // The generator reports each class it reads; that is only worth showing when it fails.
        let output = cmd.output().context("failed to start JMH's benchmark generator")?;
        if !output.status.success() || ctx.shell().is_verbose() {
            let mut err = anstream::stderr();
            err.write_all(&output.stdout)?;
            err.write_all(&output.stderr)?;
        }
        if !output.status.success() {
            bail!("JMH's benchmark generator failed ({})", process::describe_status(output.status));
        }
    }

    let java_files = collect_files(&java_dir, &["java"]);
    if java_files.is_empty() {
        bail!("no benchmarks found; mark functions of a `@State` class in benches/ with `@Benchmark`");
    }
    let mut classpath = vec![compiled.classes.clone()];
    classpath.extend(compiled.classpath.iter().cloned());
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let unit = JvmCompilation {
        name: &format!("{}-bench-harness", manifest.package.name),
        sources: SourceSet::new(java_files, Vec::new(), TargetKind::Jvm.as_str()),
        classpath,
        friend_paths: Vec::new(),
        output: harness_classes.clone(),
        jvm_target: &jvm.target_for(jdk.version)?,
        plugins: &[],
        kotlin_args: &[],
        javac_args: &[],
        processor_path: &[],
        resources: vec![resource_dir],
    };
    unit.compile(ctx.shell(), jdk, ctx.kotlin_compiler()?, &fingerprints, &|| {
        announce(ctx, "Compiling", TargetKind::Jvm, ", JMH harness")
    })?;
    Ok(Harness { classes: harness_classes })
}

/// Runs JMH, with `filter` as the text that selected the benchmarks. Its output is long, so it is read for the
/// name of each benchmark as it starts, and shown in full with `--verbose` or when the run fails.
fn run_jmh(ctx: &BuildContext, mut cmd: Command, filter: Option<&str>) -> Result<()> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("failed to start JMH")?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let verbose = ctx.shell().is_verbose();
    let readers = [
        read_output(child.stdout.take().context("missing JMH output")?, ctx.shell().clone(), log.clone()),
        read_output(child.stderr.take().context("missing JMH output")?, ctx.shell().clone(), log.clone()),
    ];
    let status = child.wait().context("failed to wait for JMH")?;
    for reader in readers {
        reader.join().map_err(|_| anyhow::anyhow!("the JMH output reader panicked"))?;
    }
    if !status.success() {
        let log = log.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(filter) = filter
            && log.iter().any(|line| line.starts_with("No matching benchmarks"))
        {
            bail!("no benchmark matches `{filter}`; the filter matches `package.Class.method` names");
        }
        if !verbose {
            let mut err = anstream::stderr();
            for line in log.iter() {
                let _ = writeln!(err, "{line}");
            }
        }
        bail!("the benchmarks failed ({})", process::describe_status(status));
    }
    Ok(())
}

/// Reads a stream of JMH's output on a thread: every line goes to `log`, and with `--verbose` to stderr; otherwise
/// the start of each benchmark is reported as a status.
fn read_output(stream: impl std::io::Read + Send + 'static, shell: Shell, log: Arc<Mutex<Vec<String>>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let verbose = shell.is_verbose();
        let (mut benchmark, mut announced) = (String::new(), String::new());
        let mut err = anstream::stderr();
        for line in BufReader::new(stream).lines().map_while(std::result::Result::ok) {
            if verbose {
                let _ = writeln!(err, "{line}");
            } else if let Some(name) = line.strip_prefix("# Benchmark: ") {
                benchmark = name.trim().to_owned();
            } else if let Some(params) = line.strip_prefix("# Parameters: ") {
                benchmark = format!("{} {}", benchmark.split(" (").next().unwrap_or_default(), params.trim());
            } else if line.starts_with("# Run progress:") && benchmark != announced {
                shell.status("Benchmarking", &benchmark);
                announced = benchmark.clone();
            }
            log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
        }
    })
}

/// Prints the results: JMH's JSON on stdout with `--format json` and the report on stderr, else the report on stdout.
fn report(results: &[BenchResult], baseline: Option<&[BenchResult]>, scale: Scale, json: bool, raw: &str) -> Result<()> {
    let mut out: Box<dyn Write> = if json { Box::new(anstream::stderr()) } else { Box::new(anstream::stdout()) };
    writeln!(out)?;
    match baseline {
        Some(baseline) => {
            let summary = bench::write_comparison(&mut out, baseline, results, scale)?;
            writeln!(
                out,
                "\n{} faster, {} slower, {} unchanged{}",
                summary.faster,
                summary.slower,
                summary.unchanged,
                if summary.new > 0 { format!(", {} not in the baseline", summary.new) } else { String::new() }
            )?;
        }
        None => bench::write_table(&mut out, results, scale)?,
    }
    out.flush()?;
    if json {
        let mut stdout = anstream::stdout();
        stdout.write_all(raw.as_bytes())?;
        if !raw.ends_with('\n') {
            writeln!(stdout)?;
        }
    }
    Ok(())
}

/// Where baselines are kept: `target/bench/` (`target/bench/<member>/` in a workspace).
fn baseline_path(ctx: &BuildContext, name: &str) -> PathBuf {
    ctx.ws.work_dir("bench").join(format!("{name}.json"))
}

fn read_baseline(ctx: &BuildContext, name: &str) -> Result<Vec<BenchResult>> {
    bench::validate_baseline_name(name)?;
    let path = baseline_path(ctx, name);
    let json = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("no baseline named `{name}`; save one with `kargo bench --save {name}`")
        } else {
            anyhow::Error::new(e).context(format!("failed to read {}", path.display()))
        }
    })?;
    bench::parse_results(&json).with_context(|| format!("failed to read the baseline {}", path.display()))
}

fn save_baseline(ctx: &BuildContext, name: &str, json: &str) -> Result<()> {
    let path = baseline_path(ctx, name);
    kargo_util::fs::write_string_atomic(&path, json)?;
    let shown = path.strip_prefix(ctx.ws.root()).unwrap_or(&path);
    ctx.shell().status("Saved", format!("baseline `{name}` in {}", shown.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rounds(count: u32, seconds: u64) -> BenchRounds {
        BenchRounds { count, time: Duration::from_secs(seconds) }
    }

    #[test]
    fn defaults_apply_without_settings() {
        let s = settings(&BenchSettings::default(), &BenchOverrides::default(), "class A");
        assert_eq!(s.warmup, Some(rounds(3, 1)));
        assert_eq!(s.iterations, Some(rounds(5, 1)));
        assert_eq!((s.forks, s.mode, s.time_unit.as_deref()), (Some(2), Some(BenchMode::AverageTime), None));
        assert_eq!(s.scale, Scale::Auto);
    }

    #[test]
    fn annotations_replace_the_defaults_but_not_the_manifest() {
        let code = "@Warmup(iterations = 1)\n@BenchmarkMode(Mode.Throughput)\n@OutputTimeUnit(TimeUnit.MILLISECONDS)\nclass A";
        let s = settings(&BenchSettings::default(), &BenchOverrides::default(), code);
        assert_eq!((s.warmup, s.mode, s.time_unit.clone()), (None, None, None), "JMH reads these from the annotations");
        assert_eq!(s.iterations, Some(rounds(5, 1)), "no @Measurement");
        assert_eq!(s.scale, Scale::AsReported, "an @OutputTimeUnit picks the unit");

        let manifest = BenchSettings { warmup: Some(rounds(2, 2)), mode: Some(BenchMode::SingleShot), ..Default::default() };
        let s = settings(&manifest, &BenchOverrides::default(), code);
        assert_eq!((s.warmup, s.mode), (Some(rounds(2, 2)), Some(BenchMode::SingleShot)), "[bench] wins over annotations");
    }

    #[test]
    fn flags_win_over_the_manifest() {
        let manifest =
            BenchSettings { forks: Some(3), time_unit: Some("ms".into()), iterations: Some(rounds(10, 1)), ..Default::default() };
        let flags = BenchOverrides { forks: Some(0), time_unit: Some("us".into()), ..Default::default() };
        let s = settings(&manifest, &flags, "");
        assert_eq!((s.forks, s.time_unit.as_deref(), s.iterations), (Some(0), Some("us"), Some(rounds(10, 1))));
        assert_eq!(s.scale, Scale::AsReported);
    }

    #[test]
    fn jmh_gets_the_settings_as_options() {
        let s = settings(&BenchSettings::default(), &BenchOverrides { time_unit: Some("ms".into()), ..Default::default() }, "");
        let args = jmh_args(&s, Some("Parse.parse"), Path::new("out/results.json"));
        assert_eq!(
            args.join(" "),
            "-rf json -rff out/results.json -foe true -wi 3 -w 1s -i 5 -r 1s -f 2 -bm avgt -tu ms .*Parse\\.parse.*"
        );
        let bare = Settings { warmup: None, iterations: None, forks: None, mode: None, time_unit: None, scale: Scale::Auto };
        assert_eq!(jmh_args(&bare, None, Path::new("r.json")).join(" "), "-rf json -rff r.json -foe true");
        // `#` separates the class from the method, and regular expression characters are plain text.
        assert_eq!(jmh_args(&bare, Some("A#b$c"), Path::new("r.json")).last().unwrap(), ".*A\\.b\\$c.*");
    }
}
