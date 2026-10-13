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
use kargo_ops::ops::publish::PublishOptions;
use kargo_ops::ops::run::{Program, RunOptions};
use kargo_ops::ops::test::{ListFormat, TestOptions};
use kargo_ops::ops::toolchain::{self as toolchain_ops, ToolKind};
use kargo_ops::ops::{self, Configuration, new::NewOptions};
use kargo_ops::warnings;
use kargo_ops::workspace::{ProfileKind, Project, Selection, Workspace};
use kargo_toolchain::ServerKind;
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
    /// Run Android builds with the Gradle on PATH instead of a Kargo-managed one
    #[arg(long, global = true, env = "KARGO_SYSTEM_GRADLE")]
    system_gradle: bool,
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
    /// A Java annotation processor (javac -processorpath)
    Processor,
    /// A Kotlin Symbol Processing processor (KSP2, runs before compilation)
    Ksp,
}

impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Compile => Scope::Compile,
            ScopeArg::Runtime => Scope::Runtime,
            ScopeArg::Provided => Scope::Provided,
            ScopeArg::Processor => Scope::Processor,
            ScopeArg::Ksp => Scope::Ksp,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ToolArg {
    /// A Temurin JDK
    Jdk,
    /// A Kotlin/Native distribution
    Native,
    /// A Gradle distribution (runs Android builds)
    Gradle,
    /// JetBrains' Kotlin language server, run by `kargo lsp`
    KotlinLsp,
    /// fwcd's kotlin-language-server, run by `kargo lsp --server kls`
    Kls,
}

impl From<ToolArg> for ToolKind {
    fn from(t: ToolArg) -> Self {
        match t {
            ToolArg::Jdk => ToolKind::Jdk,
            ToolArg::Native => ToolKind::Native,
            ToolArg::Gradle => ToolKind::Gradle,
            ToolArg::KotlinLsp => ToolKind::Server(ServerKind::KotlinLsp),
            ToolArg::Kls => ToolKind::Server(ServerKind::Kls),
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
    /// List installed JDKs, Kotlin/Native and Gradle distributions and Kotlin language servers
    #[command(visible_alias = "ls")]
    List,
    /// Install a toolchain (without arguments: everything this project needs)
    Install {
        #[arg(value_enum)]
        kind: Option<ToolArg>,
        /// JDK: `21` (latest 21) or `21.0.5+11`; Native: a Kotlin version; Gradle: a Gradle version (default: the project's);
        /// kotlin-lsp, kls: a server version (default: the one `kargo lsp` uses)
        version: Option<String>,
    },
    /// Remove an installed toolchain
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(value_enum)]
        kind: ToolArg,
        /// JDK: a major version (removes all its releases) or a release; Native, Gradle, kotlin-lsp, kls: a version
        #[arg(add = ArgValueCandidates::new(complete::installed_toolchains))]
        version: String,
    },
    /// Show JDK versions available for download
    Available,
    /// Show which toolchains this project uses
    Which,
}

#[derive(Debug, Subcommand)]
enum TargetCommand {
    /// Enable targets: add [target.<name>] to kargo.toml and scaffold the files they need
    Add {
        /// Targets to enable (space- or comma-separated)
        #[arg(value_enum, required = true, value_delimiter = ',')]
        targets: Vec<TargetArg>,
        /// Native and Android: build a library (default: inferred, a `main` function makes an application)
        #[arg(long, conflicts_with = "app")]
        lib: bool,
        /// Native and Android: build an application (default: inferred)
        #[arg(long)]
        app: bool,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Disable targets: remove [target.<name>] and its sub-tables (sources stay on disk)
    #[command(visible_alias = "rm")]
    Remove {
        /// Targets to disable (space- or comma-separated)
        #[arg(value_enum, required = true, value_delimiter = ',')]
        targets: Vec<TargetArg>,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// List the enabled targets and the ones that can be added
    #[command(visible_alias = "ls")]
    List {
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
        #[command(flatten)]
        package: OnePackageArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConfigurationArg {
    Compile,
    Runtime,
    Test,
}

/// `-p` / `--workspace` for commands that work on several packages.
#[derive(Debug, Args, Default)]
struct PackageArgs {
    /// Only this package (repeatable); defaults to the current member, or every member at a workspace root
    #[arg(short, long = "package", value_name = "NAME")]
    packages: Vec<String>,
    /// Every member of the workspace
    #[arg(long, conflicts_with = "packages")]
    workspace: bool,
}

impl PackageArgs {
    fn selection(&self) -> Selection {
        Selection { packages: self.packages.clone(), workspace: self.workspace }
    }
}

/// `-p` for commands that work on one package.
#[derive(Debug, Args, Default)]
struct OnePackageArg {
    /// The package to work on (required at the root of a workspace with several members)
    #[arg(short, long = "package", value_name = "NAME")]
    package: Option<String>,
}

impl OnePackageArg {
    fn selection(&self) -> Selection {
        Selection { packages: self.package.iter().cloned().collect(), workspace: false }
    }
}

/// Output format of `kargo info` and `kargo tree`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum FormatArg {
    #[default]
    Text,
    Json,
}

/// The Kotlin language server behind `kargo lsp`.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum LspServerArg {
    /// JetBrains' Kotlin LSP
    #[default]
    KotlinLsp,
    /// fwcd's kotlin-language-server
    Kls,
    /// No Kotlin server: only kargo.toml support and runnables
    None,
}

/// Output format of `kargo metadata`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum MetadataFormatArg {
    #[default]
    Json,
}

/// The port `--debug` listens on when none is given.
const DEFAULT_DEBUG_PORT: &str = "5005";

/// How compiler diagnostics are printed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum MessageFormatArg {
    /// Compiler output as is, on stderr
    #[default]
    Human,
    /// One JSON object per diagnostic, on stdout
    Json,
}

