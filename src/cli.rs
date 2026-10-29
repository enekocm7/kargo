//! Command-line interface.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::builder::BoolishValueParser;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCandidates, SubcommandCandidates};

use kargo_manifest::edit::DependencySection;
use kargo_manifest::{BenchMode, BenchRounds, Scope, TargetKind};
use kargo_ops::env::EnvOptions;
use kargo_ops::ops::bench::{BenchOptions, BenchOverrides};
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
use kargo_util::help;
use kargo_util::jobs::Jobs;
use kargo_util::shell::{self, Shell, Verbosity};

use crate::complete::{self, ShellArg};

/// Exit code for build and test failures (matches Cargo).
const FAILURE: i32 = 101;

const AFTER_HELP: &str = "See all commands with `kargo --list`.\nSee `kargo help <command>` for more information on a specific command.";

/// The commands `kargo --help` lists. The others are left out of the short help, but `kargo --list`, `kargo help <command>`
/// and the shell completions still show them.
const COMMON_COMMANDS: &[&str] = &[
    "new",
    "init",
    "import",
    "build",
    "check",
    "run",
    "test",
    "bench",
    "watch",
    "add",
    "remove",
    "update",
    "fetch",
    "tree",
    "why",
    "clean",
    "package",
    "publish",
    "install",
    "uninstall",
    "fmt",
    "lint",
    "doc",
    "task",
    "toolchain",
    "doctor",
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
    #[arg(long, global = true, env = "KARGO_OFFLINE", value_parser = BoolishValueParser::new())]
    offline: bool,
    /// Require kargo.lock to be up to date
    #[arg(long, global = true)]
    locked: bool,
    /// Resolve dependencies only from the repository `kargo vendor` wrote
    #[arg(long, global = true, env = "KARGO_VENDORED", value_parser = BoolishValueParser::new())]
    vendored: bool,
    /// Ignore cached metadata and kargo.lock, re-checking repositories and re-verifying cached artifacts
    #[arg(long, global = true)]
    refresh: bool,
    /// Fail on a download whose repository publishes no checksum for it
    #[arg(long, global = true, env = "KARGO_REQUIRE_CHECKSUMS", value_parser = BoolishValueParser::new())]
    require_checksums: bool,
    /// Run Android builds with the Gradle on PATH instead of a Kargo-managed one
    #[arg(long, global = true, env = "KARGO_SYSTEM_GRADLE", value_parser = BoolishValueParser::new())]
    system_gradle: bool,
    /// Path to kargo.toml
    #[arg(long, global = true, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// When to color Kargo's output (NO_COLOR turns it off with `auto`)
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value = "auto")]
    color: ColorArg,
}

/// `--color`: when Kargo's own output uses colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ColorArg {
    Auto,
    Always,
    Never,
}

impl ColorArg {
    fn apply(self) {
        shell::set_color(match self {
            ColorArg::Auto => shell::Color::Auto,
            ColorArg::Always => shell::Color::Always,
            ColorArg::Never => shell::Color::Never,
        });
    }
}

/// The CI system `kargo init-ci` writes a workflow for.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CiSystemArg {
    Github,
}

impl From<CiSystemArg> for ops::ci::CiSystem {
    fn from(system: CiSystemArg) -> Self {
        match system {
            CiSystemArg::Github => ops::ci::CiSystem::Github,
        }
    }
}

/// The build tool `kargo import` reads.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ImportSource {
    Maven,
    Gradle,
}

