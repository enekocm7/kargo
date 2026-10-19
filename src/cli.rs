//! Command-line interface.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, SubcommandCandidates};

use kargo_manifest::edit::DependencySection;
use kargo_manifest::{Scope, TargetKind};
use kargo_ops::env::EnvOptions;
use kargo_ops::ops::build::BuildOptions;
use kargo_ops::ops::check::CheckOptions;
use kargo_ops::ops::classpath::ClasspathScope;
use kargo_ops::ops::context::BuildContext;
use kargo_ops::ops::doc::DocOptions;
use kargo_ops::ops::fmt::FmtOptions;
use kargo_ops::ops::lint::LintOptions;
use kargo_ops::ops::publish::PublishOptions;
use kargo_ops::ops::run::{Program, RunOptions, Running, Start};
use kargo_ops::ops::test::{ListFormat, TestOptions};
use kargo_ops::ops::toolchain::{self as toolchain_ops, ToolKind};
use kargo_ops::ops::watch::{WatchOptions, WatchSet};
use kargo_ops::ops::{self, Configuration, PackageFormat, new::NewOptions};
use kargo_ops::warnings;
use kargo_ops::workspace::{ProfileKind, Project, Selection, Workspace};
use kargo_toolchain::ServerKind;
use kargo_util::config::Config;
use kargo_util::shell::{Shell, Verbosity};

use crate::complete::{self, ShellArg};

/// Exit code for build and test failures (matches Cargo).
const FAILURE: u8 = 101;

const AFTER_HELP: &str = "See all commands with `kargo --list`.\nSee `kargo help <command>` for more information on a specific command.";

/// The commands `kargo --help` lists. The others are left out of the short help, but `kargo --list`, `kargo help <command>`
/// and the shell completions still show them.
const COMMON_COMMANDS: &[&str] = &[
    "new",
    "init",
    "build",
    "check",
    "run",
    "test",
    "watch",
    "add",
    "remove",
    "update",
    "fetch",
    "tree",
    "clean",
    "package",
    "publish",
    "fmt",
    "lint",
    "doc",
    "task",
    "toolchain",
];

#[derive(Debug, Parser)]
#[command(
    name = "kargo",
    version,
    about = "Cargo for Kotlin: a build tool and package manager",
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

/// `kargo package --format`.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum PackageFormatArg {
    /// A self-contained jar (a library: its jar and sources jar)
    #[default]
    Jar,
    /// The jars, a Java runtime trimmed by jlink and launchers: runs without an installed Java
    Jlink,
    /// A native executable built by GraalVM
    NativeImage,
    /// An installer built by jpackage: .msi on Windows, .dmg on macOS, .deb on Linux
    Installer,
}

