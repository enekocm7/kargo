//! Command-line interface.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, SubcommandCandidates};

use kargo_manifest::edit::DependencySection;
use kargo_manifest::{Scope, TargetKind};
use kargo_ops::ops::build::BuildOptions;
use kargo_ops::ops::check::CheckOptions;
use kargo_ops::ops::context::BuildContext;
use kargo_ops::ops::doc::DocOptions;
use kargo_ops::ops::fmt::FmtOptions;
use kargo_ops::ops::lint::LintOptions;
use kargo_ops::ops::run::RunOptions;
use kargo_ops::ops::test::TestOptions;
use kargo_ops::ops::toolchain::{self as toolchain_ops, ToolKind};
use kargo_ops::ops::{self, Configuration, new::NewOptions};
use kargo_ops::workspace::{ProfileKind, Workspace};
use kargo_util::config::Config;
use kargo_util::shell::{Shell, Verbosity};

use crate::complete::{self, ShellArg};

/// Exit code for build and test failures (matches Cargo).
const FAILURE: u8 = 101;

const AFTER_HELP: &str = "See `kargo help <command>` for more information on a specific command.";

#[derive(Debug, Parser)]
#[command(
    name = "kargo",
    version,
    about = "Kotlin's package manager and build tool",
    long_about = "Kargo builds Kotlin projects for the JVM, WebAssembly, Kotlin/Native and Android,\nmanages Maven dependencies and runs tests, configured by a single kargo.toml.",
    after_help = AFTER_HELP,
    add = SubcommandCandidates::new(complete::external_subcommands)
)]
pub struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct GlobalArgs {
    /// Use verbose output (show compiler commands)
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Do not print Kargo status messages
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Run without accessing the network
    #[arg(long, global = true, env = "KARGO_OFFLINE")]
    offline: bool,
    /// Require kargo.lock to be up to date
    #[arg(long, global = true)]
    locked: bool,
    /// Ignore cached metadata and kargo.lock, re-checking repositories and re-verifying cached artifacts
    #[arg(long, global = true)]
    refresh: bool,
    /// Path to kargo.toml
    #[arg(long, global = true, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TargetArg {
    Jvm,
    Wasm,
    Native,
    Android,
}

impl From<TargetArg> for TargetKind {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Jvm => TargetKind::Jvm,
            TargetArg::Wasm => TargetKind::Wasm,
            TargetArg::Native => TargetKind::Native,
            TargetArg::Android => TargetKind::Android,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScopeArg {
    Compile,
    Runtime,
    Provided,
}

impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Compile => Scope::Compile,
            ScopeArg::Runtime => Scope::Runtime,
            ScopeArg::Provided => Scope::Provided,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ToolArg {
    /// A Temurin JDK
    Jdk,
    /// A Kotlin/Native distribution
    Native,
}

impl From<ToolArg> for ToolKind {
    fn from(t: ToolArg) -> Self {
        match t {
            ToolArg::Jdk => ToolKind::Jdk,
            ToolArg::Native => ToolKind::Native,
        }
    }
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// List running compiler daemons
    Status,
    /// Stop every running compiler daemon
    Stop,
}

#[derive(Debug, Subcommand)]
enum ToolchainCommand {
    /// List installed JDKs and Kotlin/Native distributions
    #[command(visible_alias = "ls")]
    List,
    /// Install a toolchain (without arguments: everything this project needs)
    Install {
        #[arg(value_enum)]
        kind: Option<ToolArg>,
        /// JDK: `21` (latest 21) or `21.0.5+11`; Native: a Kotlin version (default: the project's)
        version: Option<String>,
    },
    /// Remove an installed toolchain
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(value_enum)]
        kind: ToolArg,
        /// JDK: a major version (removes all its releases) or a release; Native: a version
        #[arg(add = ArgValueCandidates::new(complete::installed_toolchains))]
        version: String,
    },
    /// Show JDK versions available for download
    Available,
    /// Show which toolchains this project uses
    Which,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConfigurationArg {
    Compile,
    Runtime,
    Test,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Build with the release profile
    #[arg(short, long)]
    release: bool,
    /// Only this target (repeatable); defaults to all enabled targets
    #[arg(short, long = "target", value_enum)]
    targets: Vec<TargetArg>,
}