impl From<ImportSource> for ops::import::Build {
    fn from(source: ImportSource) -> Self {
        match source {
            ImportSource::Maven => ops::import::Build::Maven,
            ImportSource::Gradle => ops::import::Build::Gradle,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TargetArg {
    Jvm,
    Wasm,
    Js,
    Native,
    Android,
}

impl From<TargetArg> for TargetKind {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Jvm => TargetKind::Jvm,
            TargetArg::Wasm => TargetKind::Wasm,
            TargetArg::Js => TargetKind::Js,
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

/// `kargo install --format`.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum InstallFormatArg {
    /// The self-contained jar, run by the JDK of the build
    #[default]
    Jar,
    /// A Java runtime trimmed by jlink and launchers: runs without an installed Java
    Jlink,
}

impl From<InstallFormatArg> for ops::install::Format {
    fn from(f: InstallFormatArg) -> Self {
        match f {
            InstallFormatArg::Jar => ops::install::Format::Jar,
            InstallFormatArg::Jlink => ops::install::Format::Jlink,
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
    /// An Android SDK platform with its build-tools and platform-tools
    AndroidSdk,
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
            ToolArg::AndroidSdk => ToolKind::AndroidSdk,
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

/// The arguments of `kargo android`, in a struct of their own for the same reason as [`WhyArgs`].
#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    long_about = "Run Gradle tasks in the generated Android project, and manage the devices `kargo run -t android` installs on.

Anything that is not one of the subcommands is passed to Gradle: `kargo android lint`, `kargo android bundleRelease`.
Tasks that use a device (`install*`, `uninstall*`, `connected*`, `*AndroidTest`) run on the one chosen like
`kargo run -t android` chooses: `--device`, then KARGO_ANDROID_DEVICE, then the only device, or a list to pick from."
)]
struct AndroidArgs {
    #[command(subcommand)]
    command: Option<AndroidCommand>,
    /// For Gradle tasks that use a device: the serial or the name of the virtual device to run on (or set KARGO_ANDROID_DEVICE)
    #[arg(short = 's', long, value_name = "SERIAL|AVD")]
    device: Option<String>,
    /// Arguments passed to Gradle
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// The arguments of `kargo devices`.
#[derive(Debug, Args)]
struct DevicesArgs {
    /// Output format
    #[arg(long, value_enum, default_value_t)]
    format: FormatArg,
}

/// The options of `kargo run -t android` that choose the device and the log.
#[derive(Debug, Args)]
struct AndroidRunArgs {
    /// Android: run on this device, a serial (`kargo android devices` lists them) or the name of a virtual device, which is started if it is not
    /// running; without it one device is used, several are chosen from a list, and none asks before starting or creating an emulator
    #[arg(short = 's', long, value_name = "SERIAL|AVD", env = "KARGO_ANDROID_DEVICE")]
    device: Option<String>,
    /// Android: do not show the app's log; leave once the app started
    #[arg(long, overrides_with = "logcat", conflicts_with = "watch")]
    no_logcat: bool,
    /// Android: show the app's log until it exits or Ctrl+C stops it (the default)
    #[arg(long, overrides_with = "no_logcat")]
    logcat: bool,
    #[command(flatten)]
    emulator: EmulatorArgs,
    /// Android: shut down an emulator that this run started once the app is done
    #[arg(long, conflicts_with = "watch")]
    stop_emulator: bool,
}

impl AndroidRunArgs {
    /// Whether a flag that only an Android run has was given (`--device` set by the environment does not count).
    fn is_set(&self) -> bool {
        self.no_logcat || self.stop_emulator || self.emulator.headless || self.emulator.cold_boot || self.emulator.wipe_data
    }

    fn options(self) -> kargo_ops::android_device::AndroidRun {
        kargo_ops::android_device::AndroidRun {
            device: self.device,
            no_logcat: self.no_logcat,
            emulator: self.emulator.options(),
            stop_emulator: self.stop_emulator,
            watch: false,
        }
    }
}

/// How an emulator that Kargo starts boots.
#[derive(Debug, Args)]
struct EmulatorArgs {
    /// Android: start an emulator without a window or sound
    #[arg(long)]
    headless: bool,
    /// Android: boot an emulator without its saved snapshot
    #[arg(long)]
    cold_boot: bool,
    /// Android: erase the user data of an emulator before it starts
    #[arg(long)]
    wipe_data: bool,
    /// Android: seconds to wait for an emulator to boot
    #[arg(long, value_name = "SECONDS", default_value_t = 300)]
    boot_timeout: u64,
}

impl EmulatorArgs {
    fn options(&self) -> kargo_ops::android_device::EmulatorOptions {
        kargo_ops::android_device::EmulatorOptions {
            headless: self.headless,
            cold_boot: self.cold_boot,
            wipe_data: self.wipe_data,
            timeout: std::time::Duration::from_secs(self.boot_timeout),
        }
    }
}

#[derive(Debug, Subcommand)]
enum AndroidCommand {
    /// List connected devices, running emulators and installed virtual devices
    Devices {
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
    },
    /// List, create and delete virtual devices (AVDs)
    Avd {
        #[command(subcommand)]
        command: AvdCommand,
    },
    /// Start and stop emulators
    Emulator {
        #[command(subcommand)]
        command: EmulatorCommand,
    },
    /// Connect to a device over Wi-Fi (wireless debugging)
    Connect {
        /// The address from the Wireless debugging screen of the device, `192.168.1.20:37099`
        address: String,
    },
    /// Pair with a device for wireless debugging
    Pair {
        /// The pairing address from "Pair device with pairing code" on the device
        address: String,
        /// The six-digit pairing code (adb asks for it when left out)
        code: Option<String>,
    },
    /// Disconnect from a wireless device (all of them without an address)
    Disconnect { address: Option<String> },
}

#[derive(Debug, Subcommand)]
enum AvdCommand {
    /// List the installed virtual devices
    #[command(visible_alias = "ls")]
    List {
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
    },
    /// Create a virtual device; Kargo's SDK installs the emulator and the system image first (after asking)
    Create {
        /// Its name (default: Kargo_API_<api>)
        name: Option<String>,
        /// The Android API level (default: the project's compile-sdk, else the newest installed system image)
        #[arg(long, value_name = "LEVEL")]
        api: Option<u32>,
        /// The hardware profile, such as `pixel_8` (`avdmanager list device` shows them)
        #[arg(long, value_name = "PROFILE")]
        hardware: Option<String>,
        /// Replace a virtual device of the same name
        #[arg(long)]
        force: bool,
        /// Accept the licenses of the Android SDK packages, after showing them, instead of asking (for CI)
        #[arg(long)]
        accept_licenses: bool,
    },
    /// Delete a virtual device
    #[command(visible_alias = "rm")]
    Delete {
        /// Its name
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum EmulatorCommand {
    /// Start a virtual device and wait until it booted
    Start {
        /// The virtual device (default: the only one)
        avd: Option<String>,
        #[command(flatten)]
        options: EmulatorArgs,
    },
    /// Shut down a running emulator
    Stop {
        /// The serial (`emulator-5554`) or the virtual device it runs
        target: String,
    },
}

#[derive(Debug, Subcommand)]
enum ToolchainCommand {
    /// List installed JDKs, GraalVMs, Kotlin/Native and Gradle distributions, Android SDK packages, Kotlin language servers and the debug adapter
    #[command(visible_alias = "ls")]
    List,
    /// Install a toolchain (without arguments: everything this project needs)
    Install {
        #[arg(value_enum)]
        kind: Option<ToolArg>,
        /// JDK: `21` (latest 21) or `21.0.5+11`; GraalVM: `25` or `oracle-25`; Native: a Kotlin version; Gradle: a Gradle version (default: the project's);
        /// android-sdk: a platform API level such as `36` (default: the project's);
        /// kotlin-lsp, kls, kotlin-debug-adapter: a version (default: the one `kargo lsp` or `kargo dap` uses)
        version: Option<String>,
        /// Accept the licenses of the Android SDK packages, after showing them, instead of asking (for CI)
        #[arg(long)]
        accept_licenses: bool,
        /// android-sdk: also install the emulator, `avdmanager` and a system image, to run the app on a virtual device
        #[arg(long)]
        emulator: bool,
    },
    /// Remove an installed toolchain
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(value_enum)]
        kind: ToolArg,
        /// JDK: a major version (removes all its releases) or a release; Native, Gradle, kotlin-lsp, kls, kotlin-debug-adapter: a version;
        /// android-sdk: a platform API level or a package such as `build-tools;36.0.0`
        #[arg(add = ArgValueCandidates::new(complete::installed_toolchains))]
        version: String,
    },
    /// Show JDK versions, or with `android-sdk` the Android SDK platforms, available for download
    Available {
        #[arg(value_enum)]
        kind: Option<ToolArg>,
    },
    /// Show which toolchains this project uses
    Which,
}

#[derive(Debug, Subcommand)]
enum SelfCommand {
    /// Update Kargo to the latest release (or to --version), verifying the archive's SHA-256
    #[command(long_about = "Download the latest Kargo release for this platform, verify its SHA-256 against the release's SHA256SUMS and
replace the running binary with it. Kargo installed by Homebrew, Scoop, winget, Nix, SDKMAN!, Cargo or a system
package manager is not replaced: the command prints that manager's update command instead.

KARGO_RELEASES_URL points Kargo at another place that publishes releases (a mirror, or a directory with file://).")]
    Update {
        /// Install this version (such as 0.3.0) instead of the latest; an older one downgrades
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
    },
    /// Remove the kargo binary and the PATH entries the installers added
    Uninstall {
        /// Also delete ~/.kargo: downloaded toolchains, the Maven cache and installed tools
        #[arg(long)]
        purge: bool,
    },
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

impl From<ConfigurationArg> for Configuration {
    fn from(c: ConfigurationArg) -> Self {
        match c {
            ConfigurationArg::Compile => Configuration::Compile,
            ConfigurationArg::Runtime => Configuration::Runtime,
            ConfigurationArg::Test => Configuration::Test,
        }
    }
}

/// The arguments of `kargo why`, in a struct of their own: clap parses them in a function apart from
/// the large one for [`Command`], which keeps the latter's stack frame small enough for test threads.
#[derive(Debug, Args)]
struct WhyArgs {
    /// The module: group:artifact, optionally with a version (group:artifact:version)
    #[arg(value_name = "MODULE")]
    module: String,
    #[arg(short, long, value_enum)]
    target: Option<TargetArg>,
    /// Which classpath to explain
    #[arg(short, long, value_enum, default_value = "runtime")]
    configuration: ConfigurationArg,
    /// Show every path from the project to the module, not only the shortest
    #[arg(long)]
    all_paths: bool,
    #[command(flatten)]
    packages: PackageArgs,
    /// Output format; `json` describes the requests, the reason and the paths
    #[arg(long, value_enum, default_value_t)]
    format: FormatArg,
}

/// Which members `kargo inspect` prints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum VisibilityArg {
    /// Public members (and protected ones, which subclasses use)
    #[default]
    Public,
    Protected,
    Package,
    Private,
}

impl From<VisibilityArg> for ops::inspect::Visibility {
    fn from(v: VisibilityArg) -> Self {
        match v {
            VisibilityArg::Public => Self::Public,
            VisibilityArg::Protected => Self::Protected,
            VisibilityArg::Package => Self::Package,
            VisibilityArg::Private => Self::Private,
        }
    }
}

/// The arguments of `kargo inspect`, in a struct of their own for the same reason as [`WhyArgs`].
/// The arguments of `kargo add`, apart from the enum of commands, which is close to the stack of the test threads.
#[derive(Debug, clap::Args)]
struct AddArgs {
    /// Coordinates: group:artifact[:version[:classifier]] (latest release if no version); with --npm, packages: name[@version]
    #[arg(required_unless_present_any = ["frameworks", "path", "cinterop"], conflicts_with_all = ["path", "cinterop"])]
    coordinates: Vec<String>,
    /// Depend on the Kargo package in this directory, built from source
    #[arg(long, value_name = "DIR", conflicts_with_all = ["bom", "pre", "no_verify", "frameworks"])]
    path: Option<PathBuf>,
    /// Add to [dev-dependencies] (with --npm: to the test-only [target.<target>.dev-npm-dependencies])
    #[arg(long)]
    dev: bool,
    /// Add to [target.<target>.dependencies] (with --npm: to the wasm or js target's npm packages)
    #[arg(short, long, value_enum)]
    target: Option<TargetArg>,
    /// Add npm packages (name[@version], @scope/name, name@tag) to [target.wasm.npm-dependencies] or [target.js.npm-dependencies]
    #[arg(long, conflicts_with_all = ["path", "cinterop", "frameworks", "bom", "scope", "header", "pkg_config"])]
    npm: bool,
    /// With --npm: also add @types/<package> when the package ships no TypeScript declarations
    #[arg(long, requires = "npm")]
    types: bool,
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
}

#[derive(Debug, Args)]
#[command(long_about = "Show the jar of a resolved dependency and its API, as `javap` would, without finding the jar by hand.

Looks the module up in the classpath a build uses (the version in kargo.lock) and prints where its jar
is in the cache. With no class it lists the jar's public classes; with one or more it prints their
members using the JDK's `javap`, with the whole classpath behind them so supertypes resolve. A class
is named in full (com.acme.Outer.Inner or com/acme/Outer$Inner) or by the end of its name (Inner,
Outer.Inner) when only one class matches.

The output is the JVM view of a class, so Kotlin properties show as getters and setters.")]
struct InspectArgs {
    /// The module: group:artifact, optionally with a version (group:artifact:version)
    #[arg(value_name = "MODULE")]
    module: String,
    /// Classes to print; none lists the classes of the jar
    #[arg(value_name = "CLASS")]
    classes: Vec<String>,
    /// Which classpath to read the module from
    #[arg(short, long, value_enum, default_value = "runtime")]
    configuration: ConfigurationArg,
    /// Which members to print, and with `package` or `private` also the classes that are not public
    #[arg(long, value_enum, default_value_t)]
    visibility: VisibilityArg,
    #[command(flatten)]
    package: OnePackageArg,
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

/// The lowest severity that makes `kargo audit` fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DenyArg {
    Low,
    Medium,
    High,
    Critical,
    /// Report advisories without failing
    None,
}

impl From<DenyArg> for kargo_manifest::DenyLevel {
    fn from(arg: DenyArg) -> Self {
        match arg {
            DenyArg::Low => Self::Low,
            DenyArg::Medium => Self::Medium,
            DenyArg::High => Self::High,
            DenyArg::Critical => Self::Critical,
            DenyArg::None => Self::None,
        }
    }
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
    #[command(flatten)]
    jobs: JobsArg,
    /// How to print compiler diagnostics
    #[arg(long, value_enum, default_value_t, value_name = "FMT")]
    message_format: MessageFormatArg,
    /// Print Binaryen's per-function size metrics for Wasm release builds
    #[arg(long)]
    size_report: bool,
}

impl BuildArgs {
    fn options(&self) -> BuildOptions {
        BuildOptions {
            targets: self.targets.iter().map(|t| (*t).into()).collect(),
            profile: profile_kind(self.release, self.profile.as_deref()),
            size_report: self.size_report,
            ..Default::default()
        }
    }
}

/// `-j/--jobs`, of every command that builds.
#[derive(Debug, Clone, Copy, Args)]
struct JobsArg {
    /// How many packages and targets to build at the same time, or `auto` (0) for the CPUs within the free memory
    #[arg(short, long, value_name = "N", env = kargo_util::jobs::ENV, value_parser = Jobs::parse)]
    jobs: Option<Jobs>,
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
        let mut message = serde_json::json!({ "reason": "build-finished", "success": result.is_ok() });
        if let Err(err) = &result {
            advice_fields(err, &mut message);
        }
        BUILD_FINISHED.store(true, std::sync::atomic::Ordering::Relaxed);
        kargo_compiler::diagnostics::emit(&message);
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
    /// Built-in frameworks to enable: compose, serialization, coroutines, ktor, lombok, spring-boot, benchmark;
    /// or compose-multiplatform, Compose on the jvm (desktop) and wasm (browser) targets with a shared App()
    #[arg(short = 'w', long = "with", value_delimiter = ',', add = ArgValueCandidates::new(complete::new_frameworks))]
    frameworks: Vec<String>,
    /// Add a sample benchmark in benches/ (the same as `--with benchmark`)
    #[arg(long)]
    bench: bool,
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
    #[arg(long, conflicts_with_all = ["lib", "targets", "frameworks", "bench", "name"])]
    workspace: bool,
}

/// The arguments of `kargo run`, in a struct of their own for the same reason as [`WhyArgs`].
#[derive(Debug, Args)]
struct RunArgs {
    #[arg(short, long, conflicts_with = "profile")]
    release: bool,
    /// Run with a custom profile from kargo.toml (`[profile.<name>]`)
    #[arg(long, value_name = "NAME", conflicts_with = "release")]
    profile: Option<String>,
    /// Target to run (defaults to jvm, then native, wasm, js, android)
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
    /// Port to serve a Wasm or JS browser app on (default 8080; 0 picks a free one)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Open the served page of a Wasm or JS browser app in your browser
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
    android: Box<AndroidRunArgs>,
    #[command(flatten)]
    package: OnePackageArg,
    #[command(flatten)]
    jobs: JobsArg,
    /// How to print compiler diagnostics
    #[arg(long, value_enum, default_value_t, value_name = "FMT")]
    message_format: MessageFormatArg,
    /// Rebuild and restart the program when its files change, or reload the open pages of a browser app (Ctrl+C stops it)
    #[arg(long, conflicts_with = "debug")]
    watch: bool,
    /// Like --watch, but a JVM program keeps running and the changed classes are swapped into it (Compose recomposes)
    #[arg(long, visible_alias = "hot", conflicts_with = "debug")]
    hot_reload: bool,
    #[command(flatten)]
    watch_args: WatchArgs,
    #[command(flatten)]
    env: EnvArgs,
    /// Run this Kotlin code as a script, like `kargo script -e`
    #[arg(short, long, value_name = "CODE", conflicts_with_all = ["main", "bin", "example"])]
    expr: Option<String>,
    /// A .kt or .kts file to run, then arguments for the program; a first word that is not an existing .kt or .kts file is an argument too
    #[arg(value_name = "FILE")]
    words: Vec<String>,
    /// Arguments for the program
    #[arg(last = true)]
    args: Vec<String>,
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
    /// Convert a Maven or Gradle project to a Kargo project
    Import {
        /// The project directory (defaults to the current one)
        path: Option<PathBuf>,
        /// Print the kargo.toml files and the planned file moves, and change nothing
        #[arg(long)]
        dry_run: bool,
        /// Do not build the project when it is imported
        #[arg(long)]
        no_build: bool,
        /// The build tool to read, when the directory has the files of both
        #[arg(long, value_enum)]
        from: Option<ImportSource>,
    },
    /// Write a CI workflow for the project, from its kargo.toml
    InitCi {
        /// The CI system to write a workflow for
        #[arg(long, value_enum, default_value = "github")]
        system: CiSystemArg,
        /// Replace the workflow if it exists
        #[arg(long)]
        force: bool,
        /// Publish the JUnit test reports in the run summary
        #[arg(long)]
        junit: bool,
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
    Run(RunArgs),
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
        /// Run JVM tests against the R8-optimized package jar
        #[arg(long, conflicts_with_all = ["list", "no_run"])]
        against_min: bool,
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
    /// Compile and run benchmarks (JMH)
    Bench {
        #[command(flatten)]
        build: BuildArgs,
        /// Only run benchmarks whose `package.Class.method` name contains this string
        filter: Option<String>,
        /// Compile the benchmarks but do not run them
        #[arg(long, conflicts_with_all = ["save", "baseline"])]
        no_run: bool,
        /// Store the results as the baseline NAME in target/bench/
        #[arg(long, value_name = "NAME")]
        save: Option<String>,
        /// Compare the results with the baseline NAME, saved earlier with --save
        #[arg(long, value_name = "NAME")]
        baseline: Option<String>,
        /// Output format; `json` prints JMH's JSON results on stdout and the report on stderr
        #[arg(long, value_enum, default_value_t, value_name = "FMT")]
        format: FormatArg,
        /// Warmup iterations, as a count and a duration such as 3x1s (overrides [bench] warmup)
        #[arg(long, value_name = "NxTIME", value_parser = parse_rounds)]
        warmup: Option<BenchRounds>,
        /// Measured iterations, as a count and a duration such as 5x1s (overrides [bench] iterations)
        #[arg(long, value_name = "NxTIME", value_parser = parse_rounds)]
        iterations: Option<BenchRounds>,
        /// How many JVMs run each benchmark; 0 runs it in the JVM of the launcher (overrides [bench] forks)
        #[arg(long, value_name = "N")]
        forks: Option<u32>,
        /// What to measure: thrpt (operations per time), avgt (time per operation), sample, ss or all (overrides [bench] mode)
        #[arg(long, value_name = "MODE", value_parser = ["thrpt", "avgt", "sample", "ss", "all"])]
        mode: Option<String>,
        /// The unit results are reported in (overrides [bench] time-unit)
        #[arg(long, value_name = "UNIT", value_parser = kargo_manifest::BENCH_TIME_UNITS)]
        time_unit: Option<String>,
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
        #[command(flatten)]
        jobs: JobsArg,
    },
    /// Render the @Preview composables of a Compose Desktop app to PNG files
    Preview {
        /// Previews to render, by function or qualified name (default: all)
        names: Vec<String>,
        /// List the previews instead of rendering them
        #[arg(long)]
        list: bool,
        /// Write the PNG files here (default: target/preview)
        #[arg(long, value_name = "DIR")]
        out_dir: Option<PathBuf>,
        /// Width of the image, in density-independent pixels
        #[arg(long, default_value_t = 800)]
        width: u32,
        /// Height of the image, in density-independent pixels
        #[arg(long, default_value_t = 600)]
        height: u32,
        /// Pixels per density-independent pixel (2 for a high-density image)
        #[arg(long, default_value_t = 1.0)]
        density: f32,
        /// Build with the release profile
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Build with a custom profile from kargo.toml
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Format Kotlin sources with ktlint
    Fmt {
        /// Report unformatted files without changing them; fail if there are any
        #[arg(long)]
        check: bool,
        #[command(flatten)]
        packages: PackageArgs,
        #[command(flatten)]
        jobs: JobsArg,
    },
    /// Analyze Kotlin sources with detekt
    Lint {
        /// Record the current issues in the baseline file instead of reporting them
        #[arg(long)]
        create_baseline: bool,
        #[command(flatten)]
        packages: PackageArgs,
        #[command(flatten)]
        jobs: JobsArg,
    },
    /// Add dependencies or built-in frameworks to kargo.toml
    Add(AddArgs),
    /// Remove dependencies from kargo.toml
    #[command(visible_alias = "rm")]
    Remove {
        /// group:artifact or the manifest key; with --npm, the package name
        #[arg(required = true)]
        names: Vec<String>,
        /// Remove from [dev-dependencies] (with --npm: from [target.<target>.dev-npm-dependencies])
        #[arg(long)]
        dev: bool,
        /// Remove from [target.<target>.dependencies] (with --npm: from the wasm or js target's npm packages)
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// Remove npm packages from [target.wasm.npm-dependencies] or [target.js.npm-dependencies]
        #[arg(long)]
        npm: bool,
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
    /// Check the dependencies against known security advisories (OSV)
    #[command(long_about = "Check the dependencies against known security advisories.

Resolves the dependencies of every target (or the ones given with --target), with the npm packages of Wasm and Kotlin/JS, and looks each version up in the OSV database (https://osv.dev). This is the only command that contacts the advisory database. Answers are cached for a day under the Kargo home; --offline uses the cache, --refresh ignores it, and KARGO_OSV_URL points at another OSV server.

Exits with 101 when an advisory at or above the --deny severity (default: [audit] deny, else high) affects a dependency and is not in [audit] ignore.")]
    Audit {
        /// Only audit these targets [default: every enabled one]
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
        /// The lowest severity that fails the audit [default: `[audit] deny`, else high]
        #[arg(long, value_enum)]
        deny: Option<DenyArg>,
        /// Also audit the build's tools that kargo.lock holds (Kotlin compiler, KSP, JUnit launcher, compiler plugins, `[[generate]]` tools) and the TypeScript compiler's npm packages
        #[arg(long)]
        tooling: bool,
        /// Raise the versions kargo.toml declares to ones that fix the advisories, where they are compatible
        #[arg(long)]
        fix: bool,
        #[command(flatten)]
        packages: PackageArgs,
        /// Output format; `json` describes every advisory found, ignored ones included
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
    },
    /// Enable, disable or list the project's targets (jvm, wasm, js, native, android)
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
        /// Show what depends on this module (group:artifact, optionally :version), up to the project: the inverted tree
        #[arg(short, long, value_name = "MODULE", conflicts_with = "duplicates")]
        invert: Option<String>,
        #[command(flatten)]
        packages: PackageArgs,
        /// Output format; `json` prints the whole resolved graph (`kargo why --format json` explains one module)
        #[arg(long, value_enum, default_value_t, conflicts_with_all = ["depth", "duplicates", "invert"])]
        format: FormatArg,
    },
    /// Explain why a module is on the classpath and why at its version
    Why(WhyArgs),
    /// Show where a dependency's jar is and the API of its classes, javap style
    Inspect(InspectArgs),
    /// Download all dependencies and toolchains
    Fetch {
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
        /// Also download the sources jars of the dependencies (or their Javadoc jars where they have none), for editors
        #[arg(long)]
        sources: bool,
        /// Also download the tools of `kargo doc` (Dokka), `kargo fmt` (ktlint), `kargo lint` (detekt) and R8 for JVM packaging
        #[arg(long)]
        tools: bool,
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Run the `[[generate]]` steps of kargo.toml: protobuf, OpenAPI, BuildConfig and other generated sources
    #[command(long_about = "Run the `[[generate]]` steps of kargo.toml now, without building.

A step writes into target/generated/<profile>/<name>/ ($KARGO_GEN_DIR), which the build adds to the sources
of the targets the step names. A step with `inputs` runs only when they, its definition or Kargo changed since
it last succeeded; pass --force to run it anyway. `kargo build`, `check`, `test` and the other commands that
compile run the steps they need first, so this command is for editors, CI checks and trying a step out.")]
    Generate {
        /// The steps to run, with the steps they wait for [default: every step]
        names: Vec<String>,
        /// Generate for the release profile
        #[arg(short, long, conflicts_with = "profile")]
        release: bool,
        /// Generate for a custom profile from kargo.toml
        #[arg(long, value_name = "NAME", conflicts_with = "release")]
        profile: Option<String>,
        /// Only the steps for this target (repeatable)
        #[arg(short, long = "target", value_enum)]
        targets: Vec<TargetArg>,
        /// Run steps whose output is up to date too
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        packages: PackageArgs,
    },
    /// Copy every dependency into a directory laid out as a Maven repository, for offline builds
    Vendor {
        /// The directory to write, relative to the project root [default: `[source] directory`, else vendor]
        path: Option<PathBuf>,
        /// Also copy the Kotlin compiler and the Kotlin/Native distribution
        #[arg(long)]
        toolchains: bool,
        /// Remove the files of the directory that the project no longer needs
        #[arg(long)]
        sync: bool,
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
        /// Shrink and obfuscate the packaged JVM jar with R8 (also on by default for --release)
        #[arg(long)]
        optimize: bool,
        /// What to build for the JVM: jar (default), jlink, native-image or installer (default with --desktop)
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<PackageFormatArg>,
        /// Package the Compose Desktop application: an installer for this OS, or with --format jar a jar
        #[arg(long)]
        desktop: bool,
        /// With --desktop --format jar: the platforms whose Skiko natives the jar carries (all, or e.g. windows-x64,macos-arm64)
        #[arg(long, value_name = "PLATFORMS", value_delimiter = ',', requires = "desktop")]
        platforms: Vec<String>,
        /// How to print compiler and package reports
        #[arg(long, value_enum, default_value_t, value_name = "FMT")]
        message_format: MessageFormatArg,
        #[command(flatten)]
        package: OnePackageArg,
        #[command(flatten)]
        jobs: JobsArg,
    },
    /// Retrace an obfuscated JVM stack trace with an R8 mapping file
    Retrace {
        /// The mapping.txt written by `kargo package --optimize`
        mapping: PathBuf,
        /// Stack trace file (reads stdin when omitted)
        stack_trace: Option<PathBuf>,
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
    /// Install a Kotlin command-line tool from a project, a git repository or Maven
    #[command(long_about = "Install a Kotlin command-line tool, so that it runs from any directory.

The tool comes from a project (`--path <dir>`), a git repository (`--git <url>`, at `--tag`, `--branch`
or `--rev`) or a published application on Maven (`group:artifact[:version]`). Projects are built with
the release profile. Launchers go to ~/.kargo/bin (put it on your PATH) and the program's files to
~/.kargo/install/<name>/<version>/.

A JVM program gets a launcher that runs its jar with the JDK it was built with; `--format jlink`
installs a trimmed Java runtime with it instead. A Native program is copied to ~/.kargo/bin as it is.
Every entry point of the project is installed, or the ones `--bin` names.

`kargo install --force <name>` installs an installed program again from where it came from (an
upgrade). `kargo install --list` shows what is installed; `kargo uninstall <name>` removes it.")]
    Install {
        /// A published application: `group:artifact[:version]` (the newest version by default); with `--force`, the name of an installed program to install again
        #[arg(value_name = "SPEC", conflicts_with_all = ["path", "git", "list"])]
        spec: Option<String>,
        /// Install the project in this directory (`.` for the current one)
        #[arg(long, value_name = "DIR", conflicts_with_all = ["git", "list"])]
        path: Option<PathBuf>,
        /// Install the project in this git repository
        #[arg(long, value_name = "URL", conflicts_with = "list")]
        git: Option<String>,
        /// With `--git`: the tag to install
        #[arg(long, value_name = "TAG", requires = "git", conflicts_with_all = ["branch", "rev"])]
        tag: Option<String>,
        /// With `--git`: the branch to install
        #[arg(long, value_name = "BRANCH", requires = "git", conflicts_with_all = ["tag", "rev"])]
        branch: Option<String>,
        /// With `--git`: the commit to install
        #[arg(long, value_name = "SHA", requires = "git", conflicts_with_all = ["tag", "branch"])]
        rev: Option<String>,
        /// Install only this program (repeatable); every entry point by default
        #[arg(long = "bin", value_name = "NAME", add = ArgValueCandidates::new(complete::bins))]
        bins: Vec<String>,
        /// Target to install: jvm (default when enabled) or native
        #[arg(short, long, value_enum)]
        target: Option<TargetArg>,
        /// What to install for the JVM: the jar (default) or a jlink runtime image
        #[arg(long, value_enum, default_value_t)]
        format: InstallFormatArg,
        /// A Maven repository to look the application up in before Maven Central (repeatable)
        #[arg(long, value_name = "URL", requires = "spec")]
        repository: Vec<String>,
        /// Replace a program that is installed already
        #[arg(short, long)]
        force: bool,
        /// List the installed programs
        #[arg(long, conflicts_with_all = ["bins", "target", "force", "repository"])]
        list: bool,
        /// Install below this directory (`bin/` and `install/`) instead of the Kargo home
        #[arg(long, value_name = "DIR", env = "KARGO_INSTALL_ROOT")]
        root: Option<PathBuf>,
        #[command(flatten)]
        package: OnePackageArg,
    },
    /// Remove programs installed with `kargo install`
    Uninstall {
        /// Remove from this directory instead of the Kargo home (see `kargo install --root`)
        #[arg(long, value_name = "DIR", env = "KARGO_INSTALL_ROOT")]
        root: Option<PathBuf>,
        /// The programs to remove
        #[arg(required = true, value_name = "NAME", add = ArgValueCandidates::new(complete::installed_programs))]
        names: Vec<String>,
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
    /// Run Gradle tasks in the generated Android project, and manage devices, emulators and virtual devices
    Android(AndroidArgs),
    /// List the devices, emulators and virtual devices `kargo run -t android` can use (same as `kargo android devices`)
    Devices(DevicesArgs),
    /// Manage JDKs and Kotlin/Native distributions
    Toolchain {
        #[command(subcommand)]
        command: ToolchainCommand,
    },
    /// Update or uninstall Kargo itself
    #[command(name = "self")]
    SelfManage {
        #[command(subcommand)]
        command: SelfCommand,
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
    #[command(long_about = "Print the resolved classpath (JVM) or klib path (Wasm, JS, Native) of a target.

Prints the jars the build compiles with, in order: path dependencies (built if needed), then the
resolved Maven artifacts, including the Kotlin stdlib and what frameworks add. The package's own
classes are not part of it. Resolution respects kargo.lock, --locked and --offline. Without
--target, the JVM target is used, or the package's only Wasm, JS or Native target.")]
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
        /// Append each message from the editor to this file, with how long each request took to answer
        #[arg(long, value_name = "PATH")]
        log_file: Option<PathBuf>,
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
    /// Check the JDK, toolchains, SDKs, network and project setup for problems
    #[command(long_about = "Check the machine and the project for what keeps builds from working.

Checks the Kargo home (writable, free space), the JDK (found or pinned, new enough, supported by the
project's Kotlin version, JAVA_HOME), the installed toolchains (incomplete installs), the Android SDK,
Gradle and adb, Node.js, a browser and wasm-opt for Wasm and Kotlin/JS, the proxy, certificates,
credentials and whether every repository answers, and, inside a project, the manifest warnings, the
build lock and whether kargo.lock is in sync. Outside a project only the machine is checked.

Each check is ok, a warning or an error, with a one-line fix. Exits with 1 when a check fails.
--offline skips contacting the repositories.")]
    Doctor {
        /// Make the safe fixes: remove toolchains whose install was interrupted, so they are installed again
        #[arg(long)]
        fix: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t)]
        format: FormatArg,
        /// Print the report as a Markdown block to paste into a bug report, with your home directory shortened to `~`
        #[arg(long, conflicts_with = "format")]
        bug_report: bool,
    },
    /// Print the Kargo version (with -v, also the release and the target it was built for)
    Version,
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
    ops::self_update::remove_stale_binary();
    let cli = parse();
    let verbosity = if cli.global.verbose {
        Verbosity::Verbose
    } else if cli.global.quiet {
        Verbosity::Quiet
    } else {
        Verbosity::Normal
    };
    cli.global.color.apply();
    let shell = Shell::new(verbosity);
    match execute(cli, &shell) {
        Ok(code) => match u8::try_from(code) {
            Ok(code) => ExitCode::from(code),
            // A program's full status (Windows exit codes are 32 bits) cannot go through `ExitCode`.
            Err(_) => std::process::exit(code),
        },
        Err(err) => {
            report_error(&shell, &err);
            ExitCode::from(FAILURE as u8)
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
    if wants_version(&args) {
        print_version(&args);
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
    if let Some(name) = matches.subcommand_name() {
        kargo_util::flock::set_command(name);
    }
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

/// `kargo --version` or `kargo -V`, possibly with other global flags. With a command after it, clap reports the flag as unexpected.
fn wants_version(args: &[std::ffi::OsString]) -> bool {
    let words: Vec<&str> = args.iter().skip(1).filter_map(|a| a.to_str()).collect();
    words.iter().any(|w| matches!(*w, "--version" | "-V" | "-vV")) && words.iter().all(|w| w.starts_with('-'))
}

/// `kargo --version`: the version on stdout like clap prints it, then, on stderr, a note when a newer release exists (at most
/// once a day; see `KARGO_NO_UPDATE_CHECK`).
fn print_version(args: &[std::ffi::OsString]) {
    let offline =
        args.iter().any(|a| a == "--offline") || std::env::var("KARGO_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false");
    let verbose = args.iter().any(|a| a == "-vV" || a == "-v" || a == "--verbose");
    let quiet = args.iter().any(|a| a == "-q" || a == "--quiet");
    show_version(verbose, quiet, offline);
}

/// The output of `kargo --version` and `kargo version`.
fn show_version(verbose: bool, quiet: bool, offline: bool) {
    println!("kargo {}", env!("CARGO_PKG_VERSION"));
    if verbose {
        println!(
            "release: {}
host: {}",
            env!("CARGO_PKG_VERSION"),
            ops::self_update::TARGET
        );
    }
    let verbosity = if quiet { Verbosity::Quiet } else { Verbosity::Normal };
    let shell = Shell::new(verbosity);
    if verbosity == Verbosity::Quiet {
        return;
    }
    if let Ok(config) = Config::from_env(shell.clone()).map(|c| c.with_offline(offline))
        && let Some(note) = ops::self_update::update_notice(&config)
    {
        shell.note(note);
    }
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
    warnings::emit(shell, project, ws, warnings::stale_lockfile(ws));
}

fn report_error(shell: &Shell, err: &anyhow::Error) {
    shell.error(err);
    for cause in err.chain().skip(1) {
        eprintln!("\nCaused by:\n  {cause}");
    }
    let help = help::help_lines(err);
    if !help.is_empty() {
        eprintln!();
    }
    for line in help {
        shell.help(line);
    }
    if kargo_compiler::diagnostics::is_json() && !BUILD_FINISHED.load(std::sync::atomic::Ordering::Relaxed) {
        kargo_compiler::diagnostics::emit(&error_message(err));
    }
}

/// Whether `build-finished` was printed, which already says the command failed.
static BUILD_FINISHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Adds the advice of a failure to a JSON message: `help` and `suggestions`, each only when there is one.
fn advice_fields(err: &anyhow::Error, message: &mut serde_json::Value) {
    let lines = help::help_lines(err);
    if !lines.is_empty() {
        message["help"] = serde_json::json!(lines.join("\n"));
    }
    if let Some(advice) = help::advice(err).filter(|a| !a.suggestions().is_empty()) {
        message["suggestions"] = serde_json::json!(advice.suggestions());
    }
}

/// The `error` message of `--message-format json` for a failure before (or outside) a build.
fn error_message(err: &anyhow::Error) -> serde_json::Value {
    let mut message = serde_json::json!({ "reason": "error", "message": format!("{err:#}") });
    advice_fields(err, &mut message);
    message
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

/// How many packages and targets build at the same time: `--jobs` or `KARGO_JOBS` (`flag`), else
/// `[build] jobs` of kargo.toml (`[workspace.build]` in a workspace), else `[build] jobs` of
/// config.toml; `None` when none of them sets it. Also sets the limit of the targets and Native
/// platforms a package builds at the same time ([`kargo_util::jobs::set`]), `auto` without one.
fn resolve_jobs(config: &Config, project: &Project, flag: Option<Jobs>) -> Option<Jobs> {
    let manifest = match project.workspace_manifest() {
        Some(workspace) => workspace.build.jobs,
        None => project.packages().iter().find(|p| p.root() == project.root()).and_then(|p| p.manifest().build.jobs),
    };
    let jobs = flag.or(manifest).or(config.settings().jobs());
    kargo_util::jobs::set(jobs.unwrap_or(Jobs::Auto));
    jobs
}

/// What [`build_packages`] does besides running the operation.
#[derive(Clone, Copy)]
struct Packages {
    /// `--jobs` (or `KARGO_JOBS`).
    jobs: Option<Jobs>,
    /// Without a number of jobs from the flag, the environment, kargo.toml or config.toml, build
    /// one package at a time instead of [`kargo_util::jobs::auto`], so the output of each stays together.
    one_by_default: bool,
    /// Order packages after the ones their tests use too.
    dev: bool,
    /// With `--keep-going` / `--no-fail-fast`, the verb of the summary ("build"): a failed package
    /// no longer stops the others, only the packages that depend on it.
    keep_going: Option<&'static str>,
}

/// Runs `op` for every selected package, sharing one lockfile and the toolchains: in dependency
/// order, with up to `jobs` independent packages at the same time. `dev` orders packages after the
/// ones their tests use too. The first failure (in dependency order) is returned; the others are
/// printed. With `keep_going` every failure is printed, the packages that depend on a failed one
/// are skipped with a note, and the error is a summary of the packages.
fn build_packages<T: Send>(
    config: &Config,
    project: &Project,
    selection: &Selection,
    global: &GlobalArgs,
    profile: ProfileKind,
    packages: Packages,
    op: impl Fn(&BuildContext) -> Result<T> + Sync,
) -> Result<Vec<T>> {
    let selected = project.select(selection)?;
    let ctx = context(config, project, selected[0], global, profile)?;
    let order = project.dependency_order(&selected, packages.dev);
    for &index in &order {
        warn_manifest(config.shell(), project, project.package(index));
    }
    ctx.select(&order);
    let jobs = match resolve_jobs(config, project, packages.jobs) {
        Some(jobs) => jobs.count(),
        None if packages.one_by_default => 1,
        None => kargo_ops::ops::default_jobs(),
    };
    config.shell().verbose("Jobs", format!("{jobs} packages at a time, {} targets each", kargo_util::jobs::limit()));
    let keep_going = packages.keep_going.is_some();
    let results = kargo_ops::ops::build_graph(project, &order, packages.dev, jobs, keep_going, |index| op(&ctx.for_package(index)));
    let name = |k: usize| project.package(order[k]).name().to_owned();
    let mut out = Vec::new();
    let mut first_error = None;
    let (mut failed, mut skipped) = (Vec::new(), Vec::new());
    for (k, result) in results.into_iter().enumerate() {
        match result {
            Some(Ok(value)) => out.push(value),
            Some(Err(e)) => {
                failed.push(name(k));
                if first_error.is_none() { first_error = Some(e) } else { report_error(config.shell(), &e) }
            }
            None if keep_going => skipped.push(name(k)),
            None => {}
        }
    }
    let Some(first) = first_error else { return Ok(out) };
    let Some(action) = packages.keep_going else { return Err(first) };
    if failed.len() == 1 && out.is_empty() && skipped.is_empty() {
        return Err(first);
    }
    report_error(config.shell(), &first);
    for package in &skipped {
        config.shell().note(format!("skipped `{package}`: a package it depends on failed"));
    }
    let total = failed.len() + skipped.len() + out.len();
    let mut summary = format!("could not {action} {} of {total} packages ({})", failed.len(), failed.join(", "));
    if !skipped.is_empty() {
        summary.push_str(&format!("; {} skipped", skipped.len()));
    }
    if !out.is_empty() {
        summary.push_str(&format!("; {} succeeded", out.len()));
    }
    bail!("{summary}")
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

/// Splits the words `kargo run` got before `--` into the file to run and the program's arguments.
/// The first word is the file when it names an existing `.kt` or `.kts` file; otherwise every word
/// is an argument, as are the words after `--`.
fn split_run_words(mut words: Vec<String>, args: Vec<String>) -> (Option<PathBuf>, Vec<String>) {
    let is_file = words.first().is_some_and(|w| {
        let path = Path::new(w);
        matches!(path.extension().and_then(|e| e.to_str()), Some("kt" | "kts")) && path.is_file()
    });
    let file = is_file.then(|| PathBuf::from(words.remove(0)));
    words.extend(args);
    (file, words)
}

/// Whether `kargo run <file>` compiles the file against a project: a `.kt` file without a
/// `/* kargo ... */` front matter, with a `kargo.toml` above it (or `--manifest-path`). Scripts and
/// other files run on their own, as `kargo script` runs them.
fn compiled_in_project(file: &Path, manifest_path: Option<&Path>) -> bool {
    if file.extension().is_none_or(|e| e != "kt") {
        return false;
    }
    // An unreadable file or a broken front matter is reported by `kargo script`.
    let Ok(text) = std::fs::read_to_string(file) else { return false };
    if !matches!(kargo_manifest::script::parse_kt_front_matter(&text), Ok(None)) {
        return false;
    }
    let dir = kargo_util::fs::dunce_canonicalize(file).parent().map(Path::to_path_buf).unwrap_or_default();
    manifest_path.is_some() || Workspace::find_manifest(&dir).is_ok()
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
        Packages { jobs: build.jobs.jobs, one_by_default: true, dev: true, keep_going: opts.no_fail_fast.then_some("test") },
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
        Start::Serve(site) => running.serve(*site, config.shell()),
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
    if global.require_checksums {
        args.push("--require-checksums".to_owned());
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
/// The profile `kargo bench` builds with: the one asked for, else `[profile.bench]` when every selected package
/// defines it, else `release`. Measuring unoptimized code tells little.
fn bench_profile(project: &Project, build: &BuildArgs) -> Result<ProfileKind> {
    if build.release || build.profile.is_some() {
        return Ok(profile_kind(build.release, build.profile.as_deref()));
    }
    let selected = project.select(&build.packages.selection())?;
    let defined = selected.iter().all(|&index| project.package(index).manifest().profile("bench").is_some());
    Ok(if defined { ProfileKind::from_name("bench") } else { ProfileKind::Release })
}

/// Reads `--warmup` and `--iterations`.
fn parse_rounds(text: &str) -> std::result::Result<BenchRounds, String> {
    BenchRounds::parse("the value", text).map_err(|e| format!("{e:#}"))
}

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
    let mut frameworks = args.frameworks;
    if args.bench && !frameworks.iter().any(|f| f == "benchmark") {
        frameworks.push("benchmark".to_owned());
    }
    Ok(NewOptions { name, group: args.group, lib: args.lib, targets, frameworks, kotlin_version: args.kotlin, vcs: !args.no_vcs })
}

/// `kargo new` and `kargo init`: a package in `dir` (added to the members of the workspace it is
/// in, if any), or with `--workspace` an empty workspace.
fn create_package(shell: &Shell, dir: &Path, args: NewArgs) -> Result<i32> {
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
    if ops::new::ios_disabled(&opts) {
        shell.note("the iOS target is commented out in kargo.toml: iOS frameworks need macOS with Xcode");
    }
    if let Some(root) = workspace
        && let Some(member) = ops::new::add_to_workspace(&root, dir)?
    {
        shell.status("Adding", format!("`{member}` to the members of the workspace at {}", root.display()));
    }
    Ok(0)
}

/// `kargo import`: converts the Maven or Gradle project in `path` and, unless `no_build`, builds it.
fn import_project(
    config: &Config,
    global: &GlobalArgs,
    path: Option<PathBuf>,
    dry_run: bool,
    no_build: bool,
    from: Option<ImportSource>,
) -> Result<i32> {
    let shell = config.shell();
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let root = match path {
        Some(path) => std::path::absolute(cwd.join(path))?,
        None => cwd,
    };
    let plan = ops::import::plan(config, &root, &ops::import::ImportOptions { from: from.map(Into::into) })?;
    if dry_run {
        print!("{}", plan.describe());
        shell.status("Dry run", format!("{} project not changed; run `kargo import` to apply the plan above", plan.build.name()));
        ops::import::report(shell, &plan);
        return Ok(0);
    }
    ops::import::apply(shell, &root, &plan)?;
    let packages = match plan.packages.as_slice() {
        [one] => format!("package `{one}`"),
        many => format!("{} packages ({})", many.len(), many.join(", ")),
    };
    shell.status("Imported", format!("{} project as {packages}", plan.build.name()));
    ops::import::report(shell, &plan);
    if no_build {
        return Ok(0);
    }
    let project = Project::discover(&root, None)?;
    let opts = BuildOptions::default();
    build_packages(
        config,
        &project,
        &Selection::default(),
        global,
        ProfileKind::Dev,
        Packages { jobs: None, one_by_default: false, dev: false, keep_going: None },
        |ctx| ops::build::build(ctx, &opts),
    )
    .context("the project was imported, but `kargo build` failed; fix what it reports, or see the items above that need manual work")?;
    Ok(0)
}

fn execute(cli: Cli, shell: &Shell) -> Result<i32> {
    let global = cli.global;
    let config = Config::from_env(shell.clone())?
        .with_offline(global.offline)
        .with_refresh(global.refresh)
        .with_require_checksums(global.require_checksums)
        .with_system_gradle(global.system_gradle);
    let config = with_settings(config, &global, &cli.command)?;
    let auto_install = auto_install_env()?.unwrap_or(config.settings().auto_install());
    let config = config.with_auto_install(auto_install).with_locked(global.locked);

    // Commands that may change kargo.toml or kargo.lock keep the files for language servers current.
    let refreshes_editor_files = matches!(
        cli.command,
        Command::Build { .. }
            | Command::Check { .. }
            | Command::Test { .. }
            | Command::Bench { .. }
            | Command::Fetch { .. }
            | Command::Add(_)
            | Command::Remove { .. }
            | Command::Update { .. }
            | Command::Upgrade { .. }
            | Command::Audit { .. }
            | Command::Target { .. }
    );
    let result = run_command(cli.command, &global, &config, shell);
    if refreshes_editor_files && result.is_ok() {
        refresh_editor_files(&config, &global, shell);
    }
    result
}

/// `KARGO_AUTO_INSTALL`, when it is set: whether commands may download the toolchains they need.
fn auto_install_env() -> Result<Option<bool>> {
    let Some(value) = std::env::var_os(kargo_util::config::AUTO_INSTALL_ENV).filter(|v| !v.is_empty()) else { return Ok(None) };
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        other => bail!("{} must be `1` or `0`, got `{other}`", kargo_util::config::AUTO_INSTALL_ENV),
    }
}

/// Applies `~/.kargo/config.toml` and the project's `.kargo/config.toml`, and with `--vendored` (or
/// `[source] vendored = true`) the vendored repository. `kargo vendor` itself always uses the real
/// repositories.
fn with_settings(config: Config, global: &GlobalArgs, command: &Command) -> Result<Config> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let start = match global.manifest_path.as_deref().and_then(Path::parent) {
        Some(dir) => cwd.join(dir),
        None => cwd,
    };
    let settings = kargo_util::settings::Settings::load(config.home(), Some(&start))?;
    let config = config.with_settings(settings)?;
    if !(global.vendored || config.settings().vendored()) || matches!(command, Command::Vendor { .. }) {
        return Ok(config);
    }
    let project = match load_project(global) {
        Ok(project) => project,
        // Outside a project there is nothing to resolve; only an explicit flag is worth an error.
        Err(e) if global.vendored => return Err(e.context("`--vendored` needs a project")),
        Err(_) => return Ok(config),
    };
    let dir = project.root().join(config.settings().vendor_dir());
    if !dir.is_dir() {
        bail!("there is no vendored repository at {}; run `kargo vendor` first", dir.display());
    }
    Ok(config.with_vendor_dir(Some(dir)))
}

/// Where Kargo fetches `repo` from: the vendored repository, its mirror, or its own URL.
fn effective_url(config: &Config, repo: &kargo_maven::Repository) -> String {
    match config.vendor_dir() {
        Some(dir) => dir.display().to_string(),
        None => kargo_maven::repo::effective_repositories(config, std::slice::from_ref(repo)).remove(0).url,
    }
}

fn run_command(command: Command, global: &GlobalArgs, config: &Config, shell: &Shell) -> Result<i32> {
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
        Command::InitCi { system, force, junit } => {
            let project = load_project(global)?;
            let report = ops::ci::init_ci(&project, &ops::ci::CiOptions { system: system.into(), force, junit })?;
            shell.status("Created", report.path.display());
            for step in &report.left_out {
                shell.note(format!("left out {step}"));
            }
            Ok(0)
        }
        Command::Import { path, dry_run, no_build, from } => import_project(config, global, path, dry_run, no_build, from),
        Command::Build { build, keep_going, xcframework } => {
            set_message_format(build.message_format);
            let project = load_project(global)?;
            let opts = BuildOptions { keep_going, xcframework, ..build.options() };
            let results = build_packages(
                config,
                &project,
                &build.packages.selection(),
                global,
                opts.profile.clone(),
                Packages { jobs: build.jobs.jobs, one_by_default: false, dev: false, keep_going: keep_going.then_some("build") },
                |ctx| ops::build::build(ctx, &opts),
            );
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
                Packages { jobs: build.jobs.jobs, one_by_default: false, dev: tests, keep_going: keep_going.then_some("check") },
                |ctx| ops::check::check(ctx, &opts),
            );
            build_finished(build.message_format, results.map(|_| ()))?;
            Ok(0)
        }
        Command::Run(RunArgs {
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
            android,
            message_format,
            package,
            jobs,
            env,
            watch,
            hot_reload,
            watch_args,
            expr,
            words,
            args,
        }) => {
            set_message_format(message_format);
            let watch = watch_options(watch || hot_reload, &watch_args)?;
            let (file, args) = if expr.is_some() { (None, [words, args].concat()) } else { split_run_words(words, args) };
            if let Some(file) = &file
                && let Some(flag) = [(main.is_some(), "--main"), (bin.is_some(), "--bin"), (example.is_some(), "--example")]
                    .into_iter()
                    .find_map(|(given, flag)| given.then_some(flag))
            {
                bail!("`{flag}` picks a program of the project; `kargo run {}` runs that file instead, so use one of them", file.display());
            }
            // A script, or a file with no project or with its own front matter, runs as `kargo script` runs it.
            let script = match (expr, &file) {
                (Some(code), _) => Some(kargo_ops::script::Source::Expression(code)),
                (None, Some(file)) if !compiled_in_project(file, global.manifest_path.as_deref()) => {
                    Some(kargo_ops::script::Source::File(file.clone()))
                }
                _ => None,
            };
            if let Some(source) = script {
                let given = [
                    (release, "--release"),
                    (profile.is_some(), "--profile"),
                    (target.is_some_and(|t| !matches!(t, TargetArg::Jvm)), "--target"),
                    (port.is_some(), "--port"),
                    (open, "--open"),
                    (debug.is_some(), "--debug"),
                    (native_agent, "--native-agent"),
                    (!flavors.is_empty(), "--flavor"),
                    (android.is_set(), "--no-logcat, --stop-emulator or an emulator option"),
                    (package.package.is_some(), "--package"),
                    (watch.is_some(), if hot_reload { "--hot-reload" } else { "--watch" }),
                    (!env.env_file.is_empty(), "--env-file"),
                    (env.no_env_file, "--no-env-file"),
                ];
                if let Some((_, flag)) = given.into_iter().find(|(given, _)| *given) {
                    let what = match &source {
                        kargo_ops::script::Source::File(file) => file.display().to_string(),
                        kargo_ops::script::Source::Expression(_) => "`-e` code".to_owned(),
                    };
                    bail!("`{flag}` applies to a project's program, but {what} runs on its own like `kargo script`");
                }
                return kargo_ops::script::run(config, &source, &args);
            }
            if file.is_some() && watch.is_some() {
                bail!("`--watch` reruns the project's program when the project changes; it does not run a single file");
            }
            let file = file.map(|f| kargo_util::fs::dunce_canonicalize(&f));
            let project = match &file {
                // The project is the one the file is in, wherever `kargo run` runs.
                Some(file) => Project::discover(file.parent().unwrap_or(Path::new(".")), global.manifest_path.as_deref())?,
                None => load_project(global)?,
            };
            let index = project.select_one(&package.selection(), "`kargo run`")?;
            warn_manifest(shell, &project, project.package(index));
            resolve_jobs(config, &project, jobs.jobs);
            let profile = profile_kind(release, profile.as_deref());
            let ctx = context(config, &project, index, global, profile.clone())?;
            let program = match (file, main, bin, example) {
                (Some(file), ..) => Program::File(file),
                (_, Some(main), _, _) => Program::Main(main),
                (_, _, Some(bin), _) => Program::Bin(bin),
                (_, _, _, Some(example)) => Program::Example(example),
                _ => Program::Default,
            };
            let mut opts = RunOptions {
                target: target.map(Into::into),
                profile,
                program,
                args,
                port,
                open,
                debug,
                native_agent,
                flavors,
                android: android.options(),
                env: env.options(),
                hot_reload,
            };
            let Some(watch) = watch else {
                let code = ops::run::run(&ctx, &opts)?;
                return Ok(code);
            };
            ops::run::check_watchable(&ctx, &opts)?;
            ops::run::choose_watch_device(&ctx, &mut opts)?;
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
            kargo_ops::script::run(config, &source, &args)
        }
        Command::Test {
            build,
            filter,
            no_run,
            no_fail_fast,
            include_tag,
            exclude_tag,
            failed,
            against_min,
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
                against_min,
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
        Command::Bench { build, filter, no_run, save, baseline, format, warmup, iterations, forks, mode, time_unit, env } => {
            set_message_format(build.message_format);
            let project = load_project(global)?;
            let opts = BenchOptions {
                build: BuildOptions { profile: bench_profile(&project, &build)?, ..build.options() },
                env: env.options(),
                filter,
                no_run,
                save,
                baseline,
                json: format == FormatArg::Json,
                overrides: BenchOverrides {
                    warmup,
                    iterations,
                    forks,
                    mode: mode.as_deref().map(str::parse::<BenchMode>).transpose()?,
                    time_unit,
                },
            };
            build_packages(
                config,
                &project,
                &build.packages.selection(),
                global,
                opts.build.profile.clone(),
                Packages { jobs: build.jobs.jobs, one_by_default: true, dev: true, keep_going: None },
                |ctx| ops::bench::bench(ctx, &opts),
            )?;
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
        Command::Doc { targets, open, packages, jobs } => {
            let project = load_project(global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let open = std::sync::atomic::AtomicBool::new(open);
            let one_by_one = Packages { jobs: jobs.jobs, one_by_default: true, dev: false, keep_going: None };
            build_packages(config, &project, &packages.selection(), global, ProfileKind::Dev, one_by_one, |ctx| {
                // Only the first package's documentation opens in the browser.
                let opts = DocOptions { targets: targets.clone(), open: open.swap(false, std::sync::atomic::Ordering::Relaxed) };
                ops::doc::doc(ctx, &opts)
            })?;
            Ok(0)
        }
        Command::Preview { names, list, out_dir, width, height, density, release, profile, package } => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo preview`")?;
            let profile = profile_kind(release, profile.as_deref());
            let ctx = context(config, &project, index, global, profile.clone())?;
            if list {
                let root = ctx.ws.root();
                for p in ops::preview::list(&ctx)? {
                    let file = p.file.strip_prefix(root).unwrap_or(&p.file);
                    println!("{}	{}:{}", p.qualified_name(), file.display(), p.line);
                }
                return Ok(0);
            }
            if !density.is_finite() || density <= 0.0 || width == 0 || height == 0 {
                bail!("`--width`, `--height` and `--density` must be greater than zero");
            }
            let opts = ops::preview::PreviewOptions { profile, names, out_dir, width, height, density };
            for file in ops::preview::render(&ctx, &opts)? {
                let shown = file.strip_prefix(ctx.ws.root()).unwrap_or(&file);
                shell.status("Rendered", shown.display());
            }
            Ok(0)
        }
        Command::Fmt { check, packages, jobs } => {
            let project = load_project(global)?;
            let one_by_one = Packages { jobs: jobs.jobs, one_by_default: true, dev: false, keep_going: None };
            let ok = build_packages(config, &project, &packages.selection(), global, ProfileKind::Dev, one_by_one, |ctx| {
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
        Command::Lint { create_baseline, packages, jobs } => {
            let project = load_project(global)?;
            let one_by_one = Packages { jobs: jobs.jobs, one_by_default: true, dev: false, keep_going: None };
            let ok = build_packages(config, &project, &packages.selection(), global, ProfileKind::Dev, one_by_one, |ctx| {
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
        Command::Add(AddArgs {
            coordinates,
            path,
            dev,
            target,
            npm,
            types,
            scope,
            bom,
            pre,
            no_verify,
            frameworks,
            cinterop,
            header,
            pkg_config,
            package,
        }) => {
            if cinterop.is_none() && (!header.is_empty() || pkg_config.is_some()) {
                bail!("`--header` and `--pkg-config` describe a C library; use them with `--cinterop <NAME>`");
            }
            let ws = load_one(global, &package, "`kargo add`")?;
            if npm {
                let opts = ops::add_npm::NpmAddOptions {
                    specs: coordinates,
                    target: target.map(Into::into),
                    dev,
                    types,
                    prerelease: pre,
                    no_verify,
                };
                ops::add_npm::add(config, &ws, &opts)?;
                return Ok(0);
            }
            if dev && target.is_some() {
                bail!(
                    "`--dev` and `--target` cannot be used together; use `--dev` for [dev-dependencies] or `--target <target>` for [target.<target>.dependencies]"
                );
            }
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
        Command::Remove { names, dev, target, npm, package } => {
            let ws = load_one(global, &package, "`kargo remove`")?;
            if npm {
                ops::add_npm::remove(config, &ws, &names, target.map(Into::into), dev)?;
                return Ok(0);
            }
            if dev && target.is_some() {
                bail!(
                    "`--dev` and `--target` cannot be used together; use `--dev` for [dev-dependencies] or `--target <target>` for [target.<target>.dependencies]"
                );
            }
            ops::add::remove(config, &ws, &names, section(dev, target))?;
            Ok(0)
        }
        Command::Target { command } => target(shell, global, command),
        Command::Tree { target, configuration, depth, duplicates, invert, packages, format } => {
            let project = load_project(global)?;
            let configuration = configuration.into();
            let invert: Option<ops::why::Query> = invert.as_deref().map(str::parse).transpose()?;
            if format == FormatArg::Json {
                let mut documents = Vec::new();
                for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                    let mut trees = Vec::new();
                    for t in tree_targets(ctx, target) {
                        for (label, res) in ops::resolution_for(ctx, t, configuration)? {
                            let mut tree = ops::tree::json(&label, &res);
                            if matches!(t, TargetKind::Wasm | TargetKind::Js) {
                                let tests = matches!(configuration, Configuration::Test);
                                let origins = kargo_ops::npm::origins(ctx.ws.manifest(), t, &res, tests)?;
                                tree["npm"] = ops::tree::json_npm(&origins);
                            }
                            trees.push(tree);
                        }
                    }
                    let configuration_name = configuration_name(configuration);
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
            let found = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                let targets = tree_targets(ctx, target);
                let pkg = &ctx.ws.manifest().package;
                let mut found = false;
                for t in targets {
                    let locals = ops::tree::path_dependencies(ctx, t, configuration)?;
                    for (label, res) in ops::resolution_for(ctx, t, configuration)? {
                        if let Some(query) = &invert {
                            if query.matches(&res) {
                                found = true;
                                let title = format!("{} v{} ({label})", pkg.name, pkg.version);
                                print!("{}", ops::why::render_inverted(&title, &res, &query.module, depth));
                            }
                        } else if duplicates {
                            for line in ops::tree::conflicts(&res)? {
                                println!("{line}");
                            }
                        } else {
                            print!("{}", ops::tree::render(&format!("{} v{} ({label})", pkg.name, pkg.version), &locals, &res, depth));
                            if matches!(t, TargetKind::Wasm | TargetKind::Js) {
                                let tests = matches!(configuration, Configuration::Test);
                                print!("{}", ops::tree::render_npm(&kargo_ops::npm::origins(ctx.ws.manifest(), t, &res, tests)?));
                            }
                        }
                    }
                }
                Ok(found)
            })?;
            if let Some(query) = &invert
                && !found.contains(&true)
            {
                bail!("{}", not_found(query, configuration));
            }
            Ok(0)
        }
        Command::Inspect(InspectArgs { module, classes, configuration, visibility, package }) => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo inspect`")?;
            let ctx = context(config, &project, index, global, ProfileKind::Dev)?;
            let configuration: Configuration = configuration.into();
            let visibility: ops::inspect::Visibility = visibility.into();
            let query: ops::why::Query = module.parse()?;
            let mut found = None;
            for (label, resolution) in ops::inspect::resolutions(&ctx, configuration)? {
                if let Some(located) = ops::inspect::locate(&label, &resolution, &query)? {
                    found = Some(located);
                    break;
                }
            }
            let Some(located) = found else { bail!("{}", not_found(&query, configuration)) };
            shell.status(
                "Inspecting",
                format!(
                    "{}:{} ({}, {}) {}",
                    located.module,
                    located.version,
                    located.target,
                    configuration_name(configuration),
                    located.jar.display()
                ),
            );
            let available = ops::inspect::classes(&located.jar, visibility)?;
            if classes.is_empty() {
                for class in &available {
                    println!("{}", ops::inspect::source_name(class));
                }
                shell.note(format!("{} classes; name one to print its members: `kargo inspect {} <class>`", available.len(), query.module));
                return Ok(0);
            }
            let all = ops::inspect::classes(&located.jar, ops::inspect::Visibility::Private)?;
            let wanted = classes.iter().map(|c| ops::inspect::find_class(&all, c)).collect::<Result<Vec<_>>>()?;
            ops::inspect::javap(&ctx, &located, &wanted, visibility)?;
            Ok(0)
        }
        Command::Why(WhyArgs { module, target, configuration, all_paths, packages, format }) => {
            let project = load_project(global)?;
            let configuration: Configuration = configuration.into();
            let query: ops::why::Query = module.parse()?;
            let configuration_name = configuration_name(configuration);
            let documents = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                let pkg = &ctx.ws.manifest().package;
                let mut answers = Vec::new();
                for t in tree_targets(ctx, target) {
                    for (label, res, explanation) in ops::explanation_for(ctx, t, configuration)? {
                        if let Some(answer) = ops::why::answer(&label, &res, &explanation, &query, all_paths) {
                            answers.push(answer);
                        }
                    }
                }
                if format != FormatArg::Json {
                    for answer in &answers {
                        print!("{}", ops::why::render(&format!("{} v{}", pkg.name, pkg.version), configuration_name, answer));
                    }
                }
                Ok((
                    !answers.is_empty(),
                    serde_json::json!({
                        "package": { "name": pkg.name, "version": pkg.version },
                        "configuration": configuration_name,
                        "module": query.module.to_string(),
                        "results": answers.iter().map(ops::why::json).collect::<Vec<_>>(),
                    }),
                ))
            })?;
            if !documents.iter().any(|(found, _)| *found) {
                bail!("{}", not_found(&query, configuration));
            }
            if format == FormatArg::Json {
                // One package prints one object; a workspace prints one per member.
                let mut documents: Vec<serde_json::Value> = documents.into_iter().map(|(_, d)| d).collect();
                let output = if documents.len() == 1 { documents.remove(0) } else { serde_json::Value::Array(documents) };
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            Ok(0)
        }
        Command::Fetch { targets, sources, tools, packages } => {
            let project = load_project(global)?;
            let targets: Vec<TargetKind> = targets.into_iter().map(Into::into).collect();
            let found = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                ops::fetch(ctx, &targets, tools)?;
                if !sources {
                    return Ok(ops::metadata::FetchedDocs::default());
                }
                let selected = ops::build::select_targets(ctx, &targets)?;
                ops::metadata::fetch_sources(ctx, &selected)
            })?;
            if sources {
                let found = found.into_iter().fold(ops::metadata::FetchedDocs::default(), |a, b| a + b);
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                let mut message = format!("{} sources jar{}", found.sources, plural(found.sources));
                if found.javadoc > 0 {
                    message.push_str(&format!(" and {} javadoc jar{}", found.javadoc, plural(found.javadoc)));
                }
                shell.status("Fetched", message);
            }
            Ok(0)
        }
        Command::Generate { names, release, profile, targets, force, packages } => {
            let project = load_project(global)?;
            let opts = kargo_ops::generate::GenerateOptions { names, targets: targets.into_iter().map(Into::into).collect(), force };
            let profile = profile_kind(release, profile.as_deref());
            let reports = for_each_package(config, &project, &packages.selection(), global, profile, |ctx| {
                // In a workspace, a member without steps has nothing to do; alone it is a mistake worth saying.
                if project.is_workspace() && ctx.ws.manifest().generators.is_empty() {
                    return Ok(Vec::new());
                }
                kargo_ops::generate::generate(ctx, &opts)
            })?;
            for report in reports.iter().flatten().filter(|r| !r.ran) {
                shell.status("Fresh", format!("`{}`: the generated files are up to date", report.name));
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
        Command::Lsp { server, server_path, klib_targets, log_file, stdio: _ } => {
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
            let opts =
                kargo_lsp::Options { server, server_path, targets, builtin_commands: builtin_commands(), search_url: None, log_file };
            let clean = kargo_lsp::run(config.clone(), opts)?;
            Ok(if clean { 0 } else { 1 })
        }
        Command::Dap { adapter_path } => {
            let clean = kargo_lsp::dap::run(config.clone(), kargo_lsp::dap::Options { adapter_path })?;
            Ok(if clean { 0 } else { 1 })
        }
        Command::Audit { targets, deny, tooling, fix, packages, format } => {
            let project = load_project(global)?;
            let opts = ops::audit::AuditOptions { targets: targets.into_iter().map(Into::into).collect(), tooling };
            let reports = for_each_package(config, &project, &packages.selection(), global, ProfileKind::Dev, |ctx| {
                let report = ops::audit::audit(ctx, &opts)?;
                let deny = deny.map(kargo_manifest::DenyLevel::from).or(ctx.ws.manifest().audit.deny).unwrap_or_default();
                let fixed = if fix { ops::audit::fix(ctx, &report)? } else { 0 };
                Ok((report, deny, fixed))
            })?;
            for (report, _, _) in &reports {
                for note in &report.skipped {
                    shell.note(note);
                }
                for f in report.findings.iter().filter(|f| f.ignored.is_some()) {
                    let reason = f.ignored.as_ref().map(|i| i.reason.as_str()).unwrap_or_default();
                    shell.note(format!("ignoring {} in {} {}: {reason}", f.advisory.id, f.subject.query.name, f.subject.query.version));
                }
            }
            if format == FormatArg::Json {
                let mut documents: Vec<serde_json::Value> = reports.iter().map(|(r, deny, _)| ops::audit::json(r, *deny)).collect();
                // One package prints one object; a workspace prints one per member.
                let output = if documents.len() == 1 { documents.remove(0) } else { serde_json::Value::Array(documents) };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                for (report, _, _) in &reports {
                    print!("{}", ops::audit::render(report));
                }
            }
            let fixed: usize = reports.iter().map(|(_, _, f)| f).sum();
            if fixed > 0 {
                shell.status(
                    "Fixed",
                    format!("{fixed} {} in kargo.toml; run `kargo audit` again to check the result", plural(fixed, "version")),
                );
            }
            let audited: usize = reports.iter().map(|(r, _, _)| r.audited).sum();
            let ignored: usize = reports.iter().map(|(r, _, _)| r.findings.iter().filter(|f| f.ignored.is_some()).count()).sum();
            let reported: usize = reports.iter().map(|(r, _, _)| r.reported().count()).sum();
            let denied: Vec<(usize, kargo_manifest::DenyLevel)> =
                reports.iter().map(|(r, deny, _)| (r.denied(*deny).count(), *deny)).filter(|(n, _)| *n > 0).collect();
            if let Some(&(_, deny)) = denied.first() {
                let count: usize = denied.iter().map(|(n, _)| n).sum();
                shell.error(format!(
                    "{count} {} at or above the `{deny}` severity; upgrade the affected packages (`kargo audit --fix` raises the versions kargo.toml declares) or add the advisories to [audit] ignore with a reason",
                    plural(count, "advisory")
                ));
                return Ok(FAILURE);
            }
            let ignored = if ignored > 0 { format!(" ({ignored} ignored)") } else { String::new() };
            if reported > 0 {
                let deny = reports.first().map(|(_, deny, _)| *deny).unwrap_or_default();
                shell.warn(format!("{reported} {} below the `{deny}` severity{ignored}", plural(reported, "advisory")));
            } else {
                shell.status("Audited", format!("{audited} {}: no known advisories{ignored}", plural(audited, "package")));
            }
            Ok(0)
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
        Command::Vendor { path, toolchains, sync } => {
            let project = load_project(global)?;
            let relative = path.unwrap_or_else(|| config.settings().vendor_dir().to_path_buf());
            let dir = std::path::absolute(project.root().join(&relative))?;
            // First as `kargo fetch`, which brings kargo.lock up to date (or checks it with --locked).
            let earlier = {
                let ctx = context(config, &project, project.members()[0], global, ProfileKind::Dev)?;
                ops::vendor::fetch(&ctx, toolchains)?;
                ctx.client.served_files()
            };
            // Then at the locked versions, bypassing the lock, so every POM and module file is read again.
            let refresh = config.clone().with_refresh(true);
            let mut ctx = BuildContext::new(&refresh, &project, project.members()[0], ProfileKind::Dev)?;
            ctx.update = Some(kargo_ops::ops::context::UpdatePlan { previous: ctx.lockfile(), modules: Vec::new(), precise: None });
            ops::vendor::fetch(&ctx, toolchains)?;
            ops::vendor::fetch_metadata(&ctx)?;
            let summary = ops::vendor::write(&ctx, &ops::vendor::VendorOptions { dir: dir.clone(), toolchains, sync }, earlier)?;
            let mut what = format!("{} files into {} ({} copied", summary.files, dir.display(), summary.copied);
            if sync {
                what.push_str(&format!(", {} removed", summary.removed));
            }
            what.push(')');
            shell.status("Vendored", what);
            let directory = if relative == Path::new(kargo_util::settings::DEFAULT_VENDOR_DIR) {
                String::new()
            } else {
                format!("\ndirectory = \"{}\"", relative.display().to_string().replace('\\', "/"))
            };
            shell.note(format!(
                "to build from it, pass `--vendored`, or add this to {}:\n\n[source]\nvendored = true{directory}\n",
                Path::new(kargo_util::settings::PROJECT_DIR).join(kargo_util::settings::FILE_NAME).display()
            ));
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
        Command::Package { release, profile, target, bin, fat, optimize, format, desktop, platforms, message_format, package, jobs } => {
            set_message_format(message_format);
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo package`")?;
            warn_manifest(shell, &project, project.package(index));
            resolve_jobs(config, &project, jobs.jobs);
            let ctx = context(config, &project, index, global, profile_kind(release, profile.as_deref()))?;
            let opts = ops::PackageOptions {
                target: target.map(Into::into),
                bin,
                fat,
                optimize,
                format: format.map(Into::into),
                desktop,
                platforms,
            };
            let files = match ops::package(&ctx, &opts) {
                Ok(files) => files,
                Err(error) => {
                    if message_format == MessageFormatArg::Json {
                        kargo_compiler::diagnostics::emit(&serde_json::json!({ "reason": "package-finished", "success": false }));
                    }
                    return Err(error);
                }
            };
            for file in files {
                if message_format == MessageFormatArg::Json {
                    kargo_compiler::diagnostics::emit(
                        &serde_json::json!({ "reason": "package-artifact", "file": file.display().to_string() }),
                    );
                } else {
                    println!("{}", file.display());
                }
            }
            if message_format == MessageFormatArg::Json {
                kargo_compiler::diagnostics::emit(&serde_json::json!({ "reason": "package-finished", "success": true }));
            }
            Ok(0)
        }
        Command::Retrace { mapping, stack_trace, package } => {
            let project = load_project(global)?;
            let index = project.select_one(&package.selection(), "`kargo retrace`")?;
            let ctx = context(config, &project, index, global, ProfileKind::Dev)?;
            ops::packaging::retrace(&ctx, &mapping, stack_trace.as_deref())?;
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
        Command::Install { spec, path, git, tag, branch, rev, bins, target, format, repository, force, list, root, package } => {
            let layout = ops::install::Layout::new(config, root.as_deref());
            if list {
                for tool in ops::install::installed(&layout)? {
                    println!("{}:", tool.describe());
                    for binary in &tool.binaries {
                        println!("    {binary}");
                    }
                }
                return Ok(0);
            }
            let locked = global.locked;
            let options = |source| ops::install::InstallOptions {
                source,
                package: package.package.clone(),
                bins: bins.clone(),
                target: target.map(Into::into),
                format: format.into(),
                force,
                locked,
                root: root.clone(),
            };
            let opts = match (spec, path, git) {
                (_, Some(path), _) => options(ops::install::Source::Path { path }),
                (_, _, Some(url)) => options(ops::install::Source::Git { url, tag, branch, rev }),
                (Some(spec), _, _) if spec.contains(':') => {
                    let coordinate: kargo_maven::CoordSpec = spec.parse()?;
                    if coordinate.classifier.is_some() || coordinate.extension.is_some() {
                        bail!("`{spec}` names a classifier or an extension; install `group:artifact[:version]`");
                    }
                    options(ops::install::Source::Maven {
                        module: coordinate.module.to_string(),
                        version: coordinate.version,
                        repositories: repository,
                    })
                }
                // The name of an installed program: install it again from where it came from.
                (Some(name), _, _) => ops::install::recorded(&layout, &name)?.options(locked, root),
                (None, None, None) => bail!(
                    "nothing to install; give `--path <dir>` (`.` for this project), `--git <url>` or a `group:artifact` from Maven, \
                     or see what is installed with `kargo install --list`"
                ),
            };
            ops::install::install(config, &opts)?;
            Ok(0)
        }
        Command::Uninstall { root, names } => {
            ops::install::uninstall(config, &ops::install::Layout::new(config, root.as_deref()), &names)?;
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
        Command::Android(AndroidArgs { command: Some(command), .. }) => android(config, global, command),
        Command::Devices(DevicesArgs { format }) => android(config, global, AndroidCommand::Devices { format }),
        Command::Android(AndroidArgs { command: None, device, args }) => {
            let project = load_project(global)?;
            let index = project.select_one(&Selection::default(), "`kargo android`")?;
            let ctx = context(config, &project, index, global, ProfileKind::Dev)?;
            let device = device.or_else(|| std::env::var(kargo_ops::android_device::DEVICE_ENV).ok());
            let ok = kargo_ops::gradle::run_tasks(&ctx, &args, device.as_deref())?;
            Ok(if ok { 0 } else { FAILURE })
        }
        Command::Info { format } => {
            let project = load_project(global)?;
            // At the root of a workspace, the workspace; elsewhere, the package.
            let package = task_package(&project);
            match (format, package) {
                (FormatArg::Text, Some(index)) => print_info(config, &project, project.package(index))?,
                (FormatArg::Text, None) => print_workspace_info(&project),
                (FormatArg::Json, Some(index)) => {
                    println!("{}", serde_json::to_string_pretty(&info_json(config, project.package(index))?)?)
                }
                (FormatArg::Json, None) => println!("{}", serde_json::to_string_pretty(&workspace_info_json(&project))?),
            }
            Ok(0)
        }
        Command::Version => {
            show_version(global.verbose, global.quiet, global.offline);
            Ok(0)
        }
        Command::Doctor { fix, format, bug_report } => {
            let cwd = std::env::current_dir().context("failed to read the current directory")?;
            let loaded = load_project(global);
            // A manifest that does not load is a finding, not a reason to stop.
            let manifest = match &global.manifest_path {
                Some(path) => Some(cwd.join(path)),
                None => cwd.ancestors().map(|d| d.join(kargo_manifest::MANIFEST_NAME)).find(|p| p.is_file()),
            };
            let state = match (&loaded, manifest) {
                (Ok(project), _) => ops::doctor::ProjectState::Loaded(project),
                (Err(error), Some(manifest)) => ops::doctor::ProjectState::Broken { manifest, error },
                (Err(_), None) => ops::doctor::ProjectState::None,
            };
            let builtins = builtin_commands();
            let mut report = ops::doctor::diagnose(config, state, &|name| builtins.iter().any(|b| b == name));
            if fix {
                ops::doctor::fix(&mut report);
            }
            if bug_report {
                print!("{}", ops::doctor::bug_report(&report));
            } else if format == FormatArg::Json {
                println!("{}", serde_json::to_string_pretty(&ops::doctor::json(&report))?);
            } else {
                print!("{}", ops::doctor::render(&report));
            }
            Ok(if report.has_errors() { 1 } else { 0 })
        }
        Command::Completions { shell } => {
            complete::write_script(shell, &mut std::io::stdout().lock())?;
            Ok(0)
        }
        Command::Toolchain { command } => toolchain(config, global, command),
        Command::SelfManage { command } => {
            match command {
                SelfCommand::Update { version, check } => {
                    ops::self_update::update(config, &ops::self_update::UpdateOptions { version, check })?
                }
                SelfCommand::Uninstall { purge } => ops::self_update::uninstall(config, purge)?,
            }
            Ok(0)
        }
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

/// How `kargo toolchain install` gets the licenses of Android SDK packages accepted.
fn licenses(accept: bool) -> kargo_toolchain::android_sdk::Licenses {
    if accept { kargo_toolchain::android_sdk::Licenses::Accept } else { kargo_toolchain::android_sdk::Licenses::Ask }
}

/// `kargo android <devices|avd|emulator|connect|pair|disconnect>`.
fn android(config: &Config, global: &GlobalArgs, command: AndroidCommand) -> Result<i32> {
    use kargo_ops::android_device as device;
    // These commands also work outside a project, with the SDK of ANDROID_HOME.
    let project = load_project(global).ok();
    let manifest = project.as_ref().map(|p| p.package(p.current().unwrap_or(p.members()[0])).manifest());
    let toolbox = device::Toolbox::locate(config, manifest);
    let shell = config.shell();
    match command {
        AndroidCommand::Devices { format } => {
            let devices = device::list(&toolbox, shell)?;
            match format {
                FormatArg::Text => print!("{}", device::render_table(&devices)),
                FormatArg::Json => println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "devices": devices }))?),
            }
        }
        AndroidCommand::Avd { command: AvdCommand::List { format } } => {
            let avds = device::avds(&toolbox.avd_home());
            match format {
                FormatArg::Text if avds.is_empty() => {
                    println!("No virtual devices in {}; `kargo android avd create` makes one.", toolbox.avd_home().display())
                }
                FormatArg::Text => {
                    for avd in &avds {
                        let api = avd.api.map(|a| format!("API {a}")).unwrap_or_else(|| "-".to_owned());
                        println!("{:<28} {:<8} {}", avd.name, api, avd.image.as_deref().unwrap_or("-"));
                    }
                }
                FormatArg::Json => {
                    let list: Vec<_> = avds
                        .iter()
                        .map(|a| serde_json::json!({ "name": a.name, "api": a.api, "image": a.image, "hardware": a.hardware, "path": a.dir }))
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "avds": list }))?);
                }
            }
        }
        AndroidCommand::Avd { command: AvdCommand::Create { name, api, hardware, force, accept_licenses } } => {
            let new = device::NewAvd { name, api: api.unwrap_or_else(|| toolbox.default_api()), hardware, force };
            device::create_avd(&toolbox, shell, &new, licenses(accept_licenses))?;
        }
        AndroidCommand::Avd { command: AvdCommand::Delete { name } } => device::delete_avd(&toolbox, shell, &name)?,
        AndroidCommand::Emulator { command: EmulatorCommand::Start { avd, options } } => {
            let name = match avd {
                Some(name) => name,
                None => match device::avds(&toolbox.avd_home()).as_slice() {
                    [only] => only.name.clone(),
                    [] => bail!("there is no virtual device to start; `kargo android avd create` makes one"),
                    many => bail!("which virtual device? {}", many.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")),
                },
            };
            let serial = device::start_emulator(&toolbox, shell, &toolbox.adb()?, &name, &options.options())?;
            shell.status("Ready", format!("{name} is {serial}"));
        }
        AndroidCommand::Emulator { command: EmulatorCommand::Stop { target } } => {
            let serial = if target.starts_with("emulator-") {
                target
            } else {
                device::running_serial(&toolbox, &target)?
                    .with_context(|| format!("no running emulator for the virtual device `{target}`"))?
            };
            device::stop_emulator(&toolbox.adb()?, shell, &serial)?;
        }
        AndroidCommand::Connect { address } => device::connect(&toolbox, shell, &address)?,
        AndroidCommand::Pair { address, code } => device::pair(&toolbox, shell, &address, code.as_deref())?,
        AndroidCommand::Disconnect { address } => device::disconnect(&toolbox, shell, address.as_deref())?,
    }
    Ok(0)
}

fn toolchain(config: &Config, global: &GlobalArgs, command: ToolchainCommand) -> Result<i32> {
    // Toolchain commands also work outside a project. Packages built together share their
    // toolchains, so the first member stands for a whole workspace.
    let project = load_project(global).ok();
    let ws = project.as_ref().map(|p| p.package(p.current().unwrap_or(p.members()[0])));
    let manifest = ws.map(Workspace::manifest);
    let shell = config.shell();
    match command {
        ToolchainCommand::List => print!("{}", toolchain_ops::list(config, manifest)?),
        ToolchainCommand::Install { kind: Some(kind), version, accept_licenses, emulator } => {
            if emulator && !matches!(kind, ToolArg::AndroidSdk) {
                bail!("`--emulator` installs the Android emulator and needs `android-sdk`");
            }
            let what = toolchain_ops::install(config, manifest, kind.into(), version.as_deref(), licenses(accept_licenses))?;
            shell.status("Installed", what);
            if emulator {
                let what = toolchain_ops::install_android_emulator(config, manifest, version.as_deref(), licenses(accept_licenses))?;
                shell.status("Installed", what);
            }
        }
        ToolchainCommand::Install { kind: None, version: Some(_), .. } => {
            bail!("specify what to install: `jdk`, `graalvm`, `native`, `gradle` or `android-sdk`")
        }
        ToolchainCommand::Install { kind: None, version: None, accept_licenses, .. } => {
            let project = project
                .as_ref()
                .context("not in a Kargo project; use `kargo toolchain install jdk <version>`, `native <version>` or `gradle <version>`")?;
            // Members share the JDK and Kotlin version, but any of them may have the Native target.
            let mut done: Vec<String> = Vec::new();
            for &member in project.members() {
                for what in toolchain_ops::install_for_project(config, project.package(member).manifest(), licenses(accept_licenses))? {
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
        ToolchainCommand::Available { kind } => print!("{}", toolchain_ops::available(config, kind.map(Into::into))?),
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
                println!("android-sdk:   {}", kargo_ops::gradle::describe_sdk(ws, config)?);
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

fn target(shell: &Shell, global: &GlobalArgs, command: TargetCommand) -> Result<i32> {
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
/// `word`, made plural for every `count` but one (`advisory` -> `advisories`).
fn plural(count: usize, word: &str) -> String {
    match word.strip_suffix('y') {
        _ if count == 1 => word.to_owned(),
        Some(stem) => format!("{stem}ies"),
        None => format!("{word}s"),
    }
}

fn configuration_name(configuration: Configuration) -> &'static str {
    match configuration {
        Configuration::Compile => "compile",
        Configuration::Runtime => "runtime",
        Configuration::Test => "test",
    }
}

/// The error of `kargo why` and `kargo tree --invert` for a module no resolution has.
fn not_found(query: &ops::why::Query, configuration: Configuration) -> String {
    let module = match &query.version {
        Some(v) => format!("{}:{v}", query.module),
        None => query.module.to_string(),
    };
    format!(
        "`{module}` is not in the {} dependencies; check the name with `kargo tree`, or pick another classpath with `-c` or target with `-t`",
        configuration_name(configuration)
    )
}

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

/// The versions of the frameworks of `ws`, with the modules their BOMs manage as `kargo.lock` resolved them for the
/// JVM runtime. Before the first resolution there is no such entry, and those modules are left out.
fn locked_framework_versions(ws: &Workspace) -> Result<Vec<kargo_ops::builtins::FrameworkVersion>> {
    let lock = kargo_ops::lockfile::Lockfile::load(&ws.lockfile_path())?;
    let runtime = lock.resolutions.get(&ws.lock_key("jvm/runtime"));
    Ok(kargo_ops::builtins::framework_versions(ws.manifest(), |module| {
        let package = runtime?.packages.iter().find(|p| p.module == module.to_string())?;
        Some(package.version.clone())
    }))
}

fn print_info(config: &Config, project: &Project, ws: &Workspace) -> Result<()> {
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
    for (i, framework) in locked_framework_versions(ws)?.iter().enumerate() {
        let label = if i == 0 { "frameworks:" } else { "" };
        println!("{label:<14}{}", framework.describe());
    }
    println!("dependencies: {} (+{} dev)", m.dependencies.len(), m.dev_dependencies.len());
    if !ws.path_dependencies().is_empty() {
        let paths: Vec<String> =
            ws.path_dependencies().iter().map(|(d, i)| format!("{} ({})", d.name, project.package(*i).root().display())).collect();
        println!("path deps:    {}", paths.join(", "));
    }
    // Where each repository is actually fetched from, after `[mirrors]` and `--vendored`.
    let repos: Vec<String> = m
        .repositories
        .iter()
        .map(|r| match effective_url(config, r) {
            url if url == r.url => r.name.clone(),
            url => format!("{} (via {url})", r.name),
        })
        .collect();
    println!("repositories: {}", repos.join(", "));
    let files = config.settings().files();
    if !files.is_empty() {
        let files: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        println!("config:       {}", files.join(", "));
    }
    println!("root:         {}", ws.root().display());
    println!("target dir:   {}", ws.target_dir().display());
    Ok(())
}

/// `kargo info --format json`.
fn info_json(config: &Config, ws: &Workspace) -> Result<serde_json::Value> {
    let m = ws.manifest();
    let frameworks = locked_framework_versions(ws)?;
    let dependencies = |deps: &[kargo_manifest::Dependency]| -> Vec<serde_json::Value> {
        deps.iter()
            .map(|d| serde_json::json!({ "name": d.name, "module": d.module.to_string(), "version": d.version, "scope": d.scope.as_str() }))
            .collect()
    };
    Ok(serde_json::json!({
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
        "framework-versions": frameworks
            .iter()
            .map(|f| {
                let managed: Vec<serde_json::Value> = f
                    .managed
                    .iter()
                    .map(|v| serde_json::json!({ "name": v.name, "module": v.module.to_string(), "version": v.version }))
                    .collect();
                serde_json::json!({ "name": f.name, "version": f.version, "managed": managed })
            })
            .collect::<Vec<_>>(),
        "dependencies": dependencies(&m.dependencies),
        "dev-dependencies": dependencies(&m.dev_dependencies),
        "repositories": m
            .repositories
            .iter()
            .map(|r| serde_json::json!({ "name": r.name, "url": r.url, "effective-url": effective_url(config, r) }))
            .collect::<Vec<_>>(),
        "bins": m
            .bins
            .iter()
            .map(|b| serde_json::json!({ "name": b.name, "main": b.main, "entry-point": b.entry_point, "path": b.path }))
            .collect::<Vec<_>>(),
        "examples": ws.examples(),
        "root": ws.root().display().to_string(),
        "manifest": ws.manifest_path().display().to_string(),
        "target-dir": ws.target_dir().display().to_string(),
    }))
}

/// `kargo <name>`: a task from the manifest, else a `kargo-<name>` executable.
fn external(config: &Config, global: &GlobalArgs, argv: Vec<String>) -> Result<i32> {
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
        let message = format!(
            "no such command: `{name}`

	View all installed commands with `kargo --help` and `kargo plugins`"
        );
        let suggestions: Vec<String> = help::nearest(name, &known, 3).into_iter().map(str::to_owned).collect();
        return Err(match help::closest(name, &known) {
            Some(close) => help::helpful(message, format!("did you mean `{close}`?"), suggestions),
            None => anyhow::anyhow!(message),
        });
    };
    let cwd = std::env::current_dir()?;
    let code = kargo_ops::plugins::run_subcommand(config, name, &program, args, &cwd)?;
    Ok(code)
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
        assert!(matches!(cli.command, Command::Run(RunArgs { ref args, .. }) if args == &["--flag", "x"]));
        let cli = Cli::try_parse_from(["kargo", "run", "--entry-point", "com.x.main"]).unwrap();
        assert!(matches!(cli.command, Command::Run(RunArgs { main: Some(ref m), .. }) if m == "com.x.main"));
        let cli = Cli::try_parse_from(["kargo", "run", "--bin", "migrate", "--", "up"]).unwrap();
        assert!(matches!(cli.command, Command::Run(RunArgs { bin: Some(ref b), ref args, .. }) if b == "migrate" && args == &["up"]));
        assert!(Cli::try_parse_from(["kargo", "run", "--bin", "a", "--example", "b"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "run", "Tool.kt", "up", "--release", "--", "--flag"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Run(RunArgs { release: true, ref words, ref args, .. }) if words == &["Tool.kt", "up"] && args == &["--flag"]
        ));
        let cli = Cli::try_parse_from(["kargo", "run", "-e", "println(1)", "x"]).unwrap();
        assert!(matches!(cli.command, Command::Run(RunArgs { expr: Some(ref e), ref words, .. }) if e == "println(1)" && words == &["x"]));
        assert!(Cli::try_parse_from(["kargo", "run", "-e", "1", "--main", "A"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "run", "--main", "A", "--bin", "b"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "package", "--bin", "migrate"]).unwrap();
        assert!(matches!(cli.command, Command::Package { bin: Some(ref b), .. } if b == "migrate"));
        let cli = Cli::try_parse_from(["kargo", "package", "--optimize"]).unwrap();
        assert!(matches!(cli.command, Command::Package { optimize: true, .. }));
        let cli = Cli::try_parse_from(["kargo", "package", "--message-format", "json"]).unwrap();
        assert!(matches!(cli.command, Command::Package { message_format: MessageFormatArg::Json, .. }));
        let cli = Cli::try_parse_from(["kargo", "retrace", "mapping.txt", "crash.txt"]).unwrap();
        assert!(
            matches!(cli.command, Command::Retrace { ref mapping, ref stack_trace, .. } if mapping == Path::new("mapping.txt") && stack_trace.as_deref() == Some(Path::new("crash.txt")))
        );
        let cli = Cli::try_parse_from(["kargo", "test", "--against-min"]).unwrap();
        assert!(matches!(cli.command, Command::Test { against_min: true, .. }));
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
        assert!(matches!(cli.command, Command::Add(AddArgs { ref frameworks, .. }) if frameworks == &["compose"]));
        assert!(Cli::try_parse_from(["kargo", "add"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "my-plugin", "--x"]).unwrap();
        assert!(matches!(cli.command, Command::External(ref v) if v == &["my-plugin", "--x"]));
        let cli = Cli::try_parse_from(["kargo", "task", "gen", "--watch"]).unwrap();
        assert!(matches!(cli.command, Command::Task { ref args, .. } if args == &["--watch"]));
        let cli = Cli::try_parse_from(["kargo", "toolchain", "install", "jdk", "21"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Toolchain { command: ToolchainCommand::Install { kind: Some(ToolArg::Jdk), ref version, accept_licenses: false, .. } } if version.as_deref() == Some("21")
        ));
        let cli = Cli::try_parse_from(["kargo", "toolchain", "install", "android-sdk", "36", "--accept-licenses"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Toolchain { command: ToolchainCommand::Install { kind: Some(ToolArg::AndroidSdk), ref version, accept_licenses: true, .. } } if version.as_deref() == Some("36")
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
        assert!(matches!(
            Cli::try_parse_from(["kargo", "doctor", "--fix", "--format", "json"]).unwrap().command,
            Command::Doctor { fix: true, format: FormatArg::Json, bug_report: false }
        ));
        assert!(Cli::try_parse_from(["kargo", "doctor", "--bug-report", "--format", "json"]).is_err());
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
        assert!(matches!(cli.command, Command::Run(RunArgs { debug: Some(5005), ref args, .. }) if args == &["x"]));
        let cli = Cli::try_parse_from(["kargo", "test", "--debug=6000", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { debug: Some(6000), ref filter, .. } if filter.as_deref() == Some("MathTest")));
        // `--debug` never takes the next word as its port.
        let cli = Cli::try_parse_from(["kargo", "test", "--debug", "MathTest"]).unwrap();
        assert!(matches!(cli.command, Command::Test { debug: Some(5005), ref filter, .. } if filter.as_deref() == Some("MathTest")));
        let cli = Cli::try_parse_from(["kargo", "metadata", "--no-deps", "--format", "json"]).unwrap();
        assert!(matches!(cli.command, Command::Metadata { no_deps: true, fingerprint: false, .. }));
        assert!(Cli::try_parse_from(["kargo", "metadata", "--format", "text"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "metadata", "--fingerprint", "--no-deps"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "vendor", "third_party", "--toolchains", "--sync"]).unwrap();
        assert!(
            matches!(cli.command, Command::Vendor { path: Some(ref p), toolchains: true, sync: true } if p == Path::new("third_party"))
        );
        let cli = Cli::try_parse_from(["kargo", "init-ci", "--junit", "--force"]).unwrap();
        assert!(matches!(cli.command, Command::InitCi { force: true, junit: true, .. }));
        let cli = Cli::try_parse_from(["kargo", "build", "--vendored"]).unwrap();
        assert!(cli.global.vendored);
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
            matches!(cli.command, Command::Run(RunArgs { ref package, ref args, .. }) if package.package.as_deref() == Some("app") && args == &["x"])
        );
        assert!(Cli::try_parse_from(["kargo", "run", "-p", "a", "-p", "b"]).is_err());
        let cli = Cli::try_parse_from(["kargo", "add", "--path", "../lib", "--dev"]).unwrap();
        assert!(matches!(cli.command, Command::Add(AddArgs { ref path, dev: true, .. }) if path.as_deref() == Some(Path::new("../lib"))));
        assert!(Cli::try_parse_from(["kargo", "add", "--path", "../lib", "g:a"]).is_err());
        assert!(Cli::try_parse_from(["kargo", "add", "--path", "../lib", "--bom"]).is_err());
    }

    #[test]
    fn run_words_start_with_an_existing_kotlin_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Tool.kt");
        std::fs::write(&file, "fun main() {}").unwrap();
        let file = file.to_string_lossy().into_owned();
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let (found, args) = split_run_words(words(&[&file, "up"]), words(&["--flag"]));
        assert_eq!(found.as_deref(), Some(Path::new(&file)));
        assert_eq!(args, ["up", "--flag"]);
        // Any other word, a missing file included, is an argument as before.
        let missing = dir.path().join("Missing.kt").to_string_lossy().into_owned();
        let (found, args) = split_run_words(words(&[&missing, "x"]), Vec::new());
        assert_eq!((found, args), (None, words(&[&missing, "x"])));
        let (found, args) = split_run_words(words(&["somearg"]), words(&["y"]));
        assert_eq!((found, args), (None, words(&["somearg", "y"])));
        // A file after `--` is an argument.
        let (found, args) = split_run_words(Vec::new(), words(&[&file]));
        assert_eq!((found, args), (None, words(&[&file])));
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
            Command::Run(RunArgs { watch: true, ref watch_args, ref args, .. })
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
