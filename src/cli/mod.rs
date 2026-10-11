//! Command-line interface.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::Config;
use crate::manifest::edit::DependencySection;
use crate::manifest::{Scope, TargetKind};
use crate::ops::build::BuildOptions;
use crate::ops::context::BuildContext;
use crate::ops::run::RunOptions;
use crate::ops::test::TestOptions;
use crate::ops::{self, Configuration, new::NewOptions};
use crate::shell::{Shell, Verbosity};
use crate::workspace::{ProfileKind, Workspace};

/// Exit code for build and test failures (matches Cargo).
const FAILURE: u8 = 101;

#[derive(Debug, Parser)]
#[command(
    name = "kargo",
    version,
    about = "Kotlin's package manager and build tool",
    long_about = "Kargo builds Kotlin projects for the JVM, WebAssembly, Kotlin/Native and Android,\nmanages Maven dependencies and runs tests, configured by a single kargo.toml.",
    after_help = "See `kargo help <command>` for more information on a specific command."
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
    /// Ignore cached metadata and kargo.lock, re-checking repositories
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
        BuildOptions { targets: self.targets.iter().map(|t| (*t).into()).collect(), release: self.release }
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
    #[arg(short = 'w', long = "with", value_delimiter = ',')]
    frameworks: Vec<String>,
    /// Maven group / base package
    #[arg(long, default_value = "com.example")]
    group: String,
    /// Package name (defaults to the directory name)
    #[arg(long)]
    name: Option<String>,
    /// Kotlin version
    #[arg(long, default_value = crate::manifest::DEFAULT_KOTLIN_VERSION)]
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
    Build(BuildArgs),
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
        /// Only run tests whose name contains this string
        filter: Option<String>,
        /// Compile tests but do not run them
        #[arg(long)]
        no_run: bool,
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
        #[arg(short = 'F', long = "framework", value_delimiter = ',', conflicts_with_all = ["dev", "target", "scope", "bom"])]
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
        name: Option<String>,
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
    /// Print project information
    Info,
    /// Run a task or a `kargo-<name>` plugin executable
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Entry point used by `main.rs`.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
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
            let dir = crate::workspace::dunce_canonicalize(dir);
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
            std::fs::create_dir_all(&path)?;
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
        Command::Build(args) => {
            let ws = load_workspace(&global)?;
            let opts = args.options();
            let ctx = context(&config, &ws, &global, opts.profile())?;
            ops::build::build(&ctx, &opts)?;
            Ok(0)
        }
        Command::Run { release, target, args } => {
            let ws = load_workspace(&global)?;
            let ctx = context(&config, &ws, &global, profile(release))?;
            let code = ops::run::run(&ctx, &RunOptions { target: target.map(Into::into), release, args })?;
            Ok(exit_code(code))
        }
        Command::Test { build, filter, no_run } => {
            let ws = load_workspace(&global)?;
            let opts = TestOptions { build: build.options(), filter, no_run };
            let ctx = context(&config, &ws, &global, opts.build.profile())?;
            if ops::test::test(&ctx, &opts)? {
                Ok(0)
            } else {
                shell.error("test failed");
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
        Command::Task { name, args } => {
            let ws = load_workspace(&global)?;
            match name {
                None => {
                    list_tasks(&ws);
                    Ok(0)
                }
                Some(name) => {
                    let env = crate::plugins::base_env(&ws, "dev");
                    crate::tasks::run(shell, ws.root(), &ws.manifest().tasks, &name, &args, &env)?;
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
            let ok = crate::gradle::gradle(&ctx, &args)?;
            Ok(if ok { 0 } else { FAILURE })
        }
        Command::Info => {
            let ws = load_workspace(&global)?;
            print_info(&ws);
            Ok(0)
        }
        Command::External(argv) => external(&config, &global, argv),
    }
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
        let mut desc = task.description.clone().unwrap_or_else(|| task.run.join(" && "));
        if !task.hooks.is_empty() {
            let hooks: Vec<&str> = task.hooks.iter().map(|h| h.as_str()).collect();
            desc.push_str(&format!(" [hooks: {}]", hooks.join(", ")));
        }
        println!("    {:width$}  {desc}", task.name);
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
                println!("    {:16} {} [{}]", p.name, p.command, hooks.join(", "));
            }
        }
    }
    let subcommands = crate::plugins::list_subcommands(config);
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
            let env = crate::plugins::base_env(&ws, "dev");
            crate::tasks::run(config.shell(), ws.root(), &ws.manifest().tasks, name, args, &env)?;
            return Ok(0);
        }
    }
    let Some(program) = crate::plugins::find_subcommand(config, name) else {
        bail!("no such command: `{name}`\n\n\tView all installed commands with `kargo --help` and `kargo plugins`");
    };
    let cwd = std::env::current_dir()?;
    let code = crate::plugins::run_subcommand(config, name, &program, args, &cwd)?;
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
            Command::Build(args) => {
                let o = args.options();
                assert!(o.release);
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
        let cli = Cli::try_parse_from(["kargo", "-v", "test", "MathTest", "--no-run"]).unwrap();
        assert!(cli.global.verbose);
        assert!(matches!(cli.command, Command::Test { ref filter, no_run: true, .. } if filter.as_deref() == Some("MathTest")));
    }

    #[test]
    fn dependency_sections() {
        assert_eq!(section(true, None), DependencySection::Dev);
        assert_eq!(section(false, Some(TargetArg::Wasm)), DependencySection::Target(TargetKind::Wasm));
        assert_eq!(section(false, None), DependencySection::Main);
    }
}