impl BuildArgs {
    fn options(&self) -> BuildOptions {
        BuildOptions { targets: self.targets.iter().map(|t| (*t).into()).collect(), release: self.release, keep_going: false }
    }
}

#[derive(Debug, Args)]
struct NewArgs {
    /// Use a library template instead of an application
    #[arg(long)]
    lib: bool,
    /// Targets to enable (comma-separated)
    #[arg(short, long = "target", value_enum, value_delimiter = ',', default_value = "jvm")]
    targets: Vec<TargetArg>,
    /// Built-in frameworks to enable: compose, serialization, coroutines, ktor, lombok, spring-boot
    #[arg(short = 'w', long = "with", value_delimiter = ',', add = ArgValueCandidates::new(complete::frameworks))]
    frameworks: Vec<String>,
    /// Maven group / base package
    #[arg(long, default_value = "com.example")]
    group: String,
    /// Package name (defaults to the directory name)
    #[arg(long)]
    name: Option<String>,
    /// Kotlin version
    #[arg(long, default_value = kargo_manifest::DEFAULT_KOTLIN_VERSION)]
    kotlin: String,
    /// Do not initialize a git repository
    #[arg(long)]
    no_vcs: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new Kotlin project in a new directory
    New {
        /// Directory to create
        path: PathBuf,
        #[command(flatten)]
        args: NewArgs,
    },
    /// Create a new Kotlin project in an existing directory
    Init {
        /// Directory (defaults to the current one)
        path: Option<PathBuf>,
        #[command(flatten)]
        args: NewArgs,
    },
    /// Compile the project
    #[command(visible_alias = "b")]
    Build {
        #[command(flatten)]
        build: BuildArgs,
        /// Build every target even if one fails, then report all failures
        #[arg(long)]
        keep_going: bool,
    },
    /// Type-check the project without packaging or linking
    #[command(visible_alias = "c")]
    Check {
        #[command(flatten)]
        build: BuildArgs,
        /// Also check the test sources
        #[arg(long)]
        tests: bool,
        /// Check every target even if one fails, then report all failures
        #[arg(long)]
        keep_going: bool,
    },
    /// Build and run the program
    #[command(visible_alias = "r")]
    Run {
        #[arg(short, long)]
        release: bool,
        /// Target to run (defaults to jvm, then native, wasm, android)
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Arguments for the program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Compile and run tests
    #[command(visible_alias = "t")]
    Test {
        #[command(flatten)]
        build: BuildArgs,
        /// Only run tests whose `package.Class.method` name contains this string
        filter: Option<String>,
        /// Compile tests but do not run them
        #[arg(long)]
        no_run: bool,
        /// Test every target even if one fails, then print a result per target
        #[arg(long)]
        no_fail_fast: bool,
    },
    /// Generate API documentation from KDoc with Dokka
    #[command(visible_alias = "d")]
    Doc {
        /// Only this target (repeatable); defaults to all enabled targets
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
        /// Open the documentation in a browser when it is generated
        #[arg(long)]
        open: bool,
    },
    /// Format Kotlin sources with ktlint
    Fmt {
        /// Report unformatted files without changing them; fail if there are any
        #[arg(long)]
        check: bool,
    },
    /// Analyze Kotlin sources with detekt
    Lint {
        /// Record the current issues in the baseline file instead of reporting them
        #[arg(long)]
        create_baseline: bool,
    },
    /// Add dependencies or built-in frameworks to kargo.toml
    Add {
        /// Coordinates: group:artifact[:version[:classifier]] (latest release if no version)
        #[arg(required_unless_present = "frameworks")]
        coordinates: Vec<String>,
        /// Add to [dev-dependencies]
        #[arg(long, conflicts_with = "target")]
        dev: bool,
        /// Add to [target.<target>.dependencies]
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Dependency scope
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        /// Import as a BOM (platform) managing versions
        #[arg(long)]
        bom: bool,
        /// Allow pre-release versions when picking the latest
        #[arg(long)]
        pre: bool,
        /// Do not check that the artifact exists
        #[arg(long)]
        no_verify: bool,
        /// Enable built-in frameworks instead (comma-separated)
        #[arg(
            short = 'F',
            long = "framework",
            value_delimiter = ',',
            conflicts_with_all = ["dev", "target", "scope", "bom"],
            add = ArgValueCandidates::new(complete::frameworks)
        )]
        frameworks: Vec<String>,
    },
    /// Remove dependencies from kargo.toml
    #[command(visible_alias = "rm")]
    Remove {
        /// group:artifact or the manifest key
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(long, conflicts_with = "target")]
        dev: bool,
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
    },
    /// Display the dependency tree
    Tree {
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Which classpath to show
        #[arg(short, long, value_enum, default_value = "runtime")]
        configuration: ConfigurationArg,
        /// Maximum display depth
        #[arg(long)]
        depth: Option<usize>,
        /// Only list modules requested in several versions
        #[arg(short, long)]
        duplicates: bool,
    },
    /// Download all dependencies and toolchains
    Fetch {
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
    },
    /// Re-resolve dependencies and rewrite kargo.lock
    Update,
    /// Remove build output
    Clean {
        /// Only remove the release profile output
        #[arg(short, long)]
        release: bool,
    },
    /// Build a self-contained executable jar (JVM)
    Package {
        #[arg(short, long)]
        release: bool,
    },
    /// Run a task from [tasks], or list tasks
    Task {
        /// Task name (omit to list tasks)
        #[arg(add = ArgValueCandidates::new(complete::tasks))]
        name: Option<String>,
        /// List the tasks defined in kargo.toml
        #[arg(short, long, conflicts_with = "name")]
        list: bool,
        /// Extra arguments appended to the task's command
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List plugins: hook plugins, compiler plugins and `kargo-*` subcommands
    Plugins,
    /// Run Gradle tasks in the generated Android project
    Android {
        /// Arguments passed to Gradle
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Manage JDKs and Kotlin/Native distributions
    Toolchain {
        #[command(subcommand)]
        command: ToolchainCommand,
    },
    /// Show or stop the background compiler daemons that keep the Kotlin compiler warm
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Print project information
    Info,
    /// Print a shell completion script
    #[command(long_about = "Print a shell completion script.

The script completes commands, flags, targets and toolchain subcommands, and asks kargo for
the current project's tasks, `kargo-*` plugins and installed toolchains as you type.")]
    Completions {
        /// The shell to generate the script for
        #[arg(value_enum)]
        shell: ShellArg,
    },
    /// Run a task or a `kargo-<name>` plugin executable
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Entry point used by `main.rs`.
pub fn main() -> ExitCode {
    complete::complete_if_requested();
    let cli = parse();
    let verbosity = if cli.global.verbose {
        Verbosity::Verbose
    } else if cli.global.quiet {
        Verbosity::Quiet
    } else {
        Verbosity::Normal
    };
    let shell = Shell::new(verbosity);
    match execute(cli, &shell) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            report_error(&shell, &err);
            ExitCode::from(FAILURE)
        }
    }
}