/// Test report files `kargo test --report` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ReportArg {
    /// JUnit XML in target/<profile>/<target>/test-reports
    Junit,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Build with the release profile
    #[arg(short, long)]
    release: bool,
    /// Only this target (repeatable); defaults to all enabled targets
    #[arg(short, long = "target", value_enum)]
    targets: Vec<TargetArg>,
    #[command(flatten)]
    packages: PackageArgs,
    /// How to print compiler diagnostics
    #[arg(long, value_enum, default_value_t, value_name = "FMT")]
    message_format: MessageFormatArg,
}

impl BuildArgs {
    fn options(&self) -> BuildOptions {
        BuildOptions { targets: self.targets.iter().map(|t| (*t).into()).collect(), release: self.release, keep_going: false }
    }
}

fn set_message_format(format: MessageFormatArg) {
    kargo_compiler::diagnostics::set_json(format == MessageFormatArg::Json);
}

/// With `--message-format json`, ends the output with `{"reason":"build-finished",...}`.
fn build_finished<T>(format: MessageFormatArg, result: Result<T>) -> Result<T> {
    if format == MessageFormatArg::Json {
        kargo_compiler::diagnostics::emit(&serde_json::json!({ "reason": "build-finished", "success": result.is_ok() }));
    }
    result
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
        /// Run this main instead: a JVM class (com.example.ToolKt) or a native entry function (com.example.main)
        #[arg(long, visible_alias = "entry-point", value_name = "NAME", conflicts_with_all = ["bin", "example"])]
        main: Option<String>,
        /// Run the [[bin]] with this name
        #[arg(long, value_name = "NAME", conflicts_with = "example", add = ArgValueCandidates::new(complete::bins))]
        bin: Option<String>,
        /// Run examples/<NAME>.kt
        #[arg(long, value_name = "NAME", add = ArgValueCandidates::new(complete::examples))]
        example: Option<String>,
        /// Port to serve a Wasm browser app on (default 8080; 0 picks a free one)
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        /// Open the served page of a Wasm browser app in your browser
        #[arg(long)]
        open: bool,
        /// Start the JVM suspended, waiting for a debugger on this port (default 5005)
        #[arg(long, value_name = "PORT", num_args = 0..=1, require_equals = true, default_missing_value = DEFAULT_DEBUG_PORT)]
        debug: Option<u16>,
        #[command(flatten)]
        package: OnePackageArg,
        /// How to print compiler diagnostics
        #[arg(long, value_enum, default_value_t, value_name = "FMT")]
        message_format: MessageFormatArg,
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
        /// Also write test reports: `junit` writes JUnit XML to target/<profile>/<target>/test-reports
        #[arg(long, value_enum, value_name = "FORMAT")]
        report: Option<ReportArg>,
        /// Start the JVM tests suspended, waiting for a debugger on this port (default 5005)
        #[arg(long, value_name = "PORT", num_args = 0..=1, require_equals = true, default_missing_value = DEFAULT_DEBUG_PORT)]
        debug: Option<u16>,
        /// Compile the tests and list them instead of running them (JVM and Native)
        #[arg(long, conflicts_with_all = ["no_run", "report", "debug"])]
        list: bool,
        /// Output format of `--list`; `json` prints one JSON object per test
        #[arg(long, value_enum, default_value_t, value_name = "FMT", requires = "list")]
        format: FormatArg,
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
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Format Kotlin sources with ktlint
    Fmt {
        /// Report unformatted files without changing them; fail if there are any
        #[arg(long)]
        check: bool,
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Analyze Kotlin sources with detekt
    Lint {
        /// Record the current issues in the baseline file instead of reporting them
        #[arg(long)]
        create_baseline: bool,
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Add dependencies or built-in frameworks to kargo.toml
    Add {
        /// Coordinates: group:artifact[:version[:classifier]] (latest release if no version)
        #[arg(required_unless_present_any = ["frameworks", "path"], conflicts_with = "path")]
        coordinates: Vec<String>,
        /// Depend on the Kargo package in this directory, built from source
        #[arg(long, value_name = "DIR", conflicts_with_all = ["bom", "pre", "no_verify", "frameworks"])]
        path: Option<PathBuf>,
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
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Remove dependencies from kargo.toml
    #[command(visible_alias = "rm")]
    Remove {
        /// group:artifact or the manifest key
        #[arg(required = true)]
        names: Vec<String>,
        /// Remove from [dev-dependencies]
        #[arg(long, conflicts_with = "target")]
        dev: bool,
        /// Remove from [target.<target>.dependencies]
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Show dependencies in kargo.toml that have newer versions
    Outdated {
        /// Only these dependencies (group:artifact or the manifest key)
        names: Vec<String>,
        /// Consider pre-release versions
        #[arg(long)]
        pre: bool,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Raise the versions in kargo.toml to the newest compatible releases
    Upgrade {
        /// Only these dependencies (group:artifact or the manifest key)
        names: Vec<String>,
        /// Upgrade to the newest version even if it is not compatible (a new major version)
        #[arg(short, long)]
        incompatible: bool,
        /// Consider pre-release versions
        #[arg(long)]
        pre: bool,
        /// Show what would change without writing kargo.toml
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Enable, disable or list the project's targets (jvm, wasm, native, android)
    Target {
        #[command(subcommand)]
        command: TargetCommand,
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
        #[command(flatten)]
        packages: PackageArgs,
        /// Output format; `json` prints the whole resolved graph
        #[arg(long, value_enum, default_value_t, conflicts_with_all = ["depth", "duplicates"])]
        format: FormatArg,
    },
    /// Download all dependencies and toolchains
    Fetch {
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
        /// Also download the sources jars of the dependencies, for editors
        #[arg(long)]
        sources: bool,
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Re-resolve dependencies and rewrite kargo.lock
    Update {
        /// Only update these locked modules (group:artifact); everything else keeps its locked version
        modules: Vec<String>,
        /// Lock the given module at exactly this version
        #[arg(long, value_name = "VERSION", requires = "modules")]
        precise: Option<String>,
        /// Show what would change without writing kargo.lock
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove build output
    Clean {
        /// Only remove the release profile output
        #[arg(short, long)]
        release: bool,
    },
    /// Build a self-contained executable jar, or a library's jar and sources jar (JVM)
    Package {
        #[arg(short, long)]
        release: bool,
        /// Launch the [[bin]] with this name instead of the main class (writes <NAME>-<version>-all.jar)
        #[arg(long, value_name = "NAME", add = ArgValueCandidates::new(complete::bins))]
        bin: Option<String>,
        /// Build the self-contained <name>-<version>-all.jar even for a library (no main function)
        #[arg(long)]
        fat: bool,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Build a library and publish it to a Maven repository
    #[command(long_about = "Build a library with the release profile and publish it to a Maven repository.

Publishes the jar or klibs of every target (except Android) with a POM, Gradle module metadata,
sources and javadoc jars, checksums and, when signing is on, GnuPG signatures. The destination is
`--repository <name>` (a [repositories] entry, `maven-central` or `maven-local`), `--local`, or
`repository` in [publish].")]
    Publish {
        /// Where to publish: a [repositories] entry, `maven-central` or `maven-local` (default: [publish] repository)
        #[arg(long, value_name = "NAME", conflicts_with = "local")]
        repository: Option<String>,
        /// Publish to the local Maven repository (~/.m2/repository), like `--repository maven-local`
        #[arg(long)]
        local: bool,
        /// Build, stage and sign everything in target/release/publish, but upload nothing
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        packages: PackageArgs,
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
    /// Print a JSON description of the project for editors and tools
    #[command(long_about = "Print a JSON description of the project for editors, language servers and other tools.

Lists every package with its source, test and resource roots, output directories, compiler
arguments and plugins, entry points, tasks and, unless --no-deps is given, the resolved
classpath or klib path of every target and scope. The `version` field is the format version.")]
    Metadata {
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: MetadataFormatArg,
        /// Leave out the resolved dependencies, plugin jars and JDK; nothing is downloaded
        #[arg(long)]
        no_deps: bool,
        /// Describe the release profile instead of the dev profile
        #[arg(short, long)]
        release: bool,
        /// Only print the fingerprint of kargo.toml and kargo.lock (changes when the metadata may have changed)
        #[arg(long, conflicts_with_all = ["no_deps", "release"])]
        fingerprint: bool,
    },
    /// Run the language server for editors (LSP over stdin and stdout)
    #[command(long_about = "Run the language server for editors: LSP over stdin and stdout.

Kargo answers for kargo.toml itself (diagnostics, completion and hover from its schema and
Kargo's own validation) and the `kargo/runnables` request, and passes everything about Kotlin
code to a Kotlin language server it downloads on first use. It describes the project to that
server from `kargo metadata` (workspace.json for the Kotlin LSP, kls-classpath for
kotlin-language-server) and again whenever kargo.toml or kargo.lock change.")]
    Lsp {
        /// The Kotlin language server: JetBrains' `kotlin-lsp`, fwcd's `kls`, or `none` for kargo.toml support only
        #[arg(long, value_enum, default_value_t)]
        server: LspServerArg,
        /// Run this language server executable instead of the one Kargo downloads
        #[arg(long, value_name = "PATH")]
        server_path: Option<PathBuf>,
        /// Accepted for editors that pass it; Kargo always talks over stdio
        #[arg(long, hide = true)]
        stdio: bool,
    },
    /// Print project information
    Info {
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
    },
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
    if wants_top_level_help(&args)
        && let Some(tasks) = tasks_help()
    {
        cmd = cmd.after_help(format!(
            "{tasks}
{AFTER_HELP}"
        ));
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
    let builtins = builtin_commands();
    let mut out = String::from(
        "Tasks (from kargo.toml, run with `kargo <task>`):
",
    );
    for task in tasks.values() {
        out.push_str(&format!(
            "  {:width$}  {}
",
            task.name,
            task_description(task, &builtins)
        ));
    }
    Some(out)
}

fn task_description(task: &kargo_manifest::Task, builtins: &[String]) -> String {
    let mut desc = task.description.clone().unwrap_or_else(|| task.platform_run().join(" && "));
    if !task.hooks.is_empty() {
        let hooks: Vec<&str> = task.hooks.iter().map(|h| h.as_str()).collect();
        desc.push_str(&format!(" [hooks: {}]", hooks.join(", ")));
    }
    if builtins.contains(&task.name) {
        desc.push_str(&format!(" [hidden by `kargo {0}`; run it with `kargo task {0}`]", task.name));
    }
    desc
}

/// The names `kargo <name>` runs as a built-in command, aliases included. A task of the same
/// name can only be run with `kargo task <name>`.
fn builtin_commands() -> Vec<String> {
    let mut names = vec!["help".to_owned()];
    for command in Cli::command().get_subcommands() {
        names.push(command.get_name().to_owned());
        names.extend(command.get_all_aliases().map(str::to_owned));
    }
    names
}

/// Warns about `kargo.toml` settings that work but are probably mistakes.
fn warn_manifest(shell: &Shell, project: &Project, ws: &Workspace) {
    let builtins = builtin_commands();
    warnings::emit(shell, project, ws, warnings::manifest(ws.manifest(), |name| builtins.iter().any(|b| b == name)));
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

fn load_project(global: &GlobalArgs) -> Result<Project> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    Project::discover(&cwd, global.manifest_path.as_deref())
}

/// The package a single-package command such as `kargo add` works on.
fn load_one(global: &GlobalArgs, package: &OnePackageArg, command: &str) -> Result<Workspace> {
    let project = load_project(global)?;
    let index = project.select_one(&package.selection(), command)?;
    Ok(project.package(index).clone())
}

fn context<'a>(
    config: &'a Config,
    project: &'a Project,
    package: usize,
    global: &GlobalArgs,
    profile: ProfileKind,
) -> Result<BuildContext<'a>> {
    let mut ctx = BuildContext::new(config, project, package, profile)?;
    ctx.locked = global.locked;
    Ok(ctx)
}

/// Runs `op` for every selected package in turn, sharing one lockfile and the toolchains.
fn for_each_package<T>(
    config: &Config,
    project: &Project,
    selection: &Selection,
    global: &GlobalArgs,
    profile: ProfileKind,
    mut op: impl FnMut(&BuildContext) -> Result<T>,
) -> Result<Vec<T>> {
    let selected = project.select(selection)?;
    let ctx = context(config, project, selected[0], global, profile)?;
    let mut results = Vec::new();
    for &index in &selected {
        warn_manifest(config.shell(), project, project.package(index));
        results.push(op(&ctx.for_package(index))?);
    }
    Ok(results)
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
    let config =
        Config::from_env(shell.clone())?.with_offline(global.offline).with_refresh(global.refresh).with_system_gradle(global.system_gradle);

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
            set_message_format(build.message_format);
            let project = load_project(&global)?;
            let opts = BuildOptions { keep_going, ..build.options() };
            let results = for_each_package(&config, &project, &build.packages.selection(), &global, opts.profile(), |ctx| {
                ops::build::build(ctx, &opts)
            });
            build_finished(build.message_format, results.map(|_| ()))?;
            Ok(0)
        }
        Command::Check { build, tests, keep_going } => {
            set_message_format(build.message_format);
            let project = load_project(&global)?;
            let opts = CheckOptions { build: BuildOptions { keep_going, ..build.options() }, tests };
            let results = for_each_package(&config, &project, &build.packages.selection(), &global, opts.build.profile(), |ctx| {
                ops::check::check(ctx, &opts)
            });
            build_finished(build.message_format, results.map(|_| ()))?;
            Ok(0)
        }
        Command::Run { release, target, main, bin, example, port, open, debug, message_format, package, args } => {
            set_message_format(message_format);
            let project = load_project(&global)?;
            let index = project.select_one(&package.selection(), "`kargo run`")?;
            warn_manifest(shell, &project, project.package(index));
            let ctx = context(&config, &project, index, &global, profile(release))?;
            let program = match (main, bin, example) {
                (Some(main), _, _) => Program::Main(main),
                (_, Some(bin), _) => Program::Bin(bin),
                (_, _, Some(example)) => Program::Example(example),
                _ => Program::Default,
            };
            let code = ops::run::run(&ctx, &RunOptions { target: target.map(Into::into), release, program, args, port, open, debug })?;
            Ok(exit_code(code))
        }
        Command::Test { build, filter, no_run, no_fail_fast, report, debug, list, format } => {
            set_message_format(build.message_format);
            let project = load_project(&global)?;
            let junit_report = report == Some(ReportArg::Junit);
            let list = list.then_some(if format == FormatArg::Json { ListFormat::Json } else { ListFormat::Text });
            let opts = TestOptions { build: build.options(), filter, no_run, no_fail_fast, junit_report, debug, list };
            let passed = for_each_package(&config, &project, &build.packages.selection(), &global, opts.build.profile(), |ctx| {
                ops::test::test(ctx, &opts)
            })?;
            if passed.iter().all(|ok| *ok) {
                Ok(0)
            } else {
                shell.error("test failed");
                Ok(FAILURE)
            }
        }
        Command::Doc { targets, open, packages } => {
            let project = load_project(&global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let mut open = open;
            for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                // Only the first package's documentation opens in the browser.
                let opts = DocOptions { targets: targets.clone(), open: std::mem::take(&mut open) };
                ops::doc::doc(ctx, &opts)
            })?;
            Ok(0)
        }
        Command::Fmt { check, packages } => {
            let project = load_project(&global)?;
            let ok = for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                ops::fmt::fmt(ctx, &FmtOptions { check })
            })?;
            if ok.iter().all(|ok| *ok) {
                Ok(0)
            } else if check {
                shell.error("formatting check failed; `kargo fmt` fixes what ktlint can correct automatically");
                Ok(FAILURE)
            } else {
                shell.error("ktlint could not fix every problem; fix the ones listed above by hand");
                Ok(FAILURE)
            }
        }
        Command::Lint { create_baseline, packages } => {
            let project = load_project(&global)?;
            let ok = for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                ops::lint::lint(ctx, &LintOptions { create_baseline })
            })?;
            if ok.iter().all(|ok| *ok) {
                Ok(0)
            } else {
                shell.error(
                    "detekt found issues; fix them, suppress them with `@Suppress`, or record them with `kargo lint --create-baseline`",
                );
                Ok(FAILURE)
            }
        }
        Command::Add { coordinates, path, dev, target, scope, bom, pre, no_verify, frameworks, package } => {
            let ws = load_one(&global, &package, "`kargo add`")?;
            if !frameworks.is_empty() {
                ops::add::add_frameworks(&config, &ws, &frameworks)?;
            }
            if let Some(path) = path {
                let dir = std::env::current_dir().context("failed to read the current directory")?.join(path);
                ops::add::add_path(&config, &ws, &dir, section(dev, target), scope.map(Into::into))?;
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
        Command::Remove { names, dev, target, package } => {
            let ws = load_one(&global, &package, "`kargo remove`")?;
            ops::add::remove(&config, &ws, &names, section(dev, target))?;
            Ok(0)
        }
        Command::Target { command } => target(shell, &global, command),
        Command::Tree { target, configuration, depth, duplicates, packages, format } => {
            let project = load_project(&global)?;
            let configuration = match configuration {
                ConfigurationArg::Compile => Configuration::Compile,
                ConfigurationArg::Runtime => Configuration::Runtime,
                ConfigurationArg::Test => Configuration::Test,
            };
            if format == FormatArg::Json {
                let mut documents = Vec::new();
                for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                    let mut trees = Vec::new();
                    for t in tree_targets(ctx, target) {
                        for (label, res) in ops::resolution_for(ctx, t, configuration)? {
                            trees.push(ops::tree::json(&label, &res));
                        }
                    }
                    let configuration_name = match configuration {
                        Configuration::Compile => "compile",
                        Configuration::Runtime => "runtime",
                        Configuration::Test => "test",
                    };
                    let pkg = &ctx.ws.manifest().package;
                    documents.push(serde_json::json!({
                        "package": { "name": pkg.name, "version": pkg.version },
                        "configuration": configuration_name,
                        "trees": trees,
                    }));
                    Ok(())
                })?;
                // One package prints the same object as before; a workspace prints one per member.
                let output = if documents.len() == 1 { documents.remove(0) } else { serde_json::Value::Array(documents) };
                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(0);
            }
            for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                let targets = tree_targets(ctx, target);
                let pkg = &ctx.ws.manifest().package;
                for t in targets {
                    let locals = ops::tree::path_dependencies(ctx, t, configuration)?;
                    for (label, res) in ops::resolution_for(ctx, t, configuration)? {
                        if duplicates {
                            for line in ops::tree::conflicts(&res)? {
                                println!("{line}");
                            }
                        } else {
                            print!("{}", ops::tree::render(&format!("{} v{} ({label})", pkg.name, pkg.version), &locals, &res, depth));
                        }
                    }
                }
                Ok(())
            })?;
            Ok(0)
        }
        Command::Fetch { targets, sources, packages } => {
            let project = load_project(&global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let found = for_each_package(&config, &project, &packages.selection(), &global, ProfileKind::Dev, |ctx| {
                ops::fetch(ctx, &targets)?;
                if !sources {
                    return Ok(0);
                }
                let selected = ops::build::select_targets(ctx, &targets)?;
                ops::metadata::fetch_sources(ctx, &selected)
            })?;
            if sources {
                let found: usize = found.iter().sum();
                shell.status("Fetched", format!("{found} sources jar{}", if found == 1 { "" } else { "s" }));
            }
            Ok(0)
        }
        Command::Metadata { format: MetadataFormatArg::Json, no_deps, release, fingerprint } => {
            let project = load_project(&global)?;
            if fingerprint {
                println!("{}", ops::metadata::fingerprint(&project));
                return Ok(0);
            }
            let ctx = context(&config, &project, project.members()[0], &global, profile(release))?;
            let doc = ops::metadata::metadata(&ctx, &ops::metadata::MetadataOptions { no_deps })?;
            println!("{}", serde_json::to_string_pretty(&doc)?);
            Ok(0)
        }
        Command::Lsp { server, server_path, stdio: _ } => {
            let server = match server {
                LspServerArg::KotlinLsp => Some(ServerKind::KotlinLsp),
                LspServerArg::Kls => Some(ServerKind::Kls),
                LspServerArg::None => None,
            };
            if server.is_none() && server_path.is_some() {
                bail!("`--server-path` needs a Kotlin language server; use `--server kotlin-lsp` or `--server kls`");
            }
            let opts = kargo_lsp::Options { server, server_path, builtin_commands: builtin_commands() };
            let clean = kargo_lsp::run(config.clone(), opts)?;
            Ok(if clean { 0 } else { 1 })
        }
        Command::Outdated { names, pre, package } => {
            let ws = load_one(&global, &package, "`kargo outdated`")?;
            let rows = ops::upgrade::outdated(&config, &ws, &names, pre)?;
            if rows.is_empty() {
                shell.status("Fresh", "every dependency in kargo.toml is on its newest version");
            } else {
                print!("{}", ops::upgrade::render(&rows));
            }
            Ok(0)
        }
        Command::Upgrade { names, incompatible, pre, dry_run, package } => {
            let ws = load_one(&global, &package, "`kargo upgrade`")?;
            let opts = ops::upgrade::UpgradeOptions { names, incompatible, prerelease: pre, dry_run };
            if ops::upgrade::upgrade(&config, &ws, &opts)?.is_empty() {
                shell.status("Fresh", "no dependency in kargo.toml has a newer version to upgrade to");
            }
            Ok(0)
        }
        Command::Update { modules, precise, dry_run } => {
            let project = load_project(&global)?;
            let config = config.with_refresh(true);
            let mut ctx = context(&config, &project, project.members()[0], &global, ProfileKind::Dev)?;
            let modules = modules
                .iter()
                .map(|m| m.parse().with_context(|| format!("`{m}` is not a `group:artifact` module")))
                .collect::<Result<Vec<_>>>()?;
            let changes = ops::update::update(&mut ctx, &ops::update::UpdateOptions { modules, precise, dry_run })?;
            for change in &changes {
                let (verb, what) = ops::update::describe(change);
                shell.status(verb, what);
            }
            if dry_run {
                shell.warn("not updating kargo.lock due to dry run");
            } else if changes.is_empty() {
                shell.status("Fresh", "kargo.lock is up to date");
            } else {
                shell.status("Updated", "kargo.lock");
            }
            Ok(0)
        }
        Command::Clean { release } => {
            let project = load_project(&global)?;
            let removed = ops::clean(project.package(project.members()[0]), shell, release.then_some(ProfileKind::Release))?;
            shell.status("Removed", removed.display());
            Ok(0)
        }
        Command::Package { release, bin, fat, package } => {
            let project = load_project(&global)?;
            let index = project.select_one(&package.selection(), "`kargo package`")?;
            warn_manifest(shell, &project, project.package(index));
            let ctx = context(&config, &project, index, &global, profile(release))?;
            let jar = ops::package(&ctx, bin.as_deref(), fat)?;
            println!("{}", jar.display());
            Ok(0)
        }
        Command::Publish { repository, local, dry_run, packages } => {
            let project = load_project(&global)?;
            let repository = if local { Some(kargo_maven::repo::MAVEN_LOCAL.to_owned()) } else { repository };
            let opts = PublishOptions { repository, dry_run };
            let mut selected = project.select(&packages.selection())?;
            if packages.workspace || (packages.packages.is_empty() && selected.len() > 1) {
                // Publishing a whole workspace skips the members that opt out; naming one with `-p` is an error.
                selected.retain(|&i| {
                    let keep = project.package(i).manifest().package.publish;
                    if !keep {
                        shell.note(format!("skipping `{}`, which has `publish = false`", project.package(i).name()));
                    }
                    keep
                });
                if selected.is_empty() {
                    bail!("no member of the workspace can be published; they all have `publish = false`");
                }
            }
            let ordered = ops::publish::publish_order(&project, &selected);
            let ctx = context(&config, &project, ordered[0], &global, ProfileKind::Release)?;
            for index in ordered {
                warn_manifest(shell, &project, project.package(index));
                ops::publish::publish(&ctx.for_package(index), &opts)?;
            }
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
            let project = load_project(&global)?;
            let index = project.select_one(&Selection::default(), "`kargo android`")?;
            let ctx = context(&config, &project, index, &global, ProfileKind::Dev)?;
            let ok = kargo_ops::gradle::gradle(&ctx, &args)?;
            Ok(if ok { 0 } else { FAILURE })
        }
        Command::Info { format } => {
            let project = load_project(&global)?;
            match format {
                FormatArg::Text => match project.select_one(&Selection::default(), "`kargo info`") {
                    Ok(index) => print_info(&project, project.package(index)),
                    Err(_) => print_workspace_info(&project),
                },
                FormatArg::Json => match project.select_one(&Selection::default(), "`kargo info`") {
                    Ok(index) => println!("{}", serde_json::to_string_pretty(&info_json(project.package(index)))?),
                    Err(_) => println!("{}", serde_json::to_string_pretty(&workspace_info_json(&project))?),
                },
            }
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

fn toolchain(config: &Config, global: &GlobalArgs, command: ToolchainCommand) -> Result<u8> {
    // Toolchain commands also work outside a project. Packages built together share their
    // toolchains, so the first member stands for a whole workspace.
    let project = load_project(global).ok();
    let ws = project.as_ref().map(|p| p.package(p.current().unwrap_or(p.members()[0])));
    let manifest = ws.map(Workspace::manifest);
    let shell = config.shell();
    match command {
        ToolchainCommand::List => print!("{}", toolchain_ops::list(config, manifest)?),
        ToolchainCommand::Install { kind: Some(kind), version } => {
            let what = toolchain_ops::install(config, manifest, kind.into(), version.as_deref())?;
            shell.status("Installed", what);
        }
        ToolchainCommand::Install { kind: None, version: Some(_) } => bail!("specify what to install: `jdk`, `native` or `gradle`"),
        ToolchainCommand::Install { kind: None, version: None } => {
            let project = project
                .as_ref()
                .context("not in a Kargo project; use `kargo toolchain install jdk <version>`, `native <version>` or `gradle <version>`")?;
            // Members share the JDK and Kotlin version, but any of them may have the Native target.
            let mut done: Vec<String> = Vec::new();
            for &member in project.members() {
                for what in toolchain_ops::install_for_project(config, project.package(member).manifest())? {
                    if !done.contains(&what) {
                        done.push(what);
                    }
                }
            }
            if done.is_empty() {
                shell.status("Fresh", "this project uses the system JDK and no Kotlin/Native or Android target");
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
            let project = project.as_ref().context("`kargo toolchain which` must run inside a Kargo project")?;
            let ws = ws.context("`kargo toolchain which` must run inside a Kargo project")?;
            let ctx = context(config, project, project.current().unwrap_or(project.members()[0]), global, ProfileKind::Dev)?;
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
            if m.targets.is_enabled(TargetKind::Android) {
                println!("gradle:        {}", kargo_ops::gradle::describe_gradle(ws, config)?);
            }
        }
    }
    Ok(0)
}

fn target(shell: &Shell, global: &GlobalArgs, command: TargetCommand) -> Result<u8> {
    let kinds = |targets: Vec<TargetArg>| targets.into_iter().map(TargetKind::from).collect::<Vec<_>>();
    match command {
        TargetCommand::Add { targets, lib, app, package } => {
            let ws = load_one(global, &package, "`kargo target add`")?;
            let lib = if lib {
                Some(true)
            } else if app {
                Some(false)
            } else {
                None
            };
            ops::target::add(shell, &ws, &kinds(targets), lib)?;
        }
        TargetCommand::Remove { targets, package } => {
            let ws = load_one(global, &package, "`kargo target remove`")?;
            ops::target::remove(shell, &ws, &kinds(targets))?;
        }
        TargetCommand::List { format, package } => {
            let ws = load_one(global, &package, "`kargo target list`")?;
            let (enabled, available) = ops::target::list(&ws)?;
            match format {
                FormatArg::Text => print!("{}", ops::target::render(&enabled, &available)),
                FormatArg::Json => println!("{}", serde_json::to_string_pretty(&ops::target::json(&enabled, &available))?),
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
    let builtins = builtin_commands();
    for task in tasks.values() {
        println!("    {:width$}  {}", task.name, task_description(task, &builtins));
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

/// The targets a tree is shown for: the one given, else every enabled target that has a resolution.
fn tree_targets(ctx: &BuildContext, target: Option<TargetArg>) -> Vec<TargetKind> {
    match target {
        Some(t) => vec![t.into()],
        None => ctx.ws.manifest().targets.enabled().into_iter().filter(|t| *t != TargetKind::Android).collect(),
    }
}

/// `kargo info --format json` at the root of a workspace: its members, lockfile and build directory.
fn workspace_info_json(project: &Project) -> serde_json::Value {
    let members: Vec<serde_json::Value> = project
        .members()
        .iter()
        .map(|&i| {
            let pkg = project.package(i);
            serde_json::json!({ "name": pkg.name(), "version": pkg.manifest().package.version, "root": pkg.root().display().to_string() })
        })
        .collect();
    serde_json::json!({
        "workspace": project.root().display().to_string(),
        "members": members,
        "lockfile": project.lockfile_path().display().to_string(),
        "target-dir": project.target_dir().display().to_string(),
    })
}

fn print_workspace_info(project: &Project) {
    let members: Vec<String> =
        project.members().iter().map(|&i| project.package(i)).map(|p| format!("{} v{}", p.name(), p.manifest().package.version)).collect();
    println!("workspace:    {}", project.root().display());
    println!("members:      {}", members.join(", "));
    if let Some(&first) = project.members().first() {
        println!("kotlin:       {}", project.package(first).manifest().kotlin.version);
    }
    println!("lockfile:     {}", project.lockfile_path().display());
    println!("target dir:   {}", project.target_dir().display());
}

fn print_info(project: &Project, ws: &Workspace) {
    let m = ws.manifest();
    println!("package:      {} v{}", m.package.name, m.package.version);
    if project.is_workspace() {
        println!("workspace:    {}", project.root().display());
    }
    if let Some(g) = &m.package.group {
        println!("group:        {g}");
    }
    println!("kotlin:       {}", m.kotlin.version);
    let targets: Vec<&str> = m.targets.enabled().iter().map(|t| t.as_str()).collect();
    println!("targets:      {}", targets.join(", "));
    let enabled = kargo_ops::builtins::enabled_frameworks(m);
    if !enabled.is_empty() {
        println!("frameworks:   {}", enabled.join(", "));
    }
    println!("dependencies: {} (+{} dev)", m.dependencies.len(), m.dev_dependencies.len());
    if !ws.path_dependencies().is_empty() {
        let paths: Vec<String> =
            ws.path_dependencies().iter().map(|(d, i)| format!("{} ({})", d.name, project.package(*i).root().display())).collect();
        println!("path deps:    {}", paths.join(", "));
    }
    let repos: Vec<&str> = m.repositories.iter().map(|r| r.name.as_str()).collect();
    println!("repositories: {}", repos.join(", "));
    println!("root:         {}", ws.root().display());
    println!("target dir:   {}", ws.target_dir().display());
}

/// `kargo info --format json`.
fn info_json(ws: &Workspace) -> serde_json::Value {
    let m = ws.manifest();
    let dependencies = |deps: &[kargo_manifest::Dependency]| -> Vec<serde_json::Value> {
        deps.iter()
            .map(|d| serde_json::json!({ "name": d.name, "module": d.module.to_string(), "version": d.version, "scope": d.scope.as_str() }))
            .collect()
    };
    serde_json::json!({
        "package": {
            "name": m.package.name,
            "version": m.package.version,
            "group": m.package.group,
            "description": m.package.description,
        },
        "kotlin": m.kotlin.version,
        "jdk": m.toolchain.jdk.to_string(),
        "targets": m.targets.enabled().iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        "frameworks": kargo_ops::builtins::enabled_frameworks(m),
        "dependencies": dependencies(&m.dependencies),
        "dev-dependencies": dependencies(&m.dev_dependencies),
        "repositories": m.repositories.iter().map(|r| serde_json::json!({ "name": r.name, "url": r.url })).collect::<Vec<_>>(),
        "bins": m
            .bins
            .iter()
            .map(|b| serde_json::json!({ "name": b.name, "main": b.main, "entry-point": b.entry_point, "path": b.path }))
            .collect::<Vec<_>>(),
        "examples": ws.examples(),
        "root": ws.root().display().to_string(),
        "manifest": ws.manifest_path().display().to_string(),
        "target-dir": ws.target_dir().display().to_string(),
    })
}

/// `kargo <name>`: a task from the manifest, else a `kargo-<name>` executable.
fn external(config: &Config, global: &GlobalArgs, argv: Vec<String>) -> Result<u8> {
    let (name, args) = argv.split_first().context("missing subcommand")?;
    if let Ok(ws) = load_workspace(global)
        && ws.manifest().tasks.contains_key(name)
    {
        let env = kargo_ops::plugins::base_env(&ws, "dev");
        kargo_ops::tasks::run(config.shell(), ws.root(), &ws.manifest().tasks, name, args, &env)?;
        return Ok(0);
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
        let cli = Cli::try_parse_from(["kargo", "run", "--entry-point", "com.x.main"]).unwrap();
        assert!(matches!(cli.command, Command::Run { main: Some(ref m), .. } if m == "com.x.main"));
        let cli = Cli::try_parse_from(["kargo", "run", "--bin", "migrate", "--", "up"]).unwrap();
        assert!(matches!(cli.command, Command::Run { bin: Some(ref b), ref args, .. } if b == "migrate" && args == &["up"]));
        assert!(Cli::try_parse_from(["kargo", "run", "--bin", "a", "--example", "b"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "run", "--main", "A", "--bin", "b"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "package", "--bin", "migrate"]).unwrap();
        assert!(matches!(cli.command, Command::Package { bin: Some(ref b), .. } if b == "migrate"));
        let cli = Cli::try_parse_from(["kargo", "package", "--fat"]).unwrap();
        assert!(matches!(cli.command, Command::Package { fat: true, bin: None, .. }));
        let cli = Cli::try_parse_from(["kargo", "publish", "--repository", "company", "--dry-run", "--workspace"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Publish { repository: Some(ref r), local: false, dry_run: true, ref packages } if r == "company" && packages.workspace
        ));
        assert!(Cli::try_parse_from(["kargo", "publish", "--local", "--repository", "company"]).is_err());
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
        let cli = Cli::try_parse_from(["kargo", "test", "--list", "--format", "json", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { list: true, format: FormatArg::Json, .. }));
        assert!(Cli::try_parse_from(["kargo", "test", "--format", "json"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "test", "--list", "--no-run"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "test", "--report", "junit", "--message-format", "json"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Test { report: Some(ReportArg::Junit), ref build, .. } if build.message_format == MessageFormatArg::Json
        ));
        assert!(matches!(
            Cli::try_parse_from(["kargo", "info", "--format", "json"]).unwrap().command,
            Command::Info { format: FormatArg::Json }
        ));
        assert!(Cli::try_parse_from(["kargo", "tree", "--format", "json", "--duplicates"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "update", "g:a", "--precise", "1.2", "--dry-run"]).unwrap();
        assert!(
            matches!(cli.command, Command::Update { ref modules, ref precise, dry_run: true } if modules == &["g:a"] && precise.as_deref() == Some("1.2"))
        );
        assert!(Cli::try_parse_from(["kargo", "update", "--precise", "1.2"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "upgrade", "-i", "g:a", "--dry-run"]).unwrap();
        assert!(matches!(cli.command, Command::Upgrade { ref names, incompatible: true, dry_run: true, .. } if names == &["g:a"]));
        let cli = Cli::try_parse_from(["kargo", "rm", "g:a", "-t", "wasm"]).unwrap();
        assert!(matches!(cli.command, Command::Remove { target: Some(TargetArg::Wasm), .. }));
        let cli = Cli::try_parse_from(["kargo", "doc", "--open", "-t", "jvm"]).unwrap();
        assert!(matches!(cli.command, Command::Doc { ref targets, open: true, .. } if matches!(targets[..], [TargetArg::Jvm])));
        let cli = Cli::try_parse_from(["kargo", "fmt", "--check"]).unwrap();
        assert!(matches!(cli.command, Command::Fmt { check: true, .. }));
        let cli = Cli::try_parse_from(["kargo", "target", "add", "wasm", "native,android", "--lib", "-p", "app"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Target { command: TargetCommand::Add { ref targets, lib: true, app: false, ref package } }
                if targets.len() == 3 && package.package.as_deref() == Some("app")
        ));
        assert!(Cli::try_parse_from(["kargo", "target", "add"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "target", "add", "wasm", "--lib", "--app"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "target", "rm", "wasm"]).unwrap();
        assert!(matches!(cli.command, Command::Target { command: TargetCommand::Remove { .. } }));
        let cli = Cli::try_parse_from(["kargo", "target", "ls", "--format", "json"]).unwrap();
        assert!(matches!(cli.command, Command::Target { command: TargetCommand::List { format: FormatArg::Json, .. } }));
        let cli = Cli::try_parse_from(["kargo", "run", "--debug", "--", "x"]).unwrap();
        assert!(matches!(cli.command, Command::Run { debug: Some(5005), ref args, .. } if args == &["x"]));
        let cli = Cli::try_parse_from(["kargo", "test", "--debug=6000", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { debug: Some(6000), ref filter, .. } if filter.as_deref() == Some("MathTest")));
        // `--debug` never takes the next word as its port.
        let cli = Cli::try_parse_from(["kargo", "test", "--debug", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { debug: Some(5005), ref filter, .. } if filter.as_deref() == Some("MathTest")));
        let cli = Cli::try_parse_from(["kargo", "metadata", "--no-deps", "--format", "json"]).unwrap();
        assert!(matches!(cli.command, Command::Metadata { no_deps: true, fingerprint: false, .. }));
        assert!(Cli::try_parse_from(["kargo", "metadata", "--format", "text"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "metadata", "--fingerprint", "--no-deps"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "fetch", "--sources"]).unwrap();
        assert!(matches!(cli.command, Command::Fetch { sources: true, .. }));
        let cli = Cli::try_parse_from(["kargo", "lint", "--create-baseline"]).unwrap();
        assert!(matches!(cli.command, Command::Lint { create_baseline: true, .. }));
    }

    #[test]
    fn parses_package_selection() {
        let cli = Cli::try_parse_from(["kargo", "build", "-p", "app", "--package", "lib"]).unwrap();
        assert!(matches!(cli.command, Command::Build { ref build, .. } if build.packages.selection().packages == ["app", "lib"]));
        let cli = Cli::try_parse_from(["kargo", "test", "--workspace"]).unwrap();
        assert!(matches!(cli.command, Command::Test { ref build, .. } if build.packages.workspace));
        assert!(Cli::try_parse_from(["kargo", "build", "--workspace", "-p", "app"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "run", "-p", "app", "--", "x"]).unwrap();
        assert!(
            matches!(cli.command, Command::Run { ref package, ref args, .. } if package.package.as_deref() == Some("app") && args == &["x"])
        );
        assert!(Cli::try_parse_from(["kargo", "run", "-p", "a", "-p", "b"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "add", "--path", "../lib", "--dev"]).unwrap();
        assert!(matches!(cli.command, Command::Add { ref path, dev: true, .. } if path.as_deref() == Some(Path::new("../lib"))));
        assert!(Cli::try_parse_from(["kargo", "add", "--path", "../lib", "g:a"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "add", "--path", "../lib", "--bom"]).is_err());
    }

    #[test]
    fn dependency_sections() {
        assert_eq!(section(true, None), DependencySection::Dev);
        assert_eq!(section(false, Some(TargetArg::Wasm)), DependencySection::Target(TargetKind::Wasm));
        assert_eq!(section(false, None), DependencySection::Main);
    }
}