impl From<PackageFormatArg> for PackageFormat {
    fn from(f: PackageFormatArg) -> Self {
        match f {
            PackageFormatArg::Jar => PackageFormat::Jar,
            PackageFormatArg::Jlink => PackageFormat::Jlink,
            PackageFormatArg::NativeImage => PackageFormat::NativeImage,
            PackageFormatArg::Installer => PackageFormat::Installer,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ToolArg {
    /// A Temurin JDK
    Jdk,
    /// A GraalVM (builds native executables)
    Graalvm,
    /// A Kotlin/Native distribution
    Native,
    /// A Gradle distribution (runs Android builds)
    Gradle,
    /// JetBrains' Kotlin language server, run by `kargo lsp`
    KotlinLsp,
    /// fwcd's kotlin-language-server, run by `kargo lsp --server kls`
    Kls,
    /// fwcd's kotlin-debug-adapter, run by `kargo dap`
    KotlinDebugAdapter,
}

impl From<ToolArg> for ToolKind {
    fn from(t: ToolArg) -> Self {
        match t {
            ToolArg::Jdk => ToolKind::Jdk,
            ToolArg::Graalvm => ToolKind::Graalvm,
            ToolArg::Native => ToolKind::Native,
            ToolArg::Gradle => ToolKind::Gradle,
            ToolArg::KotlinLsp => ToolKind::Server(ServerKind::KotlinLsp),
            ToolArg::Kls => ToolKind::Server(ServerKind::Kls),
            ToolArg::KotlinDebugAdapter => ToolKind::Server(ServerKind::Kda),
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
    /// List installed JDKs, GraalVMs, Kotlin/Native and Gradle distributions, Kotlin language servers and the debug adapter
    #[command(visible_alias = "ls")]
    List,
    /// Install a toolchain (without arguments: everything this project needs)
    Install {
        #[arg(value_enum)]
        kind: Option<ToolArg>,
        /// JDK: `21` (latest 21) or `21.0.5+11`; GraalVM: `25` or `oracle-25`; Native: a Kotlin version; Gradle: a Gradle version (default: the project's);
        /// kotlin-lsp, kls, kotlin-debug-adapter: a version (default: the one `kargo lsp` or `kargo dap` uses)
        version: Option<String>,
    },
    /// Remove an installed toolchain
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(value_enum)]
        kind: ToolArg,
        /// JDK: a major version (removes all its releases) or a release; Native, Gradle, kotlin-lsp, kls, kotlin-debug-adapter: a version
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
    /// Leave this member out (repeatable); with `--workspace` or at the workspace root
    #[arg(long, value_name = "NAME", conflicts_with = "packages")]
    exclude: Vec<String>,
}

impl PackageArgs {
    fn selection(&self) -> Selection {
        Selection { packages: self.packages.clone(), workspace: self.workspace, exclude: self.exclude.clone() }
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
        Selection { packages: self.package.iter().cloned().collect(), ..Default::default() }
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
#[derive(Debug, Clone, Copy, ValueEnum)]
enum LspServerArg {
    /// Kargo's own Kotlin server, built into kargo: no download, no JDK
    Kargo,
    /// JetBrains' Kotlin LSP
    KotlinLsp,
    /// fwcd's kotlin-language-server
    Kls,
    /// No Kotlin server: only kargo.toml support and runnables
    None,
}

/// Output format of `kargo classpath`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum ClasspathFormatArg {
    /// One line, the entries joined with the platform's path separator (`:` or `;`)
    #[default]
    Path,
    /// One entry per line
    Lines,
    /// A JSON object with the package, target, platform, scope and entries
    Json,
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

/// Which `.env` files a program's environment is read from.
#[derive(Debug, Args)]
struct EnvArgs {
    /// Read this `.env` file after the standard ones, so it wins over them (repeatable)
    #[arg(long = "env-file", value_name = "PATH", conflicts_with = "no_env_file")]
    env_file: Vec<PathBuf>,
    /// Do not read the `.env` files; the variables in kargo.toml still apply
    #[arg(long)]
    no_env_file: bool,
}

impl EnvArgs {
    fn options(&self) -> EnvOptions {
        EnvOptions { files: self.env_file.clone(), no_files: self.no_env_file }
    }
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Build with the release profile
    #[arg(short, long, conflicts_with = "profile")]
    release: bool,
    /// Build with a custom profile from kargo.toml, such as `staging` (`[profile.staging]`)
    #[arg(long, value_name = "NAME", conflicts_with = "release")]
    profile: Option<String>,
    /// Only this target (repeatable); defaults to all enabled targets
    #[arg(short, long = "target", value_enum)]
    targets: Vec<TargetArg>,
    #[command(flatten)]
    packages: PackageArgs,
    /// How many workspace members to build at the same time (default: the number of CPUs; 1 for `kargo test`)
    #[arg(short, long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    jobs: Option<u32>,
    /// How to print compiler diagnostics
    #[arg(long, value_enum, default_value_t, value_name = "FMT")]
    message_format: MessageFormatArg,
}

impl BuildArgs {
    fn options(&self) -> BuildOptions {
        BuildOptions {
            targets: self.targets.iter().map(|t| (*t).into()).collect(),
            profile: profile_kind(self.release, self.profile.as_deref()),
            ..Default::default()
        }
    }
}

/// Options of `--watch` and of `kargo watch`.
#[derive(Debug, Args)]
struct WatchArgs {
    /// Clear the terminal before each run
    #[arg(long)]
    clear: bool,
    /// Leave out the files matching this gitignore-style pattern (repeatable), on top of .gitignore;
    /// target/ and files whose name starts with a dot are always left out
    #[arg(long, value_name = "GLOB")]
    ignore: Vec<String>,
}

impl WatchArgs {
    fn options(&self) -> WatchOptions {
        WatchOptions { clear: self.clear, ignore: self.ignore.clone() }
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
    /// Create an empty workspace (a kargo.toml with [workspace]) instead of a package
    #[arg(long, conflicts_with_all = ["lib", "targets", "frameworks", "name"])]
    workspace: bool,
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
        /// Also combine the Apple frameworks of `kind = "framework"` into an XCFramework (macOS with Xcode)
        #[arg(long)]
        xcframework: bool,
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
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Run with a custom profile from kargo.toml (`[profile.<name>]`)
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
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
        /// Run on GraalVM with its tracing agent, writing native-image configuration to native-image/
        #[arg(long)]
        native_agent: bool,
        /// Install this Android product flavor (repeatable, one per dimension; default: the first of each)
        #[arg(long = "flavor", value_name = "NAME")]
        flavors: Vec<String>,
        #[command(flatten)]
        package: OnePackageArg,
        /// How to print compiler diagnostics
        #[arg(long, value_enum, default_value_t, value_name = "FMT")]
        message_format: MessageFormatArg,
        /// Rebuild and restart the program when its files change (Ctrl+C stops it)
        #[arg(long, conflicts_with = "debug")]
        watch: bool,
        #[command(flatten)]
        watch_args: WatchArgs,
        #[command(flatten)]
        env: EnvArgs,
        /// Arguments for the program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run a Kotlin script (.kts) or a .kt file without a project
    Script {
        /// The .kts or .kt file to run (not needed with --expr)
        #[arg(required_unless_present = "expr", value_name = "FILE")]
        file: Option<String>,
        /// Run this Kotlin code instead of a file
        #[arg(short, long, value_name = "CODE")]
        expr: Option<String>,
        /// Arguments for the script
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
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
        /// Only run JVM tests with this JUnit `@Tag` (repeatable)
        #[arg(long, value_name = "TAG")]
        include_tag: Vec<String>,
        /// Skip JVM tests with this JUnit `@Tag` (repeatable)
        #[arg(long, value_name = "TAG")]
        exclude_tag: Vec<String>,
        /// Rerun only the JVM test classes that failed in the last run
        #[arg(long, conflicts_with_all = ["list", "no_run"])]
        failed: bool,
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
        /// Run the tests again when the project's files change (Ctrl+C stops it)
        #[arg(long, conflicts_with_all = ["list", "debug"])]
        watch: bool,
        #[command(flatten)]
        watch_args: WatchArgs,
        #[command(flatten)]
        env: EnvArgs,
    },
    /// Run a kargo command again whenever the project's files change
    Watch {
        /// Command to run on each change, such as `-x check` (repeatable). The commands run in order, and the ones after a failing command wait for the next change. Arguments are split on spaces; quotes are not interpreted
        #[arg(short = 'x', long = "exec", value_name = "COMMAND", conflicts_with = "command")]
        exec: Vec<String>,
        /// Command to run on each change, with its arguments, as in `kargo watch test -t jvm` (default: `build`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "COMMAND")]
        command: Vec<String>,
        #[command(flatten)]
        watch_args: WatchArgs,
    },
    /// Show the environment `kargo run` and `kargo test` use, and where each variable comes from
    Env {
        /// Print the values instead of hiding them
        #[arg(long)]
        show_values: bool,
        /// Fail when a required variable is unset or a key of .env.example is not set (for CI)
        #[arg(long)]
        check: bool,
        /// Show the release profile's variables
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Show the variables of a custom profile from kargo.toml
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
        #[command(flatten)]
        env: EnvArgs,
        #[command(flatten)]
        package: OnePackageArg,
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
        #[arg(required_unless_present_any = ["frameworks", "path", "cinterop"], conflicts_with_all = ["path", "cinterop"])]
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
        /// Add the C library NAME: [target.native.cinterop.NAME] and interop/NAME.def
        #[arg(long, value_name = "NAME", conflicts_with_all = ["path", "frameworks", "dev", "target", "scope", "bom"])]
        cinterop: Option<String>,
        /// With --cinterop: a header to bind (repeatable; default NAME.h)
        #[arg(long, value_name = "FILE")]
        header: Vec<String>,
        /// With --cinterop: the pkg-config package that gives the library's compiler and linker flags
        #[arg(long, value_name = "PACKAGE")]
        pkg_config: Option<String>,
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
        /// Also download the tools of `kargo doc` (Dokka), `kargo fmt` (ktlint) and `kargo lint` (detekt)
        #[arg(long)]
        tools: bool,
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
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Only remove the output of a custom profile
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
    },
    /// Build a self-contained executable jar, a runtime image, a native executable or an installer (JVM); APKs and App Bundles (Android)
    #[command(
        long_about = "Build a self-contained executable jar, or a library's jar and sources jar (JVM); APKs and App Bundles (Android).

`--format` picks what a JVM application is shipped as:
  jar           <name>-<version>-all.jar, run with `java -jar` (the default)
  jlink         package/<name>-<version>/ with the jars, a Java runtime trimmed by jlink and
                bin/<name> launchers: runs on machines without Java
  native-image  package/<name>: a native executable built by GraalVM ([toolchain] graalvm)
  installer     an .msi, .dmg or .deb (see [package.installer] type) built by jpackage

jlink, native-image and installer build for the OS and CPU they run on. Their output goes to
target/<profile>/jvm/package/.

`--desktop` packages a Compose Desktop application ([frameworks] compose): an installer for this
OS by default, with Skiko's native library next to the app so macOS signs and notarizes it.
`--desktop --format jar --platforms all` builds a jar that runs on every OS Compose supports."
    )]
    Package {
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Package a custom profile from kargo.toml
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
        /// Target to package: jvm (default when enabled) or android
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Launch the [[bin]] with this name instead of the main class (writes <NAME>-<version>-all.jar)
        #[arg(long, value_name = "NAME", add = ArgValueCandidates::new(complete::bins))]
        bin: Option<String>,
        /// Build the self-contained <name>-<version>-all.jar even for a library (no main function)
        #[arg(long)]
        fat: bool,
        /// What to build for the JVM: jar (default), jlink, native-image or installer (default with --desktop)
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<PackageFormatArg>,
        /// Package the Compose Desktop application: an installer for this OS, or with --format jar a jar
        #[arg(long)]
        desktop: bool,
        /// With --desktop --format jar: the platforms whose Skiko natives the jar carries (all, or e.g. windows-x64,macos-arm64)
        #[arg(long, value_name = "PLATFORMS", value_delimiter = ',', requires = "desktop")]
        platforms: Vec<String>,
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
        #[command(flatten)]
        env: EnvArgs,
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
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Describe a custom profile from kargo.toml instead of the dev profile
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
        /// Only print the fingerprint of kargo.toml and kargo.lock (changes when the metadata may have changed)
        #[arg(long, conflicts_with_all = ["no_deps", "release", "profile"])]
        fingerprint: bool,
    },
    /// Print the resolved classpath of a target, for editors, scripts and tools
    #[command(long_about = "Print the resolved classpath (JVM) or klib path (Wasm, Native) of a target.

Prints the jars the build compiles with, in order: path dependencies (built if needed), then the
resolved Maven artifacts, including the Kotlin stdlib and what frameworks add. The package's own
classes are not part of it. Resolution respects kargo.lock, --locked and --offline. Without
--target, the JVM target is used, or the package's only Wasm or Native target.")]
    Classpath {
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Which classpath: the main code's compile or runtime classpath, or the tests' (with dev-dependencies)
        #[arg(short, long, value_enum, default_value = "compile")]
        scope: ConfigurationArg,
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: ClasspathFormatArg,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Write the project description a Kotlin language server reads: workspace.json or kls-classpath
    #[command(long_about = "Write the project description a Kotlin language server reads, for editors that run
the server themselves instead of `kargo lsp`.

Writes workspace.json for JetBrains' Kotlin LSP, or with --kls a kls-classpath script for fwcd's
kotlin-language-server, from `kargo metadata`. With --zed, writes .zed/tasks.json instead: tasks
for the Zed editor that run, test and debug with Kargo from its run buttons. Kargo keeps the
file up to date: `kargo build`,
`check`, `test`, `fetch`, `add`, `remove`, `update`, `upgrade` and `target` write it again when
kargo.toml or kargo.lock changed. A file Kargo didn't write is never replaced.")]
    Ide {
        /// Write kls-classpath for fwcd's kotlin-language-server instead of workspace.json
        #[arg(long)]
        kls: bool,
        /// Also describe Wasm and Native sources in workspace.json (the Kotlin LSP doesn't read klibs yet)
        #[arg(long, conflicts_with = "kls")]
        klib_targets: bool,
        /// Write .zed/tasks.json for the Zed editor instead: Kargo tasks for its run buttons
        #[arg(long, conflicts_with_all = ["kls", "klib_targets"])]
        zed: bool,
    },
    /// Run the language server for editors (LSP over stdin and stdout)
    #[command(long_about = "Run the language server for editors: LSP over stdin and stdout.

Kargo answers for kargo.toml itself (diagnostics, completion and hover from its schema and
Kargo's own validation) and the `kargo/runnables` request. Kotlin code goes to Kargo's own
Kotlin server by default: built into kargo, it reads your sources as Kargo lays them out and
the standard library's sources, and gives completion, hover, go to definition, references and
rename, with `kargo check` errors on save.

With `--server kotlin-lsp` or `--server kls`, Kotlin code goes to that language server instead,
downloaded on first use. Kargo describes the project to it from `kargo metadata`
(workspace.json for the Kotlin LSP, kls-classpath for kotlin-language-server) and again
whenever kargo.toml or kargo.lock change.")]
    Lsp {
        /// The Kotlin language server: Kargo's own (`kargo`, the default), JetBrains' `kotlin-lsp`, fwcd's `kls`, or `none` for kargo.toml support only
        #[arg(long, value_enum)]
        server: Option<LspServerArg>,
        /// Run this language server executable instead of the one Kargo downloads (`kotlin-lsp` unless `--server kls`)
        #[arg(long, value_name = "PATH")]
        server_path: Option<PathBuf>,
        /// Also describe Wasm and Native sources to the Kotlin LSP (it doesn't read klibs yet, so their libraries don't resolve)
        #[arg(long)]
        klib_targets: bool,
        /// Accepted for editors that pass it; Kargo always talks over stdio
        #[arg(long, hide = true)]
        stdio: bool,
    },
    /// Run the debug adapter for editors (DAP over stdin and stdout)
    #[command(long_about = "Run the debug adapter for editors: the Debug Adapter Protocol over stdin and stdout.

Kargo runs fwcd's kotlin-debug-adapter on the project's JDK (downloading it on first use) and
passes the editor's messages through. A `launch` request runs the Kargo command line in its
`args`, `run` or `test`, with --debug and attaches once the JVM waits for the debugger; an
`attach` request attaches to a program started with `kargo run --debug`. Kargo maps its source
layout for the adapter, so breakpoints and stack frames find your files.")]
    Dap {
        /// Run this debug adapter executable instead of the one Kargo downloads
        #[arg(long, value_name = "PATH")]
        adapter_path: Option<PathBuf>,
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

/// The stack size of the thread commands run on.
const STACK_SIZE: usize = 16 * 1024 * 1024;

/// Entry point used by `main.rs`.
pub fn main() -> ExitCode {
    // Debug builds of `execute` and clap's command builder need more stack than the 1 MiB
    // main thread on Windows has.
    match std::thread::Builder::new().name("kargo".into()).stack_size(STACK_SIZE).spawn(run) {
        Ok(thread) => thread.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        Err(_) => run(),
    }
}

fn run() -> ExitCode {
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

/// Parses the command line. `kargo --help` lists the common commands and the current project's tasks, and `kargo` alone
/// shows that same help.
fn parse() -> Cli {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if wants_builtin_list(&args) {
        print_builtin_commands();
        std::process::exit(0);
    }
    if !has_command(&args) {
        // Without a command clap would report a missing subcommand. Asking for help instead prints exactly what `--help` does.
        args.push("--help".into());
    }
    let mut cmd = Cli::command();
    if wants_top_level_help(&args) {
        cmd = hide_uncommon_commands(cmd);
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

/// Whether the arguments name a command, a task or a plugin, or ask for help with `-h` or `--help`. Global flags such as
/// `-q` don't count.
fn has_command(args: &[std::ffi::OsString]) -> bool {
    args.iter().skip(1).filter_map(|a| a.to_str()).any(|a| !a.starts_with('-') || matches!(a, "-h" | "--help"))
}

/// `kargo --list`, possibly with other global flags. With a command or a value after it, clap reports `--list` as unexpected.
fn wants_builtin_list(args: &[std::ffi::OsString]) -> bool {
    let words: Vec<&str> = args.iter().skip(1).filter_map(|a| a.to_str()).collect();
    words.contains(&"--list") && words.iter().all(|w| w.starts_with('-'))
}

/// `kargo --list`: every built-in command with its one-line description, laid out like `cargo --list`.
fn print_builtin_commands() {
    let cmd = Cli::command();
    let commands: Vec<(&str, String)> =
        cmd.get_subcommands().map(|c| (c.get_name(), c.get_about().map(ToString::to_string).unwrap_or_default())).collect();
    let width = commands.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    println!("Built-in commands:");
    for (name, about) in commands {
        println!("    {name:width$}  {about}");
    }
}

/// Hides the commands that `kargo --help` leaves out. Only the short help hides them: `Cli::command()` keeps
/// them for `kargo --list`, `kargo help <command>`, the completions and the task-shadowing check.
fn hide_uncommon_commands(mut cmd: clap::Command) -> clap::Command {
    let uncommon: Vec<String> =
        cmd.get_subcommands().map(|c| c.get_name().to_owned()).filter(|name| !COMMON_COMMANDS.contains(&name.as_str())).collect();
    for name in uncommon {
        cmd = cmd.mut_subcommand(name, |c| c.hide(true));
    }
    cmd
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

/// Writes the files `kargo lsp` or `kargo ide` generated for a Kotlin language server, and the
/// Zed tasks `kargo ide --zed` wrote, again if kargo.toml or kargo.lock changed since. A failure is only a warning: the command succeeded.
fn refresh_editor_files(config: &Config, global: &GlobalArgs, shell: &Shell) {
    let Ok(project) = load_project(global) else { return };
    match kargo_lsp::zed::refresh(config, project.root()) {
        Some(Ok(written)) => report_editor_files(shell, project.root(), &written),
        Some(Err(error)) => shell.warn(format!("could not update .zed/tasks.json: {error:#}")),
        None => {}
    }
    let Some(outcome) = kargo_lsp::project::refresh(config, project.root()) else { return };
    report_editor_files(shell, project.root(), &outcome.written);
    if let Some(error) = outcome.error {
        shell.warn(format!("could not update the project description for the Kotlin language server: {error}"));
    }
}

/// Reports what an import for a language server wrote, with paths relative to the project root.
fn report_editor_files(shell: &Shell, root: &Path, written: &kargo_lsp::project::Written) {
    let shown = |path: &Path| path.strip_prefix(root).unwrap_or(path).display().to_string();
    for path in &written.changed {
        shell.status("Wrote", shown(path));
    }
    for path in &written.foreign {
        shell.warn(format!("{} was not written by Kargo, so it was left alone; delete it to let Kargo generate it", shown(path)));
    }
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
    for &index in &project.dependency_order(&selected, false) {
        warn_manifest(config.shell(), project, project.package(index));
        results.push(op(&ctx.for_package(index))?);
    }
    Ok(results)
}

/// Runs `op` for every selected package, sharing one lockfile and the toolchains: in dependency
/// order, with up to `jobs` independent packages at the same time. `dev` orders packages after the
/// ones their tests use too. The first failure (in dependency order) is returned; the others are
/// printed.
fn build_packages<T: Send>(
    config: &Config,
    project: &Project,
    selection: &Selection,
    global: &GlobalArgs,
    profile: ProfileKind,
    (jobs, dev): (Option<u32>, bool),
    op: impl Fn(&BuildContext) -> Result<T> + Sync,
) -> Result<Vec<T>> {
    let selected = project.select(selection)?;
    let ctx = context(config, project, selected[0], global, profile)?;
    let order = project.dependency_order(&selected, dev);
    for &index in &order {
        warn_manifest(config.shell(), project, project.package(index));
    }
    let jobs = jobs.map_or_else(kargo_ops::ops::default_jobs, |j| j as usize);
    let results = kargo_ops::ops::build_graph(project, &order, dev, jobs, |index| op(&ctx.for_package(index)));
    let mut out = Vec::new();
    let mut first_error = None;
    for result in results.into_iter().flatten() {
        match result {
            Ok(value) => out.push(value),
            Err(e) if first_error.is_none() => first_error = Some(e),
            Err(e) => report_error(config.shell(), &e),
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// The environment of a package's tasks: its variables from `[env]` and the `.env` files, then Kargo's own.
fn task_env(shell: &Shell, project: &Project, ws: &Workspace, opts: &EnvOptions) -> Result<Vec<(String, String)>> {
    let resolved = kargo_ops::env::resolve(ws, "dev", opts)?;
    warnings::emit(shell, project, ws, resolved.tracked_secret_warnings(ws.root()));
    let mut env = resolved.ensure_set()?.for_child();
    env.extend(kargo_ops::plugins::base_env(ws, "dev"));
    Ok(env)
}

/// Takes `--env-file <path>`, `--env-file=<path>` and `--no-env-file` from the front of a task's
/// arguments, so `kargo deploy --env-file .env.staging` works like `kargo task --env-file
/// .env.staging deploy`. The first other argument, and everything after it, belongs to the task.
fn split_task_args(mut env: EnvOptions, args: &[String]) -> Result<(EnvOptions, &[String])> {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "--no-env-file" => env.no_files = true,
            "--env-file" => {
                let Some(path) = args.get(at + 1) else { bail!("`--env-file` needs a path") };
                env.files.push(PathBuf::from(path));
                at += 1;
            }
            _ => match arg.strip_prefix("--env-file=") {
                Some(path) => env.files.push(PathBuf::from(path)),
                None => break,
            },
        }
        at += 1;
    }
    if env.no_files && !env.files.is_empty() {
        bail!("`--env-file` and `--no-env-file` cannot be used together");
    }
    Ok((env, &args[at..]))
}

/// Loads the project for one round of a watched command. A project that no longer loads is reported, and watching goes on:
/// fixing the manifest is itself a change.
fn reload(global: &GlobalArgs, shell: &Shell) -> Option<Project> {
    load_project(global).map_err(|err| report_error(shell, &err)).ok()
}

/// The `--watch` options of `run` and `test`; `--clear` and `--ignore` have no effect without `--watch`.
fn watch_options(watch: bool, args: &WatchArgs) -> Result<Option<WatchOptions>> {
    if !watch && (args.clear || !args.ignore.is_empty()) {
        bail!("`--clear` and `--ignore` only apply with `--watch`");
    }
    Ok(watch.then(|| args.options()))
}

/// Compiles and runs the tests of the selected packages. `Ok(false)` when a test failed.
fn run_tests(config: &Config, project: &Project, global: &GlobalArgs, build: &BuildArgs, opts: &TestOptions) -> Result<bool> {
    let passed = build_packages(
        config,
        project,
        &build.packages.selection(),
        global,
        opts.build.profile.clone(),
        (build.jobs.or(Some(1)), true),
        |ctx| ops::test::test(ctx, opts),
    )?;
    Ok(passed.iter().all(|ok| *ok))
}

/// One round of `kargo run --watch`: builds the program as the project is now, and starts it in place of the running one
/// when the build succeeds. A failed build leaves the running program alone.
fn restart_program(
    config: &Config,
    project: &Project,
    global: &GlobalArgs,
    package: &OnePackageArg,
    opts: &RunOptions,
    running: &mut Running,
) -> Result<()> {
    let index = project.select_one(&package.selection(), "`kargo run`")?;
    warn_manifest(config.shell(), project, project.package(index));
    let ctx = context(config, project, index, global, opts.profile.clone())?;
    match ops::run::start(&ctx, opts)? {
        Start::Process(launch) => running.replace(*launch, config.shell()),
        // `check_watchable` refused these targets before the first round.
        Start::Finished(_) => bail!("`--watch` cannot run this program"),
    }
}

/// The commands `kargo watch` runs on each change: the one given as arguments, the `-x` ones in order, or `build`.
fn watch_commands(exec: Vec<String>, command: Vec<String>) -> Result<Vec<Vec<String>>> {
    if !command.is_empty() {
        return Ok(vec![command]);
    }
    if exec.is_empty() {
        return Ok(vec![vec!["build".to_owned()]]);
    }
    exec.iter()
        .map(|line| {
            let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
            if words.is_empty() {
                bail!("`-x` needs a command, such as `-x check`");
            }
            Ok(words)
        })
        .collect()
}

/// The global flags `kargo watch` passes on to the commands it runs.
fn forwarded_args(global: &GlobalArgs) -> Vec<String> {
    let mut args = Vec::new();
    if global.verbose {
        args.push("--verbose".to_owned());
    }
    if global.quiet {
        args.push("--quiet".to_owned());
    }
    if global.offline {
        args.push("--offline".to_owned());
    }
    if global.locked {
        args.push("--locked".to_owned());
    }
    if global.refresh {
        args.push("--refresh".to_owned());
    }
    if global.system_gradle {
        args.push("--system-gradle".to_owned());
    }
    if let Some(path) = &global.manifest_path {
        args.push("--manifest-path".to_owned());
        args.push(path.display().to_string());
    }
    args
}

/// Runs each command as its own `kargo` process, in order. The first one that fails ends the round.
fn run_commands(shell: &Shell, exe: &Path, forwarded: &[String], commands: &[Vec<String>]) {
    for command in commands {
        let line = command.join(" ");
        shell.status("Running", format!("`kargo {line}`"));
        match std::process::Command::new(exe).args(forwarded).args(command).status() {
            Ok(status) if status.success() => {}
            Ok(_) => {
                shell.error(format!("`kargo {line}` failed; the commands after it wait for the next change"));
                return;
            }
            Err(err) => {
                shell.error(format!("failed to run `kargo {line}`: {err}"));
                return;
            }
        }
    }
}

/// The profile a command works with: `--release`, `--profile <name>`, or `dev` by default.
fn profile_kind(release: bool, name: Option<&str>) -> ProfileKind {
    match (release, name) {
        (true, _) => ProfileKind::Release,
        (false, Some(name)) => ProfileKind::from_name(name),
        (false, None) => ProfileKind::Dev,
    }
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

/// `kargo new` and `kargo init`: a package in `dir` (added to the members of the workspace it is
/// in, if any), or with `--workspace` an empty workspace.
fn create_package(shell: &Shell, dir: &Path, args: NewArgs) -> Result<u8> {
    if args.workspace {
        ops::new::create_workspace(dir, &args.kotlin, !args.no_vcs)?;
        let name = kargo_ops::workspace::dunce_canonicalize(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        shell.status("Created", format!("workspace `{name}`"));
        return Ok(0);
    }
    // `create` validates the options before it creates the directory, so a bad name leaves nothing behind.
    let opts = new_options(dir, args)?;
    let workspace = ops::new::enclosing_workspace(dir)?;
    // A member shares the workspace's repository.
    let opts = if workspace.is_some() { NewOptions { vcs: false, ..opts } } else { opts };
    let kind = if opts.lib { "library" } else { "application" };
    ops::new::create(dir, &opts)?;
    shell.status("Created", format!("{kind} `{}` package", opts.name));
    if let Some(root) = workspace
        && let Some(member) = ops::new::add_to_workspace(&root, dir)?
    {
        shell.status("Adding", format!("`{member}` to the members of the workspace at {}", root.display()));
    }
    Ok(0)
}

fn execute(cli: Cli, shell: &Shell) -> Result<u8> {
    let global = cli.global;
    let config =
        Config::from_env(shell.clone())?.with_offline(global.offline).with_refresh(global.refresh).with_system_gradle(global.system_gradle);

    // Commands that may change kargo.toml or kargo.lock keep the files for language servers current.
    let refreshes_editor_files = matches!(
        cli.command,
        Command::Build { .. }
            | Command::Check { .. }
            | Command::Test { .. }
            | Command::Fetch { .. }
            | Command::Add { .. }
            | Command::Remove { .. }
            | Command::Update { .. }
            | Command::Upgrade { .. }
            | Command::Target { .. }
    );
    let result = run_command(cli.command, &global, &config, shell);
    if refreshes_editor_files && result.is_ok() {
        refresh_editor_files(&config, &global, shell);
    }
    result
}

fn run_command(command: Command, global: &GlobalArgs, config: &Config, shell: &Shell) -> Result<u8> {
    match command {
        Command::New { path, args } => {
            if path.exists() && path.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
                bail!("destination `{}` already exists; use `kargo init` to initialize an existing directory", path.display());
            }
            create_package(shell, &path, args)
        }
        Command::Init { path, args } => {
            let dir = match path {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            create_package(shell, &dir, args)
        }
        Command::Build { build, keep_going, xcframework } => {
            set_message_format(build.message_format);
            let project = load_project(global)?;
            let opts = BuildOptions { keep_going, xcframework, ..build.options() };
            let results =
                build_packages(config, &project, &build.packages.selection(), global, opts.profile.clone(), (build.jobs, false), |ctx| {
                    ops::build::build(ctx, &opts)
                });
            build_finished(build.message_format, results.map(|_| ()))?;
            Ok(0)
        }
        Command::Check { build, tests, keep_going } => {
            set_message_format(build.message_format);
            let project = load_project(global)?;
            let opts = CheckOptions { build: BuildOptions { keep_going, ..build.options() }, tests };
            let results = build_packages(
                config,
                &project,
                &build.packages.selection(),
                global,
                opts.build.profile.clone(),
                (build.jobs, tests),
                |ctx| ops::check::check(ctx, &opts),
            );
            build_finished(build.message_format, results.map(|_| ()))?;
            Ok(0)
        }
        Command::Run {
            release,
            profile,
            target,
            main,
            bin,
            example,
            port,
            open,
            debug,
            native_agent,
            flavors,
            message_format,
            package,
            env,
            watch,
            watch_args,
            args,
        } => {
            set_message_format(message_format);
            let watch = watch_options(watch, &watch_args)?;
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo run`")?;
            warn_manifest(shell, &project, project.package(index));
            let profile = profile_kind(release, profile.as_deref());
            let ctx = context(config, &project, index, global, profile.clone())?;
            let program = match (main, bin, example) {
                (Some(main), _, _) => Program::Main(main),
                (_, Some(bin), _) => Program::Bin(bin),
                (_, _, Some(example)) => Program::Example(example),
                _ => Program::Default,
            };
            let opts = RunOptions {
                target: target.map(Into::into),
                profile,
                program,
                args,
                port,
                open,
                debug,
                native_agent,
                flavors,
                env: env.options(),
            };
            let Some(watch) = watch else {
                let code = ops::run::run(&ctx, &opts)?;
                return Ok(exit_code(code));
            };
            ops::run::check_watchable(&ctx, &opts)?;
            drop(ctx);
            let mut running = Running::default();
            ops::watch::repeatedly(shell, &watch, WatchSet::of(&project), || {
                let project = reload(global, shell)?;
                let set = WatchSet::of(&project);
                if let Err(err) = restart_program(config, &project, global, &package, &opts, &mut running) {
                    report_error(shell, &err);
                }
                Some(set)
            })?;
            Ok(0)
        }
        Command::Script { file, expr, mut args } => {
            let source = match (expr, file) {
                // With --expr every positional is an argument, since the file slot is the first one.
                (Some(code), file) => {
                    args.splice(0..0, file);
                    kargo_ops::script::Source::Expression(code)
                }
                (None, Some(file)) => kargo_ops::script::Source::File(file.into()),
                (None, None) => bail!("give a .kts or .kt file to run, or code with --expr"),
            };
            Ok(exit_code(kargo_ops::script::run(config, &source, &args)?))
        }
        Command::Test {
            build,
            filter,
            no_run,
            no_fail_fast,
            include_tag,
            exclude_tag,
            failed,
            report,
            debug,
            list,
            format,
            watch,
            watch_args,
            env,
        } => {
            set_message_format(build.message_format);
            let watch = watch_options(watch, &watch_args)?;
            let project = load_project(global)?;
            let junit_report = report == Some(ReportArg::Junit);
            let list = list.then_some(if format == FormatArg::Json { ListFormat::Json } else { ListFormat::Text });
            let opts = TestOptions {
                build: build.options(),
                filter,
                no_run,
                no_fail_fast,
                junit_report,
                debug,
                list,
                include_tags: include_tag,
                exclude_tags: exclude_tag,
                failed,
                env: env.options(),
            };
            let Some(watch) = watch else {
                return if run_tests(config, &project, global, &build, &opts)? {
                    Ok(0)
                } else {
                    shell.error("test failed");
                    Ok(FAILURE)
                };
            };
            ops::watch::repeatedly(shell, &watch, WatchSet::of(&project), || {
                let project = reload(global, shell)?;
                let set = WatchSet::of(&project);
                match run_tests(config, &project, global, &build, &opts) {
                    Ok(true) => {}
                    Ok(false) => shell.error("test failed"),
                    Err(err) => report_error(shell, &err),
                }
                Some(set)
            })?;
            Ok(0)
        }
        Command::Watch { exec, command, watch_args } => {
            let watch = watch_args.options();
            let commands = watch_commands(exec, command)?;
            let project = load_project(global)?;
            let exe = std::env::current_exe().context("failed to find the kargo executable")?;
            let forwarded = forwarded_args(global);
            ops::watch::repeatedly(shell, &watch, WatchSet::of(&project), || {
                let project = reload(global, shell)?;
                let set = WatchSet::of(&project);
                run_commands(shell, &exe, &forwarded, &commands);
                Some(set)
            })?;
            Ok(0)
        }
        Command::Env { show_values, check, release, profile, env, package } => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo env`")?;
            let profile = profile_kind(release, profile.as_deref());
            ops::env::show(shell, &project, index, profile, &env.options(), show_values, check)?;
            Ok(0)
        }
        Command::Doc { targets, open, packages } => {
            let project = load_project(global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let mut open = open;
            for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                // Only the first package's documentation opens in the browser.
                let opts = DocOptions { targets: targets.clone(), open: std::mem::take(&mut open) };
                ops::doc::doc(ctx, &opts)
            })?;
            Ok(0)
        }
        Command::Fmt { check, packages } => {
            let project = load_project(global)?;
            let ok = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
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
            let project = load_project(global)?;
            let ok = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
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
        Command::Add { coordinates, path, dev, target, scope, bom, pre, no_verify, frameworks, cinterop, header, pkg_config, package } => {
            if cinterop.is_none() && (!header.is_empty() || pkg_config.is_some()) {
                bail!("`--header` and `--pkg-config` describe a C library; use them with `--cinterop <NAME>`");
            }
            let ws = load_one(global, &package, "`kargo add`")?;
            if let Some(name) = cinterop {
                ops::add::add_cinterop(config, &ws, &name, &header, pkg_config.as_deref())?;
            }
            if !frameworks.is_empty() {
                ops::add::add_frameworks(config, &ws, &frameworks)?;
            }
            if let Some(path) = path {
                let dir = std::env::current_dir().context("failed to read the current directory")?.join(path);
                ops::add::add_path(config, &ws, &dir, section(dev, target), scope.map(Into::into))?;
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
                ops::add::add(config, &ws, &opts)?;
            }
            Ok(0)
        }
        Command::Remove { names, dev, target, package } => {
            let ws = load_one(global, &package, "`kargo remove`")?;
            ops::add::remove(config, &ws, &names, section(dev, target))?;
            Ok(0)
        }
        Command::Target { command } => target(shell, global, command),
        Command::Tree { target, configuration, depth, duplicates, packages, format } => {
            let project = load_project(global)?;
            let configuration = match configuration {
                ConfigurationArg::Compile => Configuration::Compile,
                ConfigurationArg::Runtime => Configuration::Runtime,
                ConfigurationArg::Test => Configuration::Test,
            };
            if format == FormatArg::Json {
                let mut documents = Vec::new();
                for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
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
            for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
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
        Command::Fetch { targets, sources, tools, packages } => {
            let project = load_project(global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let found = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                ops::fetch(ctx, &targets, tools)?;
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
        Command::Metadata { format: MetadataFormatArg::Json, no_deps, release, profile, fingerprint } => {
            let project = load_project(global)?;
            if fingerprint {
                println!("{}", ops::metadata::fingerprint(&project));
                return Ok(0);
            }
            let ctx = context(config, &project, project.members()[0], global, profile_kind(release, profile.as_deref()))?;
            let doc = ops::metadata::metadata(&ctx, &ops::metadata::MetadataOptions { no_deps })?;
            println!("{}", serde_json::to_string_pretty(&doc)?);
            Ok(0)
        }
        Command::Classpath { target, scope, format, package } => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo classpath`")?;
            let ctx = context(config, &project, index, global, ProfileKind::Dev)?;
            let scope = match scope {
                ConfigurationArg::Compile => ClasspathScope::Compile,
                ConfigurationArg::Runtime => ClasspathScope::Runtime,
                ConfigurationArg::Test => ClasspathScope::Test,
            };
            let classpath = ops::classpath::classpath(&ctx, target.map(Into::into), scope)?;
            match format {
                ClasspathFormatArg::Path => println!("{}", kargo_util::fs::join_paths(&classpath.entries)),
                ClasspathFormatArg::Lines => {
                    for entry in &classpath.entries {
                        println!("{}", entry.display());
                    }
                }
                ClasspathFormatArg::Json => {
                    let doc = serde_json::json!({
                        "package": ctx.ws.name(),
                        "target": classpath.target.as_str(),
                        "platform": classpath.platform,
                        "scope": scope.as_str(),
                        "classpath": classpath.entries.iter().map(|e| e.display().to_string()).collect::<Vec<_>>(),
                    });
                    println!("{}", serde_json::to_string_pretty(&doc)?);
                }
            }
            Ok(0)
        }
        Command::Ide { zed: true, .. } => {
            let project = load_project(global)?;
            let metadata = kargo_lsp::project::metadata(config, project.root(), true)?;
            let written = kargo_lsp::zed::write(&metadata)?;
            report_editor_files(shell, project.root(), &written);
            if written.changed.is_empty() && written.foreign.is_empty() {
                shell.status("Fresh", ".zed/tasks.json is up to date");
            }
            Ok(0)
        }
        Command::Ide { kls, klib_targets, zed: false } => {
            let project = load_project(global)?;
            let kind = if kls { ServerKind::Kls } else { ServerKind::KotlinLsp };
            let targets = if klib_targets { kargo_lsp::project::Targets::All } else { kargo_lsp::project::Targets::Jvm };
            let outcome = kargo_lsp::project::import(config, project.root(), Some(kind), targets);
            report_editor_files(shell, project.root(), &outcome.written);
            if let Some(error) = outcome.error {
                bail!("could not describe the project completely for the {}: {error}", kind.title());
            }
            if outcome.written.changed.is_empty() && outcome.written.foreign.is_empty() {
                let file = if kls { "kls-classpath" } else { "workspace.json" };
                shell.status("Fresh", format!("{file} is up to date"));
            }
            Ok(0)
        }
        Command::Lsp { server, server_path, klib_targets, stdio: _ } => {
            // A server executable is JetBrains' Kotlin LSP unless `--server` says otherwise.
            let server = server.unwrap_or(if server_path.is_some() { LspServerArg::KotlinLsp } else { LspServerArg::Kargo });
            let server = match server {
                LspServerArg::Kargo => kargo_lsp::KotlinServer::Kargo,
                LspServerArg::KotlinLsp => kargo_lsp::KotlinServer::External(ServerKind::KotlinLsp),
                LspServerArg::Kls => kargo_lsp::KotlinServer::External(ServerKind::Kls),
                LspServerArg::None => kargo_lsp::KotlinServer::None,
            };
            if server.external().is_none() && server_path.is_some() {
                bail!("`--server-path` runs a Kotlin language server Kargo downloads; use `--server kotlin-lsp` or `--server kls` with it");
            }
            let targets = if klib_targets { kargo_lsp::project::Targets::All } else { kargo_lsp::project::Targets::Jvm };
            let opts = kargo_lsp::Options { server, server_path, targets, builtin_commands: builtin_commands(), search_url: None };
            let clean = kargo_lsp::run(config.clone(), opts)?;
            Ok(if clean { 0 } else { 1 })
        }
        Command::Dap { adapter_path } => {
            let clean = kargo_lsp::dap::run(config.clone(), kargo_lsp::dap::Options { adapter_path })?;
            Ok(if clean { 0 } else { 1 })
        }
        Command::Outdated { names, pre, package } => {
            let ws = load_one(global, &package, "`kargo outdated`")?;
            let rows = ops::upgrade::outdated(config, &ws, &names, pre)?;
            if rows.is_empty() {
                shell.status("Fresh", "every dependency in kargo.toml is on its newest version");
            } else {
                print!("{}", ops::upgrade::render(&rows));
            }
            Ok(0)
        }
        Command::Upgrade { names, incompatible, pre, dry_run, package } => {
            let ws = load_one(global, &package, "`kargo upgrade`")?;
            let opts = ops::upgrade::UpgradeOptions { names, incompatible, prerelease: pre, dry_run };
            if ops::upgrade::upgrade(config, &ws, &opts)?.is_empty() {
                shell.status("Fresh", "no dependency in kargo.toml has a newer version to upgrade to");
            }
            Ok(0)
        }
        Command::Update { modules, precise, dry_run } => {
            let project = load_project(global)?;
            let config = config.clone().with_refresh(true);
            let mut ctx = context(&config, &project, project.members()[0], global, ProfileKind::Dev)?;
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
        Command::Clean { release, profile } => {
            let project = load_project(global)?;
            // Without a profile, everything under target/ goes.
            let only = (release || profile.is_some()).then(|| profile_kind(release, profile.as_deref()));
            let removed = ops::clean(project.package(project.members()[0]), shell, only)?;
            shell.status("Removed", removed.display());
            Ok(0)
        }
        Command::Package { release, profile, target, bin, fat, format, desktop, platforms, package } => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo package`")?;
            warn_manifest(shell, &project, project.package(index));
            let ctx = context(config, &project, index, global, profile_kind(release, profile.as_deref()))?;
            let opts = ops::PackageOptions { target: target.map(Into::into), bin, fat, format: format.map(Into::into), desktop, platforms };
            for file in ops::package(&ctx, &opts)? {
                println!("{}", file.display());
            }
            Ok(0)
        }
        Command::Publish { repository, local, dry_run, packages } => {
            let project = load_project(global)?;
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
            let ctx = context(config, &project, ordered[0], global, ProfileKind::Release)?;
            for index in ordered {
                warn_manifest(shell, &project, project.package(index));
                ops::publish::publish(&ctx.for_package(index), &opts)?;
            }
            Ok(0)
        }
        Command::Task { env, name, list: _, args } => {
            let project = load_project(global)?;
            match name {
                None => {
                    list_tasks(&project);
                    Ok(0)
                }
                Some(name) => {
                    if !run_task(shell, &project, &name, &args, &env.options())? {
                        bail!("no task named `{name}`; available tasks: {}", task_names(&project).join(", "));
                    }
                    Ok(0)
                }
            }
        }
        Command::Plugins => {
            list_plugins(config, global);
            Ok(0)
        }
        Command::Android { args } => {
            let project = load_project(global)?;
            let index = project.select_one(&Selection::default(), "`kargo android`")?;
            let ctx = context(config, &project, index, global, ProfileKind::Dev)?;
            let ok = kargo_ops::gradle::gradle(&ctx, &args)?;
            Ok(if ok { 0 } else { FAILURE })
        }
        Command::Info { format } => {
            let project = load_project(global)?;
            // At the root of a workspace, the workspace; elsewhere, the package.
            let package = task_package(&project);
            match (format, package) {
                (FormatArg::Text, Some(index)) => print_info(&project, project.package(index)),
                (FormatArg::Text, None) => print_workspace_info(&project),
                (FormatArg::Json, Some(index)) => println!("{}", serde_json::to_string_pretty(&info_json(project.package(index)))?),
                (FormatArg::Json, None) => println!("{}", serde_json::to_string_pretty(&workspace_info_json(&project))?),
            }
            Ok(0)
        }
        Command::Completions { shell } => {
            complete::write_script(shell, &mut std::io::stdout().lock())?;
            Ok(0)
        }
        Command::Toolchain { command } => toolchain(config, global, command),
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
        Command::External(argv) => external(config, global, argv),
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
        ToolchainCommand::Install { kind: None, version: Some(_) } => {
            bail!("specify what to install: `jdk`, `graalvm`, `native` or `gradle`")
        }
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
                shell.status("Fresh", "this project uses the system JDK and GraalVM and no Kotlin/Native or Android target");
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
            // Only shown when pinned: most projects never build a native image.
            if m.toolchain.graalvm != kargo_manifest::GraalvmRequest::System {
                match kargo_toolchain::graalvm::find_installed(config, &m.toolchain.graalvm)? {
                    Some(g) => println!(
                        "graalvm:       {} ({} {} at {})",
                        m.toolchain.graalvm,
                        g.distribution.title(),
                        g.version,
                        g.home.display()
                    ),
                    None => println!("graalvm:       {} (not installed yet; installed on first native image)", m.toolchain.graalvm),
                }
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

/// The package whose `[tasks]` a task name refers to: the member the command runs in, or the
/// package of a project that is not a workspace. `None` at the root of a workspace.
fn task_package(project: &Project) -> Option<usize> {
    project.current().or_else(|| (!project.is_workspace()).then(|| project.members()[0]))
}

/// Runs the task `name`: the current package's, else the workspace's (`[workspace.tasks]`, at the
/// workspace root), else, at the root of a workspace, the task of every member that has one (in
/// dependency order, each in its member's directory). Returns `false` when no task has that name.
fn run_task(shell: &Shell, project: &Project, name: &str, args: &[String], env: &EnvOptions) -> Result<bool> {
    let (env, args) = split_task_args(env.clone(), args)?;
    let package = task_package(project);
    if let Some(index) = package {
        let ws = project.package(index);
        if ws.manifest().tasks.contains_key(name) {
            let vars = task_env(shell, project, ws, &env)?;
            kargo_ops::tasks::run(shell, ws.root(), &ws.manifest().tasks, name, args, &vars)?;
            return Ok(true);
        }
    }
    let Some(workspace) = project.workspace_manifest() else { return Ok(false) };
    if workspace.tasks.contains_key(name) {
        let env = kargo_ops::plugins::workspace_env(project);
        kargo_ops::tasks::run(shell, project.root(), &workspace.tasks, name, args, &env)?;
        return Ok(true);
    }
    if package.is_some() {
        return Ok(false);
    }
    let members: Vec<usize> = project
        .dependency_order(project.members(), false)
        .into_iter()
        .filter(|&i| project.package(i).manifest().tasks.contains_key(name))
        .collect();
    for &index in &members {
        let ws = project.package(index);
        let vars = task_env(shell, project, ws, &env)?;
        kargo_ops::tasks::run(shell, ws.root(), &ws.manifest().tasks, name, args, &vars)
            .with_context(|| format!("task `{name}` of `{}` failed", ws.name()))?;
    }
    Ok(!members.is_empty())
}

/// Every task name `kargo <name>` may run here (for suggestions and completion).
fn task_names(project: &Project) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let packages: Vec<usize> = match task_package(project) {
        Some(index) => vec![index],
        None => project.members().to_vec(),
    };
    for index in packages {
        names.extend(project.package(index).manifest().tasks.keys().cloned());
    }
    if let Some(workspace) = project.workspace_manifest() {
        names.extend(workspace.tasks.keys().cloned());
    }
    names.sort();
    names.dedup();
    names
}

fn list_tasks(project: &Project) {
    let builtins = builtin_commands();
    let print = |title: &str, tasks: &std::collections::BTreeMap<String, kargo_manifest::Task>| {
        println!("{title}");
        let width = tasks.keys().map(String::len).max().unwrap_or(0);
        for task in tasks.values() {
            println!("    {:width$}  {}", task.name, task_description(task, &builtins));
        }
    };
    let mut any = false;
    match task_package(project) {
        Some(index) => {
            let tasks = &project.package(index).manifest().tasks;
            if !tasks.is_empty() {
                print("Available tasks:", tasks);
                any = true;
            }
        }
        // At the root of a workspace, each member's tasks run in every member that has them.
        None => {
            for &index in project.members() {
                let ws = project.package(index);
                if !ws.manifest().tasks.is_empty() {
                    print(&format!("Tasks of `{}`:", ws.name()), &ws.manifest().tasks);
                    any = true;
                }
            }
        }
    }
    if let Some(workspace) = project.workspace_manifest()
        && !workspace.tasks.is_empty()
    {
        print("Workspace tasks:", &workspace.tasks);
        any = true;
    }
    if !any {
        println!("No tasks defined. Add some under [tasks] in kargo.toml.");
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
            // The path dependencies it builds on: the module graph.
            let dependencies: Vec<&str> = project.dependencies_of(i, false).into_iter().map(|d| project.package(d).name()).collect();
            serde_json::json!({
                "name": pkg.name(),
                "version": pkg.manifest().package.version,
                "root": pkg.root().display().to_string(),
                "dependencies": dependencies,
            })
        })
        .collect();
    serde_json::json!({
        "workspace": project.root().display().to_string(),
        "members": members,
        "default-members": project.default_members().iter().map(|&i| project.package(i).name()).collect::<Vec<_>>(),
        "lockfile": project.lockfile_path().display().to_string(),
        "target-dir": project.target_dir().display().to_string(),
    })
}

fn print_workspace_info(project: &Project) {
    let members: Vec<String> =
        project.members().iter().map(|&i| project.package(i)).map(|p| format!("{} v{}", p.name(), p.manifest().package.version)).collect();
    println!("workspace:    {}", project.root().display());
    println!("members:      {}", members.join(", "));
    if project.default_members() != project.members() {
        let defaults: Vec<&str> = project.default_members().iter().map(|&i| project.package(i).name()).collect();
        println!("default:      {}", defaults.join(", "));
    }
    if let Some(&first) = project.members().first() {
        println!("kotlin:       {}", project.package(first).manifest().kotlin.version);
    }
    println!("lockfile:     {}", project.lockfile_path().display());
    println!("target dir:   {}", project.target_dir().display());
    // The module graph, in build order.
    let order = project.dependency_order(project.members(), false);
    if order.iter().any(|&i| !project.dependencies_of(i, false).is_empty()) {
        println!("graph:");
        for i in order {
            let deps: Vec<&str> = project.dependencies_of(i, false).into_iter().map(|d| project.package(d).name()).collect();
            match deps.is_empty() {
                true => println!("    {}", project.package(i).name()),
                false => println!("    {} -> {}", project.package(i).name(), deps.join(", ")),
            }
        }
    }
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

/// The candidate closest to `name` when it is near enough to be a typo (as Cargo suggests commands):
/// at most a third of the name's length (at least one) away, counting a swap of neighbors as one edit.
fn closest_match<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    let limit = name.chars().count().max(3) / 3;
    candidates.iter().map(|c| (edit_distance(name, c), c)).filter(|(d, _)| *d <= limit).min_by_key(|(d, _)| *d).map(|(_, c)| c.as_str())
}

/// Optimal string alignment distance: insertions, deletions, substitutions and swaps of neighbors.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// `kargo <name>`: a task from the manifest, else a `kargo-<name>` executable.
fn external(config: &Config, global: &GlobalArgs, argv: Vec<String>) -> Result<u8> {
    let (name, args) = argv.split_first().context("missing subcommand")?;
    let project = load_project(global).ok();
    if let Some(project) = &project
        && run_task(config.shell(), project, name, args, &EnvOptions::default())?
    {
        return Ok(0);
    }
    let Some(program) = kargo_ops::plugins::find_subcommand(config, name) else {
        let mut known: Vec<String> = Cli::command()
            .get_subcommands()
            .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_all_aliases()))
            .map(str::to_owned)
            .collect();
        known.extend(kargo_ops::plugins::list_subcommands(config).into_keys());
        if let Some(project) = &project {
            known.extend(task_names(project));
        }
        let suggestion = closest_match(name, &known).map(|s| format!("\n\n\tDid you mean `{s}`?")).unwrap_or_default();
        bail!("no such command: `{name}`{suggestion}\n\n\tView all installed commands with `kargo --help` and `kargo plugins`");
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
    fn suggests_the_closest_command() {
        let known: Vec<String> = ["build", "bench", "run", "test", "tree", "check"].map(String::from).to_vec();
        assert_eq!(closest_match("biuld", &known), Some("build"));
        assert_eq!(closest_match("chek", &known), Some("check"));
        assert_eq!(closest_match("tset", &known), Some("test"));
        assert_eq!(closest_match("definitely-not-a-command", &known), None);
        assert_eq!(closest_match("x", &known), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn short_help_hides_only_uncommon_commands() {
        for name in COMMON_COMMANDS {
            assert!(Cli::command().find_subcommand(name).is_some(), "`{name}` is in COMMON_COMMANDS but is not a command");
        }
        let short = hide_uncommon_commands(Cli::command());
        let hidden = |name: &str| short.find_subcommand(name).is_some_and(|c| c.is_hide_set());
        assert!(!hidden("build"));
        assert!(!hidden("task"));
        assert!(hidden("lsp"));
        assert!(hidden("completions"));
        // The command itself is unchanged, so completions and `kargo help lsp` still see it.
        assert!(!Cli::command().find_subcommand("lsp").unwrap().is_hide_set());
    }

    #[test]
    fn list_and_top_level_help_are_recognized_before_parsing() {
        let args = |words: &[&str]| words.iter().map(|w| std::ffi::OsString::from(*w)).collect::<Vec<_>>();
        assert!(wants_builtin_list(&args(&["kargo", "--list"])));
        assert!(wants_builtin_list(&args(&["kargo", "-q", "--list"])));
        assert!(!wants_builtin_list(&args(&["kargo", "--list", "build"])));
        assert!(!wants_builtin_list(&args(&["kargo", "build"])));
        assert!(wants_top_level_help(&args(&["kargo", "-q", "help"])));
        assert!(!wants_top_level_help(&args(&["kargo", "help", "lsp"])));
        assert!(!has_command(&args(&["kargo"])));
        assert!(!has_command(&args(&["kargo", "-q", "-v"])));
        assert!(has_command(&args(&["kargo", "-q", "--help"])));
        assert!(has_command(&args(&["kargo", "-h"])));
        assert!(has_command(&args(&["kargo", "build"])));
        assert!(has_command(&args(&["kargo", "-q", "my-plugin"])));
    }

    #[test]
    fn parses_common_invocations() {
        let cli = Cli::try_parse_from(["kargo", "build", "--release", "-t", "jvm", "-t", "native"]).unwrap();
        match cli.command {
            Command::Build { build, keep_going, xcframework } => {
                let o = build.options();
                assert_eq!(o.profile, ProfileKind::Release);
                assert!(!keep_going && !xcframework);
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
        assert!(matches!(cli.command, Command::Package { fat: true, bin: None, format: None, desktop: false, .. }));
        let cli = Cli::try_parse_from(["kargo", "package", "--desktop", "--format", "jar", "--platforms", "all,linux-x64"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Package { desktop: true, format: Some(PackageFormatArg::Jar), ref platforms, .. } if platforms == &["all", "linux-x64"]
        ));
        assert!(Cli::try_parse_from(["kargo", "package", "--platforms", "all"]).is_err(), "--platforms needs --desktop");
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
            Cli::try_parse_from(["kargo", "build", "--xcframework"]).unwrap().command,
            Command::Build { xcframework: true, .. }
        ));
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
        assert!(matches!(cli.command, Command::Fetch { sources: true, tools: false, .. }));
        let cli = Cli::try_parse_from(["kargo", "fetch", "--tools"]).unwrap();
        assert!(matches!(cli.command, Command::Fetch { sources: false, tools: true, .. }));
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

    #[test]
    fn parses_watch_flags() {
        let cli = Cli::try_parse_from(["kargo", "run", "--watch", "--clear", "--ignore", "*.log", "--", "x"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Run { watch: true, ref watch_args, ref args, .. }
                if watch_args.clear && watch_args.ignore == ["*.log"] && args == &["x"]
        ));
        assert!(Cli::try_parse_from(["kargo", "run", "--watch", "--debug"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "test", "--watch", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { watch: true, ref filter, .. } if filter.as_deref() == Some("MathTest")));
        assert!(Cli::try_parse_from(["kargo", "test", "--watch", "--list"]).is_err());

        let cli = Cli::try_parse_from(["kargo", "watch"]).unwrap();
        assert!(matches!(cli.command, Command::Watch { ref exec, ref command, .. } if exec.is_empty() && command.is_empty()));
        let cli = Cli::try_parse_from(["kargo", "watch", "-x", "test -t jvm", "-x", "run"]).unwrap();
        assert!(matches!(cli.command, Command::Watch { ref exec, .. } if exec == &["test -t jvm", "run"]));
        let cli = Cli::try_parse_from(["kargo", "watch", "--clear", "test", "-t", "jvm"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Watch { ref command, ref watch_args, .. } if command == &["test", "-t", "jvm"] && watch_args.clear
        ));
        assert!(Cli::try_parse_from(["kargo", "watch", "-x", "check", "build"]).is_err());
    }

    #[test]
    fn clear_and_ignore_need_watch() {
        assert!(watch_options(false, &WatchArgs { clear: true, ignore: Vec::new() }).is_err());
        assert!(watch_options(false, &WatchArgs { clear: false, ignore: vec!["*.log".to_owned()] }).is_err());
        assert!(watch_options(false, &WatchArgs { clear: false, ignore: Vec::new() }).unwrap().is_none());
        let options = watch_options(true, &WatchArgs { clear: true, ignore: Vec::new() }).unwrap().unwrap();
        assert!(options.clear);
    }

    #[test]
    fn watch_commands_default_to_build_and_split_on_spaces() {
        assert_eq!(watch_commands(Vec::new(), Vec::new()).unwrap(), [vec!["build".to_owned()]]);
        assert_eq!(
            watch_commands(vec!["test  -t jvm".to_owned()], Vec::new()).unwrap(),
            [vec!["test".to_owned(), "-t".into(), "jvm".into()]]
        );
        assert_eq!(watch_commands(Vec::new(), vec!["check".to_owned()]).unwrap(), [vec!["check".to_owned()]]);
        assert!(watch_commands(vec!["  ".to_owned()], Vec::new()).is_err());
    }
}