/// Parses the command line. `kargo --help` also lists the current project's tasks.
fn parse() -> Cli {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let mut cmd = Cli::command();
    if wants_top_level_help(&args) {
        if let Some(tasks) = tasks_help() {
            cmd = cmd.after_help(format!(
                "{tasks}
{AFTER_HELP}"
            ));
        }
    }
    let matches = cmd.get_matches_from(args);
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

/// `kargo -h`, `kargo --help` or `kargo help`, possibly with global flags.
fn wants_top_level_help(args: &[std::ffi::OsString]) -> bool {
    let mut words = args.iter().skip(1).filter_map(|a| a.to_str()).skip_while(|a| a.starts_with('-') && !matches!(*a, "-h" | "--help"));
    matches!(words.next(), Some("-h" | "--help" | "help")) && words.next().is_none()
}

/// The "Tasks:" section of `kargo --help`, when run inside a project that defines tasks.
fn tasks_help() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let ws = Workspace::discover(&cwd, None).ok()?;
    let tasks = &ws.manifest().tasks;
    if tasks.is_empty() {
        return None;
    }
    let width = tasks.keys().map(String::len).max().unwrap_or(0);
    let mut out = String::from(
        "Tasks (from kargo.toml, run with `kargo <task>`):
",
    );
    for task in tasks.values() {
        out.push_str(&format!(
            "  {:width$}  {}
",
            task.name,
            task_description(task)
        ));
    }
    Some(out)
}

fn task_description(task: &kargo_manifest::Task) -> String {
    let mut desc = task.description.clone().unwrap_or_else(|| task.platform_run().join(" && "));
    if !task.hooks.is_empty() {
        let hooks: Vec<&str> = task.hooks.iter().map(|h| h.as_str()).collect();
        desc.push_str(&format!(" [hooks: {}]", hooks.join(", ")));
    }
    desc
}

fn report_error(shell: &Shell, err: &anyhow::Error) {
    shell.error(err);
    for cause in err.chain().skip(1) {
        eprintln!("\nCaused by:\n  {cause}");
    }
}

fn exit_code(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

fn load_workspace(global: &GlobalArgs) -> Result<Workspace> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    Workspace::discover(&cwd, global.manifest_path.as_deref())
}

fn context<'a>(config: &'a Config, ws: &'a Workspace, global: &GlobalArgs, profile: ProfileKind) -> Result<BuildContext<'a>> {
    let mut ctx = BuildContext::new(config, ws, profile)?;
    ctx.locked = global.locked;
    Ok(ctx)
}

fn profile(release: bool) -> ProfileKind {
    if release { ProfileKind::Release } else { ProfileKind::Dev }
}

fn section(dev: bool, target: Option<TargetArg>) -> DependencySection {
    match (dev, target) {
        (true, _) => DependencySection::Dev,
        (false, Some(t)) => DependencySection::Target(t.into()),
        (false, None) => DependencySection::Main,
    }
}

fn new_options(dir: &Path, args: NewArgs) -> Result<NewOptions> {
    let name = match args.name {
        Some(n) => n,
        None => {
            let dir = kargo_ops::workspace::dunce_canonicalize(dir);
            dir.file_name().map(|n| n.to_string_lossy().into_owned()).context("cannot infer a package name; pass --name")?
        }
    };
    let mut targets: Vec<TargetKind> = Vec::new();
    for t in args.targets {
        let t = t.into();
        if !targets.contains(&t) {
            targets.push(t);
        }
    }
    Ok(NewOptions {
        name,
        group: args.group,
        lib: args.lib,
        targets,
        frameworks: args.frameworks,
        kotlin_version: args.kotlin,
        vcs: !args.no_vcs,
    })
}

fn execute(cli: Cli, shell: &Shell) -> Result<u8> {
    let global = cli.global;
    let config = Config::from_env(shell.clone())?.with_offline(global.offline).with_refresh(global.refresh);

    match cli.command {
        Command::New { path, args } => {
            if path.exists() && path.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
                bail!("destination `{}` already exists; use `kargo init` to initialize an existing directory", path.display());
            }
            // `create` validates the options before it creates the directory, so a bad name leaves nothing behind.
            let opts = new_options(&path, args)?;
            let kind = if opts.lib { "library" } else { "application" };
            ops::new::create(&path, &opts)?;
            shell.status("Created", format!("{kind} `{}` package", opts.name));
            Ok(0)
        }
        Command::Init { path, args } => {
            let dir = match path {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            let opts = new_options(&dir, args)?;
            ops::new::create(&dir, &opts)?;
            shell.status("Created", format!("{} `{}` package", if opts.lib { "library" } else { "application" }, opts.name));
            Ok(0)
        }
        Command::Build { build, keep_going } => {
            let ws = load_workspace(&global)?;
            let opts = BuildOptions { keep_going, ..build.options() };
            let ctx = context(&config, &ws, &global, opts.profile())?;
            ops::build::build(&ctx, &opts)?;
            Ok(0)
        }
        Command::Check { build, tests, keep_going } => {
            let ws = load_workspace(&global)?;
            let opts = CheckOptions { build: BuildOptions { keep_going, ..build.options() }, tests };
            let ctx = context(&config, &ws, &global, opts.build.profile())?;
            ops::check::check(&ctx, &opts)?;
            Ok(0)
        }
        Command::Run { release, target, args } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, profile(release))?;
            let code = ops::run::run(&ctx, &RunOptions { target: target.map(Into::into), release, args })?;
            Ok(exit_code(code))
        }
        Command::Test { build, filter, no_run, no_fail_fast } => {
            let ws = load_workspace(&global)?;
            let opts = TestOptions { build: build.options(), filter, no_run, no_fail_fast };
            let ctx = context(&config, &ws, &global, opts.build.profile())?;
            if ops::test::test(&ctx, &opts)? {
                Ok(0)
            } else {
                shell.error("test failed");
                Ok(FAILURE)
            }
        }
        Command::Doc { targets, open } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            let targets = targets.into_iter().map(Into::into).collect();
            ops::doc::doc(&ctx, &DocOptions { targets, open })?;
            Ok(0)
        }
        Command::Fmt { check } => {
            let ws = load_workspace(&global)?;
            note_shadowed_task(shell, &ws, "fmt");
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            if ops::fmt::fmt(&ctx, &FmtOptions { check })? {
                Ok(0)
            } else if check {
                shell.error("formatting check failed; `kargo fmt` fixes what ktlint can correct automatically");
                Ok(FAILURE)
            } else {
                shell.error("ktlint could not fix every problem; fix the ones listed above by hand");
                Ok(FAILURE)
            }
        }
        Command::Lint { create_baseline } => {
            let ws = load_workspace(&global)?;
            note_shadowed_task(shell, &ws, "lint");
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            if ops::lint::lint(&ctx, &LintOptions { create_baseline })? {
                Ok(0)
            } else {
                shell.error(
                    "detekt found issues; fix them, suppress them with `@Suppress`, or record them with `kargo lint --create-baseline`",
                );
                Ok(FAILURE)
            }
        }
        Command::Add { coordinates, dev, target, scope, bom, pre, no_verify, frameworks } => {
            let ws = load_workspace(&global)?;
            if !frameworks.is_empty() {
                ops::add::add_frameworks(&config, &ws, &frameworks)?;
            }
            if !coordinates.is_empty() {
                let opts = ops::add::AddOptions {
                    specs: coordinates,
                    section: section(dev, target),
                    scope: scope.map(Into::into),
                    bom,
                    prerelease: pre,
                    no_verify,
                };
                ops::add::add(&config, &ws, &opts)?;
            }
            Ok(0)
        }
        Command::Remove { names, dev, target } => {
            let ws = load_workspace(&global)?;
            ops::add::remove(&config, &ws, &names, section(dev, target))?;
            Ok(0)
        }
        Command::Tree { target, configuration, depth, duplicates } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            let targets = match target {
                Some(t) => vec![t.into()],
                None => ws.manifest().targets.enabled().into_iter().filter(|t| *t != TargetKind::Android).collect(),
            };
            let configuration = match configuration {
                ConfigurationArg::Compile => Configuration::Compile,
                ConfigurationArg::Runtime => Configuration::Runtime,
                ConfigurationArg::Test => Configuration::Test,
            };
            let pkg = &ws.manifest().package;
            for t in targets {
                for (label, res) in ops::resolution_for(&ctx, t, configuration)? {
                    if duplicates {
                        for line in ops::tree::conflicts(&res)? {
                            println!("{line}");
                        }
                    } else {
                        print!("{}", ops::tree::render(&format!("{} v{} ({label})", pkg.name, pkg.version), &res, depth));
                    }
                }
            }
            Ok(0)
        }
        Command::Fetch { targets } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            ops::fetch(&ctx, &targets)?;
            Ok(0)
        }
        Command::Update => {
            let ws = load_workspace(&global)?;
            let config = config.with_refresh(true);
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            ops::update(&ctx)?;
            shell.status("Updated", "kargo.lock");
            Ok(0)
        }
        Command::Clean { release } => {
            let ws = load_workspace(&global)?;
            let removed = ops::clean(&ws, release.then_some(ProfileKind::Release))?;
            shell.status("Removed", removed.display());
            Ok(0)
        }
        Command::Package { release } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, profile(release))?;
            let jar = ops::package(&ctx)?;
            println!("{}", jar.display());
            Ok(0)
        }
        Command::Task { name, list: _, args } => {
            let ws = load_workspace(&global)?;
            match name {
                None => {
                    list_tasks(&ws);
                    Ok(0)
                }
                Some(name) => {
                    let env = kargo_ops::plugins::base_env(&ws, "dev");
                    kargo_ops::tasks::run(shell, ws.root(), &ws.manifest().tasks, &name, &args, &env)?;
                    Ok(0)
                }
            }
        }
        Command::Plugins => {
            list_plugins(&config, &global);
            Ok(0)
        }
        Command::Android { args } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, ProfileKind::Dev)?;
            let ok = kargo_ops::gradle::gradle(&ctx, &args)?;
            Ok(if ok { 0 } else { FAILURE })
        }
        Command::Info => {
            let ws = load_workspace(&global)?;
            print_info(&ws);
            Ok(0)
        }
        Command::Completions { shell } => {
            complete::write_script(shell, &mut std::io::stdout().lock())?;
            Ok(0)
        }
        Command::Toolchain { command } => toolchain(&config, &global, command),
        Command::Daemon { command } => {
            match command {
                DaemonCommand::Status => {
                    let running = kargo_compiler::daemon::running(config.home());
                    if running.is_empty() {
                        println!("No compiler daemons are running.");
                    }
                    for d in running {
                        println!("pid {:<8} {}", d.pid, d.dir.display());
                    }
                }
                DaemonCommand::Stop => {
                    let stopped = kargo_compiler::daemon::stop_all(config.home());
                    shell.status("Stopped", format!("{stopped} compiler daemon{}", if stopped == 1 { "" } else { "s" }));
                }
            }
            Ok(0)
        }
        Command::External(argv) => external(&config, &global, argv),
    }
}

/// Built-in commands win over tasks of the same name; say how to reach the task.
fn note_shadowed_task(shell: &Shell, ws: &Workspace, command: &str) {
    if ws.manifest().tasks.contains_key(command) {
        shell.note(format!("`kargo {command}` is a built-in command; run the `{command}` task with `kargo task {command}`"));
    }
}

fn toolchain(config: &Config, global: &GlobalArgs, command: ToolchainCommand) -> Result<u8> {
    // Toolchain commands also work outside a project.
    let ws = load_workspace(global).ok();
    let manifest = ws.as_ref().map(Workspace::manifest);
    let shell = config.shell();
    match command {
        ToolchainCommand::List => print!("{}", toolchain_ops::list(config, manifest)?),
        ToolchainCommand::Install { kind: Some(kind), version } => {
            let what = toolchain_ops::install(config, manifest, kind.into(), version.as_deref())?;
            shell.status("Installed", what);
        }
        ToolchainCommand::Install { kind: None, version: Some(_) } => bail!("specify what to install: `jdk` or `native`"),
        ToolchainCommand::Install { kind: None, version: None } => {
            let manifest = manifest.context("not in a Kargo project; use `kargo toolchain install jdk <version>` or `native <version>`")?;
            let done = toolchain_ops::install_for_project(config, manifest)?;
            if done.is_empty() {
                shell.status("Fresh", "this project uses the system JDK and no Kotlin/Native target");
            }
            for what in done {
                shell.status("Installed", what);
            }
        }
        ToolchainCommand::Remove { kind, version } => {
            for what in toolchain_ops::remove(config, kind.into(), &version)? {
                shell.status("Removed", what);
            }
        }
        ToolchainCommand::Available => print!("{}", toolchain_ops::available(config)?),
        ToolchainCommand::Which => {
            let ws = ws.context("`kargo toolchain which` must run inside a Kargo project")?;
            let ctx = context(config, &ws, global, ProfileKind::Dev)?;
            let m = ws.manifest();
            let jdk = ctx.jdk()?;
            println!("jdk:           {} (JDK {} at {})", m.toolchain.jdk, jdk.version, jdk.home.display());
            println!("kotlin:        {}", m.kotlin.version);
            if m.targets.is_enabled(TargetKind::Native) {
                match kargo_toolchain::NativeToolchain::find(config, &m.kotlin.version)? {
                    Some(t) => println!("kotlin-native: {} at {}", t.version, t.home.display()),
                    None => println!("kotlin-native: {} (not installed yet; installed on first native build)", m.kotlin.version),
                }
            }
        }
    }
    Ok(0)
}

fn list_tasks(ws: &Workspace) {
    let tasks = &ws.manifest().tasks;
    if tasks.is_empty() {
        println!("No tasks defined. Add some under [tasks] in kargo.toml.");
        return;
    }
    println!("Available tasks:");
    let width = tasks.keys().map(String::len).max().unwrap_or(0);
    for task in tasks.values() {
        println!("    {:width$}  {}", task.name, task_description(task));
    }
}

fn list_plugins(config: &Config, global: &GlobalArgs) {
    if let Ok(ws) = load_workspace(global) {
        let m = ws.manifest();
        if !m.compiler_plugins.is_empty() {
            println!("Compiler plugins:");
            for p in &m.compiler_plugins {
                println!("    {:16} {}:{}", p.name, p.artifact.module, p.artifact.version.as_deref().unwrap_or(""));
            }
        }
        if !m.process_plugins.is_empty() {
            println!("Hook plugins:");
            for p in &m.process_plugins {
                let hooks: Vec<&str> = p.hooks.iter().map(|h| h.as_str()).collect();
                println!("    {:16} {} [{}]", p.name, p.platform_command(), hooks.join(", "));
            }
        }
    }
    let subcommands = kargo_ops::plugins::list_subcommands(config);
    if subcommands.is_empty() {
        println!("No `kargo-*` subcommands found (put executables in {} or on PATH).", config.bin_dir().display());
    } else {
        println!("Subcommands:");
        for (name, path) in subcommands {
            println!("    {name:16} {}", path.display());
        }
    }
}

fn print_info(ws: &Workspace) {
    let m = ws.manifest();
    println!("package:      {} v{}", m.package.name, m.package.version);
    if let Some(g) = &m.package.group {
        println!("group:        {g}");
    }
    println!("kotlin:       {}", m.kotlin.version);
    let targets: Vec<&str> = m.targets.enabled().iter().map(|t| t.as_str()).collect();
    println!("targets:      {}", targets.join(", "));
    let fw = &m.frameworks;
    let enabled: Vec<&str> = [
        ("compose", fw.compose.is_some()),
        ("serialization", fw.serialization.is_some()),
        ("coroutines", fw.coroutines.is_some()),
        ("ktor", fw.ktor.is_some()),
        ("lombok", fw.lombok.is_some()),
        ("spring-boot", fw.spring_boot.is_some()),
    ]
    .into_iter()
    .filter_map(|(n, on)| on.then_some(n))
    .collect();
    if !enabled.is_empty() {
        println!("frameworks:   {}", enabled.join(", "));
    }
    println!("dependencies: {} (+{} dev)", m.dependencies.len(), m.dev_dependencies.len());
    let repos: Vec<&str> = m.repositories.iter().map(|r| r.name.as_str()).collect();
    println!("repositories: {}", repos.join(", "));
    println!("root:         {}", ws.root().display());
    println!("target dir:   {}", ws.target_dir().display());
}

/// `kargo <name>`: a task from the manifest, else a `kargo-<name>` executable.
fn external(config: &Config, global: &GlobalArgs, argv: Vec<String>) -> Result<u8> {
    let (name, args) = argv.split_first().context("missing subcommand")?;
    if let Ok(ws) = load_workspace(global) {
        if ws.manifest().tasks.contains_key(name) {
            let env = kargo_ops::plugins::base_env(&ws, "dev");
            kargo_ops::tasks::run(config.shell(), ws.root(), &ws.manifest().tasks, name, args, &env)?;
            return Ok(0);
        }
    }
    let Some(program) = kargo_ops::plugins::find_subcommand(config, name) else {
        bail!("no such command: `{name}`\n\n\tView all installed commands with `kargo --help` and `kargo plugins`");
    };
    let cwd = std::env::current_dir()?;
    let code = kargo_ops::plugins::run_subcommand(config, name, &program, args, &cwd)?;
    Ok(exit_code(code))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_common_invocations() {
        let cli = Cli::try_parse_from(["kargo", "build", "--release", "-t", "jvm", "-t", "native"]).unwrap();
        match cli.command {
            Command::Build { build, keep_going } => {
                let o = build.options();
                assert!(o.release);
                assert!(!keep_going);
                assert_eq!(o.targets, [TargetKind::Jvm, TargetKind::Native]);
            }
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from(["kargo", "run", "--", "--flag", "x"]).unwrap();
        assert!(matches!(cli.command, Command::Run { ref args, .. } if args == &["--flag", "x"]));
        let cli = Cli::try_parse_from(["kargo", "new", "app", "--target", "jvm,wasm", "--with", "serialization,coroutines"]).unwrap();
        assert!(matches!(cli.command, Command::New { ref args, .. } if args.targets.len() == 2 && args.frameworks.len() == 2));
        let cli = Cli::try_parse_from(["kargo", "add", "--framework", "compose"]).unwrap();
        assert!(matches!(cli.command, Command::Add { ref frameworks, .. } if frameworks == &["compose"]));
        assert!(Cli::try_parse_from(["kargo", "add"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "my-plugin", "--x"]).unwrap();
        assert!(matches!(cli.command, Command::External(ref v) if v == &["my-plugin", "--x"]));
        let cli = Cli::try_parse_from(["kargo", "task", "gen", "--watch"]).unwrap();
        assert!(matches!(cli.command, Command::Task { ref args, .. } if args == &["--watch"]));
        let cli = Cli::try_parse_from(["kargo", "toolchain", "install", "jdk", "21"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Toolchain { command: ToolchainCommand::Install { kind: Some(ToolArg::Jdk), ref version } } if version.as_deref() == Some("21")
        ));
        assert!(Cli::try_parse_from(["kargo", "toolchain", "remove", "native"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "-v", "test", "MathTest", "--no-run"]).unwrap();
        assert!(cli.global.verbose);
        assert!(matches!(cli.command, Command::Test { ref filter, no_run: true, .. } if filter.as_deref() == Some("MathTest")));
        let cli = Cli::try_parse_from(["kargo", "c", "--tests", "-r", "-t", "native"]).unwrap();
        assert!(
            matches!(cli.command, Command::Check { ref build, tests: true, keep_going: false } if build.release && build.options().targets == [TargetKind::Native])
        );
        assert!(matches!(Cli::try_parse_from(["kargo", "b", "--keep-going"]).unwrap().command, Command::Build { keep_going: true, .. }));
        assert!(matches!(
            Cli::try_parse_from(["kargo", "check", "--keep-going"]).unwrap().command,
            Command::Check { keep_going: true, .. }
        ));
        assert!(matches!(Cli::try_parse_from(["kargo", "t", "--no-fail-fast"]).unwrap().command, Command::Test { no_fail_fast: true, .. }));
        let cli = Cli::try_parse_from(["kargo", "doc", "--open", "-t", "jvm"]).unwrap();
        assert!(matches!(cli.command, Command::Doc { ref targets, open: true } if matches!(targets[..], [TargetArg::Jvm])));
        let cli = Cli::try_parse_from(["kargo", "fmt", "--check"]).unwrap();
        assert!(matches!(cli.command, Command::Fmt { check: true }));
        let cli = Cli::try_parse_from(["kargo", "lint", "--create-baseline"]).unwrap();
        assert!(matches!(cli.command, Command::Lint { create_baseline: true }));
    }

    #[test]
    fn dependency_sections() {
        assert_eq!(section(true, None), DependencySection::Dev);
        assert_eq!(section(false, Some(TargetArg::Wasm)), DependencySection::Target(TargetKind::Wasm));
        assert_eq!(section(false, None), DependencySection::Main);
    }
}
