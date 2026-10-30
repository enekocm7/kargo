//! `kargo doctor`: checks the machine and the project for what keeps builds from working (the
//! Kargo home, the JDK, toolchains, the Android SDK, Node.js and browsers, the network, the
//! manifest and the lockfile), with a one-line fix for each problem, and `--fix` for the safe ones.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use kargo_manifest::{
    AndroidSdkRequest, GraalvmRequest, JdkRequest, JsEnvironment, Manifest, PackageManager, Scope, TargetKind, WasmEnvironment,
};
use kargo_maven::repo::{Repository, effective_repositories};
use kargo_toolchain::archive::human_size;
use kargo_toolchain::{GradleDistribution, Jdk, LanguageServer, NativeToolchain, ServerKind, android_sdk, graalvm, jdks};
use kargo_util::config::Config;
use kargo_util::flock;

use crate::gradle::GradleChoice;
use crate::lockfile::Lockfile;
use crate::ops::context::{MIN_JDK, jdk_needs};
use crate::warnings;
use crate::workspace::{BUILD_LOCK_NAME, Project};

/// Below this much free space in the Kargo home, a JDK or Kotlin/Native download may not fit.
const LOW_DISK: u64 = 5 << 30;
/// Below this much, builds are likely to fail.
const VERY_LOW_DISK: u64 = 1 << 30;
/// How long a repository may take to answer.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Ok,
    Warning,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warning => "warning",
            Status::Error => "error",
        }
    }
}

/// The group a check belongs to, in the order they are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Kargo,
    Jdk,
    Toolchains,
    Android,
    Web,
    Network,
    Project,
}

impl Section {
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Kargo => "kargo",
            Section::Jdk => "jdk",
            Section::Toolchains => "toolchains",
            Section::Android => "android",
            Section::Web => "web",
            Section::Network => "network",
            Section::Project => "project",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Kargo => "Kargo",
            Section::Jdk => "JDK",
            Section::Toolchains => "Toolchains",
            Section::Android => "Android",
            Section::Web => "Wasm and JavaScript",
            Section::Network => "Network",
            Section::Project => "Project",
        }
    }
}

/// A safe repair `kargo doctor --fix` makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    /// Removes the directory of a toolchain whose install did not finish; the next command that
    /// needs it installs it again.
    RemoveDir(PathBuf),
}

/// One check: what was looked at, how it went, and how to fix it.
#[derive(Debug, Clone)]
pub struct Check {
    pub section: Section,
    pub status: Status,
    /// What was checked, such as `JAVA_HOME` or `repository maven-central`.
    pub name: String,
    pub message: String,
    /// What to do about a warning or an error.
    pub fix: Option<String>,
    pub repair: Option<Repair>,
}

impl Check {
    fn ok(section: Section, name: impl Into<String>, message: impl Into<String>) -> Self {
        Check { section, status: Status::Ok, name: name.into(), message: message.into(), fix: None, repair: None }
    }

    fn warning(section: Section, name: impl Into<String>, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Check { status: Status::Warning, fix: Some(fix.into()), ..Self::ok(section, name, message) }
    }

    fn error(section: Section, name: impl Into<String>, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Check { status: Status::Error, fix: Some(fix.into()), ..Self::ok(section, name, message) }
    }

    fn with_repair(mut self, repair: Repair) -> Self {
        self.repair = Some(repair);
        self
    }
}

/// The project `kargo doctor` runs in.
pub enum ProjectState<'a> {
    /// Outside a project: only the machine is checked.
    None,
    Loaded(&'a Project),
    /// A `kargo.toml` was found but does not load.
    Broken {
        manifest: PathBuf,
        error: &'a anyhow::Error,
    },
}

/// Everything `kargo doctor` found, in report order.
#[derive(Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
    /// What `--fix` did.
    pub fixed: Vec<String>,
}

impl Report {
    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    pub fn has_errors(&self) -> bool {
        self.count(Status::Error) > 0
    }
}

/// Runs every check. `is_builtin_command` tells the manifest warnings which task names a built-in
/// command hides.
pub fn diagnose(config: &Config, project: ProjectState, is_builtin_command: &dyn Fn(&str) -> bool) -> Report {
    let mut checks = Vec::new();
    kargo(config, &mut checks);
    let loaded = match &project {
        ProjectState::Loaded(p) => Some(*p),
        _ => None,
    };
    // Members share their toolchains, so the first one stands for a whole workspace.
    let manifest = loaded.map(|p| p.package(p.current().unwrap_or(p.members()[0])).manifest());
    jdk(config, manifest, &mut checks);
    toolchains(config, loaded, &mut checks);
    android(config, loaded, &mut checks);
    web(loaded, config, &mut checks);
    network(config, loaded, &mut checks);
    match project {
        ProjectState::None => {}
        ProjectState::Loaded(p) => project_checks(p, is_builtin_command, &mut checks),
        ProjectState::Broken { manifest, error } => checks.push(Check::error(
            Section::Project,
            "kargo.toml",
            format!("{} does not load: {}", manifest.display(), chain(error)),
            "fix kargo.toml as the message says",
        )),
    }
    Report { checks, fixed: Vec::new() }
}

/// Makes the repairs of `report`'s checks, marking each one fixed.
pub fn fix(report: &mut Report) {
    for check in &mut report.checks {
        let Some(Repair::RemoveDir(dir)) = &check.repair else { continue };
        match kargo_util::fs::remove_dir_all_if_exists(dir) {
            Ok(()) => {
                report.fixed.push(format!("removed {}", dir.display()));
                check.status = Status::Ok;
                check.message = format!("removed the incomplete install at {}; it is installed again when needed", dir.display());
                check.fix = None;
                check.repair = None;
            }
            Err(e) => check.message = format!("{} (--fix failed: {})", check.message, chain(&e)),
        }
    }
}

fn chain(error: &anyhow::Error) -> String {
    error.chain().map(ToString::to_string).collect::<Vec<_>>().join(": ")
}

fn kargo(config: &Config, out: &mut Vec<Check>) {
    let s = Section::Kargo;
    out.push(Check::ok(s, "version", format!("kargo {} ({})", env!("CARGO_PKG_VERSION"), crate::ops::self_update::TARGET)));

    let home = config.home();
    if !home.exists() {
        out.push(Check::ok(s, "KARGO_HOME", format!("{} (created on first use)", home.display())));
    } else {
        let probe = home.join(format!(".doctor-{}", std::process::id()));
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                out.push(Check::ok(s, "KARGO_HOME", format!("{} (writable)", home.display())));
            }
            Err(e) => out.push(Check::error(
                s,
                "KARGO_HOME",
                format!("{} is not writable: {e}", home.display()),
                "give your user write access to it, or set KARGO_HOME to a writable directory",
            )),
        }
    }

    match kargo_util::fs::free_space(home) {
        Some(free) if free < VERY_LOW_DISK => out.push(Check::error(
            s,
            "disk space",
            format!("{} free for {}", human_size(free), home.display()),
            "free some space; `kargo toolchain list` shows the toolchains you can remove",
        )),
        Some(free) if free < LOW_DISK => out.push(Check::warning(
            s,
            "disk space",
            format!("{} free for {}; a JDK or Kotlin/Native download may not fit", human_size(free), home.display()),
            "free some space; `kargo toolchain list` shows the toolchains you can remove",
        )),
        Some(free) => out.push(Check::ok(s, "disk space", format!("{} free", human_size(free)))),
        None => {}
    }

    let daemons = kargo_compiler::daemon::running(home);
    out.push(Check::ok(
        s,
        "compiler daemons",
        match daemons.len() {
            0 => "none running".to_owned(),
            1 => "1 running (`kargo daemon stop` stops it)".to_owned(),
            n => format!("{n} running (`kargo daemon stop` stops them)"),
        },
    ));

    let installing: Vec<String> = held_install_locks(config);
    if !installing.is_empty() {
        out.push(Check::warning(
            s,
            "toolchain installs",
            format!("another Kargo process is installing {}", installing.join(", ")),
            "wait for it to finish; if no Kargo command is running, a hung `kargo` process holds the lock: stop it",
        ));
    }
}

/// The toolchains another process is installing now (`.lock-<name>` files that are held).
fn held_install_locks(config: &Config) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(config.toolchains_dir()) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().strip_prefix(".lock-")?.to_owned();
            flock::is_held(&e.path()).then_some(name)
        })
        .collect();
    out.sort();
    out
}

fn jdk(config: &Config, manifest: Option<&Manifest>, out: &mut Vec<Check>) {
    let s = Section::Jdk;
    java_home(out);
    let Some(m) = manifest else {
        match Jdk::find() {
            Ok(Some(jdk)) if jdk.version < MIN_JDK => out.push(Check::error(
                s,
                "system JDK",
                format!("JDK {} at {} is too old; Kargo needs JDK {MIN_JDK} or newer", jdk.version, jdk.home.display()),
                "install a newer JDK, or set `[toolchain] jdk = \"21\"` in a project's kargo.toml to have Kargo download one",
            )),
            Ok(Some(jdk)) => out.push(Check::ok(s, "system JDK", format!("JDK {} at {}", jdk.version, jdk.home.display()))),
            Ok(None) => out.push(Check::warning(
                s,
                "system JDK",
                "no JDK on JAVA_HOME or PATH",
                "Kargo downloads one for a project that needs it; or run `kargo toolchain install jdk 21`",
            )),
            Err(e) => out.push(Check::error(s, "system JDK", chain(&e), "fix JAVA_HOME, or put a working `java` on PATH")),
        }
        return;
    };

    let found = match &m.toolchain.jdk {
        JdkRequest::System => match Jdk::find() {
            Ok(Some(jdk)) => Some((jdk, "system".to_owned())),
            Ok(None) => match jdks::find_fallback(config, jdk_needs(m)) {
                Ok(Some(managed)) => Some((managed.jdk(), format!("Kargo-managed {}, as there is no system JDK", managed.release))),
                _ => {
                    out.push(Check::warning(
                        s,
                        "project JDK",
                        "no JDK on JAVA_HOME or PATH, and none installed by Kargo",
                        "the first build downloads one; or run `kargo toolchain install`",
                    ));
                    None
                }
            },
            Err(e) => {
                out.push(Check::error(s, "project JDK", chain(&e), "fix JAVA_HOME, or pin one with `[toolchain] jdk = \"21\"`"));
                None
            }
        },
        request => match jdks::find_installed(config, request) {
            Ok(Some(managed)) => Some((managed.jdk(), format!("Kargo-managed {}", managed.release))),
            _ => {
                out.push(Check::warning(
                    s,
                    "project JDK",
                    format!("`[toolchain] jdk = \"{request}\"` is not installed yet"),
                    "the first build downloads it; or run `kargo toolchain install`",
                ));
                None
            }
        },
    };
    let Some((jdk, source)) = found else { return };
    let where_ = format!("JDK {} at {} ({source})", jdk.version, jdk.home.display());
    if jdk.version < MIN_JDK {
        out.push(Check::error(
            s,
            "project JDK",
            format!("{where_} is too old; Kargo needs JDK {MIN_JDK} or newer"),
            "set `[toolchain] jdk = \"21\"` in kargo.toml to have Kargo download one",
        ));
        return;
    }
    if let Err(e) = kargo_toolchain::kotlin::check_compiler_jdk(&m.kotlin.version, &jdk) {
        out.push(Check::error(s, "project JDK", chain(&e), "see the message"));
        return;
    }
    if let Some(needed) = m.targets.jvm.as_ref().and_then(|j| j.required_jdk())
        && jdk.version < needed
    {
        out.push(Check::error(
            s,
            "project JDK",
            format!("{where_} cannot compile for `jvm-target` {needed}"),
            format!("set `[toolchain] jdk = \"{needed}\"` in kargo.toml, or lower `jvm-target`"),
        ));
        return;
    }
    if !jdk.has_javac() && m.targets.is_enabled(TargetKind::Jvm) {
        out.push(Check::warning(
            s,
            "project JDK",
            format!("{where_} has no javac, so Java sources do not compile"),
            "install a full JDK rather than a JRE, or set `[toolchain] jdk = \"21\"`",
        ));
        return;
    }
    out.push(Check::ok(s, "project JDK", format!("{where_}, runs Kotlin {}", m.kotlin.version)));

    if m.toolchain.graalvm != GraalvmRequest::System {
        match graalvm::find_installed(config, &m.toolchain.graalvm) {
            Ok(Some(g)) => out.push(Check::ok(s, "GraalVM", format!("{} {} at {}", g.distribution.title(), g.version, g.home.display()))),
            _ => out.push(Check::warning(
                s,
                "GraalVM",
                format!("`[toolchain] graalvm = \"{}\"` is not installed yet", m.toolchain.graalvm),
                "`kargo package --format native-image` downloads it; or run `kargo toolchain install graalvm`",
            )),
        }
    }
}

/// `JAVA_HOME` that points nowhere, or at another JDK than `java` on `PATH`.
fn java_home(out: &mut Vec<Check>) {
    let s = Section::Jdk;
    let Some(home) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()).map(PathBuf::from) else { return };
    let java = home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" });
    if !java.is_file() {
        out.push(Check::warning(
            s,
            "JAVA_HOME",
            format!("{} has no bin/java, so Kargo ignores it", home.display()),
            "point JAVA_HOME at a JDK's home directory, or unset it",
        ));
        return;
    }
    let (Ok(from_home), Some(Ok(on_path))) = (Jdk::from_java(&java), kargo_util::process::find_on_path("java").map(|j| Jdk::from_java(&j)))
    else {
        return;
    };
    if from_home.version != on_path.version {
        out.push(Check::warning(
            s,
            "JAVA_HOME",
            format!(
                "JAVA_HOME is JDK {} but `java` on PATH is JDK {} ({}); Kargo uses JAVA_HOME, other tools may not",
                from_home.version,
                on_path.version,
                on_path.home.display()
            ),
            "put JAVA_HOME's bin directory first on PATH",
        ));
    } else {
        out.push(Check::ok(s, "JAVA_HOME", format!("JDK {} at {}", from_home.version, from_home.home.display())));
    }
}

fn toolchains(config: &Config, project: Option<&Project>, out: &mut Vec<Check>) {
    let s = Section::Toolchains;
    // A toolchain being installed looks incomplete until its install finishes.
    let installing = !held_install_locks(config).is_empty();
    let mut broken: Vec<(String, PathBuf)> = Vec::new();
    let mut complete: Vec<String> = Vec::new();

    let jdks = jdks::installed(config).unwrap_or_default();
    complete.extend(jdks.iter().map(|j| format!("JDK {}", j.release)));
    broken.extend(incomplete(&config.jdks_dir(), &jdks.iter().map(|j| j.dir.clone()).collect::<Vec<_>>(), "JDK"));
    let graalvms = graalvm::installed(config).unwrap_or_default();
    complete.extend(graalvms.iter().map(|g| format!("GraalVM {}", g.release())));
    broken.extend(incomplete(&config.graalvm_dir(), &graalvms.iter().map(|g| g.dir.clone()).collect::<Vec<_>>(), "GraalVM"));
    let natives = NativeToolchain::installed(config).unwrap_or_default();
    complete.extend(natives.iter().map(|n| format!("Kotlin/Native {}", n.version)));
    broken.extend(incomplete(&config.native_dir(), &natives.iter().map(|n| n.home.clone()).collect::<Vec<_>>(), "Kotlin/Native"));
    let gradles = GradleDistribution::installed(config).unwrap_or_default();
    complete.extend(gradles.iter().map(|g| format!("Gradle {}", g.version)));
    broken.extend(incomplete(&config.gradle_dir(), &gradles.iter().map(|g| g.home.clone()).collect::<Vec<_>>(), "Gradle"));
    let servers = LanguageServer::installed(config).unwrap_or_default();
    complete.extend(servers.iter().map(|l| format!("{} {}", l.kind.title(), l.version)));
    for kind in ServerKind::ALL {
        let homes: Vec<PathBuf> = servers.iter().filter(|l| l.kind == kind).map(|l| l.home.clone()).collect();
        broken.extend(incomplete(&kind.dir(config), &homes, kind.title()));
    }

    if complete.is_empty() {
        out.push(Check::ok(s, "installed", format!("none in {}", config.toolchains_dir().display())));
    } else {
        out.push(Check::ok(s, "installed", complete.join(", ")));
    }
    for (what, dir) in broken {
        let message = format!("{what} at {} is incomplete (an install was interrupted)", dir.display());
        if installing {
            out.push(Check::warning(s, what, message, "wait for the install in progress, then run `kargo doctor` again"));
        } else {
            out.push(
                Check::warning(s, what, message, "`kargo doctor --fix` removes it; it is installed again when needed")
                    .with_repair(Repair::RemoveDir(dir)),
            );
        }
    }

    let Some(project) = project else { return };
    let m = project.package(project.current().unwrap_or(project.members()[0])).manifest();
    let version = &m.kotlin.version;
    out.push(Check::ok(s, "Kotlin", format!("{version} (downloaded from Maven on first build)")));
    if project.members().iter().any(|&i| project.package(i).manifest().targets.is_enabled(TargetKind::Native)) {
        match NativeToolchain::find(config, version) {
            Ok(Some(native)) => out.push(Check::ok(s, "Kotlin/Native", format!("{version} at {}", native.home.display()))),
            _ => out.push(Check::warning(
                s,
                "Kotlin/Native",
                format!("{version} is not installed yet"),
                "the first native build downloads it; or run `kargo toolchain install`",
            )),
        }
        for member in project.members() {
            let m = project.package(*member).manifest();
            let Some(native) = m.targets.native.as_ref() else { continue };
            for platform in &native.platforms {
                let Ok(konan_target) = kargo_toolchain::native::resolve_platform(platform) else { continue };
                if let Some(limit) = super::build::native_host_limit(&konan_target) {
                    out.push(Check::warning(
                        s,
                        format!("native {konan_target}"),
                        format!("{} enables {konan_target}, which this host cannot build: {limit}", m.package.name),
                        "`kargo build` skips it here; build on macOS, or take it out of `platforms`",
                    ));
                }
            }
        }
    }
}

/// The directories under `root` that are not one of the `complete` installs, as `(what, dir)`.
fn incomplete(root: &Path, complete: &[PathBuf], what: &str) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|dir| !complete.iter().any(|c| c.starts_with(dir) || dir.starts_with(c)))
        .map(|dir| (format!("{what} {}", dir.file_name().unwrap_or_default().to_string_lossy()), dir))
        .collect();
    out.sort();
    out
}

/// The Android checks of the project, or, outside a project or without the Android target, what the machine has
/// for Android builds.
fn android(config: &Config, project: Option<&Project>, out: &mut Vec<Check>) {
    let s = Section::Android;
    let package = project
        .and_then(|project| project.members().iter().map(|&i| project.package(i)).find(|ws| ws.manifest().targets.android.is_some()));
    let Some(ws) = package else {
        machine_android(config, out);
        return;
    };
    let m = ws.manifest();
    let Some(android) = &m.targets.android else { return };
    let packages = crate::gradle::sdk_packages(&android.config);
    match m.toolchain.android_sdk {
        AndroidSdkRequest::Platform(api) => {
            let sdk = config.android_sdk_dir();
            match android_sdk::missing(&sdk, &packages).as_slice() {
                [] => out.push(Check::ok(s, "Android SDK", format!("API {api} (Kargo-managed at {})", sdk.display()))),
                missing => out.push(Check::warning(
                    s,
                    "Android SDK",
                    format!("Kargo-managed SDK lacks {}", missing.join(", ")),
                    "the first android build installs them; or run `kargo toolchain install`",
                )),
            }
        }
        AndroidSdkRequest::System => match crate::gradle::android_sdk() {
            None => out.push(Check::error(
                s,
                "ANDROID_HOME",
                "no Android SDK found (ANDROID_HOME, ANDROID_SDK_ROOT or the default location)",
                format!(
                    "set ANDROID_HOME to your SDK, or set `android-sdk = \"{}\"` under [toolchain] in kargo.toml to have Kargo install it",
                    android.config.compile_sdk
                ),
            )),
            Some(sdk) => {
                let missing = android_sdk::missing(&sdk, &packages[..2]);
                if missing.is_empty() {
                    out.push(Check::ok(s, "Android SDK", format!("{} has {}", sdk.display(), packages[..2].join(" and "))));
                } else if android_sdk::has_accepted_license(&sdk) {
                    out.push(Check::warning(
                        s,
                        "Android SDK",
                        format!("{} lacks {}; Gradle downloads them on the first build", sdk.display(), missing.join(" and ")),
                        format!("or install them now with `sdkmanager {}`", missing.join(" ")),
                    ));
                } else {
                    out.push(Check::error(
                        s,
                        "Android SDK",
                        format!("{} lacks {} and its licenses are not accepted", sdk.display(), missing.join(" and ")),
                        format!("run `sdkmanager --licenses` and `sdkmanager {}`", missing.join(" ")),
                    ));
                }
            }
        },
    }
    devices(config, m, out);
    match crate::gradle::choose_gradle(ws, config) {
        Ok(GradleChoice::Project(wrapper)) => out.push(Check::ok(s, "Gradle", format!("the project's wrapper at {}", wrapper.display()))),
        Ok(GradleChoice::System(gradle)) => out.push(Check::ok(s, "Gradle", format!("{} (--system-gradle)", gradle.display()))),
        Ok(GradleChoice::Managed { version, .. }) => match GradleDistribution::find(config, &version) {
            Ok(Some(dist)) => out.push(Check::ok(s, "Gradle", format!("{version} at {}", dist.home.display()))),
            _ => out.push(Check::warning(
                s,
                "Gradle",
                format!("{version} is not installed yet"),
                "the first android build downloads it; or run `kargo toolchain install`",
            )),
        },
        Err(e) => out.push(Check::error(s, "Gradle", chain(&e), "change `gradle-version` or `agp-version` under [target.android]")),
    }
}

/// The Android SDK the machine has, for a project that does not build for Android: nothing is wrong without one, but
/// `kargo new --target android` or a new `[target.android]` needs it.
fn machine_android(config: &Config, out: &mut Vec<Check>) {
    let s = Section::Android;
    let managed = config.android_sdk_dir();
    if let Some(sdk) = crate::gradle::android_sdk() {
        out.push(Check::ok(s, "Android SDK", format!("{} (ANDROID_HOME or the default location)", sdk.display())));
    } else if !android_sdk::installed(&managed).is_empty() {
        out.push(Check::ok(s, "Android SDK", format!("Kargo-managed at {}", managed.display())));
    } else {
        out.push(Check::ok(
            s,
            "Android SDK",
            "not found; only Android builds need it. Set ANDROID_HOME to an SDK, or run `kargo toolchain install android-sdk` to have Kargo install one",
        ));
    }
}

/// What `kargo run -t android` needs on the machine: adb, the devices and their states, the virtual devices, the emulator
/// and its hardware acceleration. Nothing is started or installed.
fn devices(config: &Config, manifest: &Manifest, out: &mut Vec<Check>) {
    use crate::android_device::{self as device, Acceleration, DeviceKind};
    let s = Section::Android;
    let toolbox = device::Toolbox::locate(config, Some(manifest));
    let report = device::report(&toolbox, config.shell());
    match &report.adb {
        Some((path, version)) => out.push(Check::ok(s, "adb", format!("{version} at {}", path.display()))),
        None => out.push(Check::warning(
            s,
            "adb",
            match toolbox.sdk() {
                Some(sdk) => format!("{} has no platform-tools/adb, which `kargo run` needs to install the app", sdk.display()),
                None => "adb not found, which `kargo run` needs to install the app".to_owned(),
            },
            if toolbox.is_managed() { "run `kargo toolchain install android-sdk`" } else { "run `sdkmanager platform-tools`" },
        )),
    }
    if report.adb.is_some() {
        let ready: Vec<String> = report.devices.iter().filter(|d| d.is_ready()).map(|d| format!("{} ({})", d.name, d.id)).collect();
        let mut problems = false;
        for d in report.devices.iter().filter(|d| !d.is_ready()) {
            problems = true;
            let fix = match d.state.as_str() {
                "unauthorized" => "unlock the device and accept the USB debugging prompt".to_owned(),
                "offline" => "reconnect the cable, or run `adb kill-server`".to_owned(),
                "no permissions" => "add the udev rules (Linux) or install the OEM USB driver (Windows) for the device".to_owned(),
                _ => "wait for it to finish starting, or reconnect it".to_owned(),
            };
            let kind = if d.kind == DeviceKind::Emulator { "emulator" } else { "device" };
            out.push(Check::warning(s, format!("{kind} {}", d.id), format!("{} is {}", d.name, d.state), fix));
        }
        if !ready.is_empty() {
            out.push(Check::ok(s, "devices", format!("{} ready: {}", ready.len(), ready.join(", "))));
        } else if !problems {
            out.push(Check::ok(s, "devices", "none connected (`kargo run -t android` asks before starting an emulator)"));
        }
    }
    if report.avds.is_empty() {
        out.push(Check::ok(s, "virtual devices", "none installed (`kargo android avd create` makes one)"));
    } else {
        let names: Vec<String> = report.avds.iter().map(|a| a.name.clone()).collect();
        out.push(Check::ok(s, "virtual devices", format!("{} installed: {}", names.len(), names.join(", "))));
    }
    match &report.emulator {
        Some(path) => out.push(Check::ok(s, "emulator", path.display().to_string())),
        None => out.push(Check::warning(
            s,
            "emulator",
            "the Android emulator package is not installed, so apps cannot run on a virtual device",
            if toolbox.is_managed() {
                format!("run `kargo toolchain install android-sdk {} --emulator`", toolbox.default_api())
            } else {
                "install the `emulator` package with Android Studio's SDK Manager or `sdkmanager emulator`".to_owned()
            },
        )),
    }
    match report.acceleration {
        Acceleration::Available(how) => out.push(Check::ok(s, "emulator acceleration", how)),
        Acceleration::Missing(fix) => {
            out.push(Check::warning(s, "emulator acceleration", "no hardware acceleration, so emulators run very slowly", fix))
        }
        Acceleration::Unknown => {}
    }
}

/// The Node.js and package manager checks of the project's Wasm and JS targets, or, outside a project or without
/// them, what the machine has.
fn web(project: Option<&Project>, config: &Config, out: &mut Vec<Check>) {
    let s = Section::Web;
    let manifests: Vec<&Manifest> = project.map_or_else(Vec::new, |p| p.members().iter().map(|&i| p.package(i).manifest()).collect());
    let wasm: Vec<_> = manifests.iter().filter_map(|m| m.targets.wasm.as_ref()).collect();
    let js: Vec<_> = manifests.iter().filter_map(|m| m.targets.js.as_ref()).collect();
    let used = !wasm.is_empty() || !js.is_empty();
    let needs_node =
        wasm.iter().any(|w| w.environment != WasmEnvironment::Browser) || js.iter().any(|j| j.environment != JsEnvironment::Browser);
    let needs_browser =
        wasm.iter().any(|w| w.environment == WasmEnvironment::Browser) || js.iter().any(|j| j.environment == JsEnvironment::Browser);
    match kargo_util::process::find_on_path("node") {
        Some(node) => out.push(Check::ok(s, "Node.js", node.display().to_string())),
        None if needs_node => out.push(Check::error(
            s,
            "Node.js",
            "`node` is not on PATH; `kargo run` and `kargo test` need it",
            "install Node.js (https://nodejs.org) and put `node` on PATH",
        )),
        None if used => out.push(Check::warning(
            s,
            "Node.js",
            "`node` is not on PATH; npm dependencies and TypeScript sources need it",
            "install Node.js (https://nodejs.org) and put `node` on PATH",
        )),
        None => out.push(Check::ok(
            s,
            "Node.js",
            "not found; only Wasm and JS projects need it (for Node.js environments, npm dependencies and TypeScript sources)",
        )),
    }

    // The package manager of `package-manager`, or the first of npm, pnpm and bun that is installed.
    let configured: Vec<PackageManager> =
        wasm.iter().filter_map(|w| w.package_manager).chain(js.iter().filter_map(|j| j.package_manager)).collect();
    let needs_packages = wasm.iter().any(|w| !w.npm_dependencies.is_empty()) || js.iter().any(|j| !j.npm_dependencies.is_empty());
    for manager in &configured {
        if kargo_util::process::find_on_path(manager.as_str()).is_none() {
            out.push(Check::error(
                s,
                manager.as_str(),
                format!("`package-manager` is `{manager}`, but `{manager}` is not on PATH"),
                format!("install {manager}, or choose npm, pnpm or bun in `package-manager`"),
            ));
        }
    }
    let found: Vec<(PackageManager, PathBuf)> =
        PackageManager::ALL.into_iter().filter_map(|m| kargo_util::process::find_on_path(m.as_str()).map(|exe| (m, exe))).collect();
    if let Some((manager, exe)) = found.iter().find(|(m, _)| configured.contains(m)).or(found.first()) {
        out.push(Check::ok(s, "package manager", format!("{manager} at {}", exe.display())));
    } else if configured.is_empty() {
        const FIX: &str = "install Node.js (it includes npm), pnpm or bun and put it on PATH";
        const NEEDED: &str = "no npm, pnpm or bun on PATH, which installs npm dependencies";
        if needs_packages {
            out.push(Check::error(s, "package manager", NEEDED, FIX));
        } else if used {
            out.push(Check::warning(s, "package manager", format!("{NEEDED} (libraries that depend on npm packages need one)"), FIX));
        } else {
            out.push(Check::ok(
                s,
                "package manager",
                "none of npm, pnpm and bun found; only Wasm and JS projects with npm dependencies need one",
            ));
        }
    }
    if !used {
        return;
    }
    match kargo_toolchain::browser::browser() {
        Ok(browser) => out.push(Check::ok(s, "browser", browser.exe.display().to_string())),
        Err(e) if needs_browser => out.push(Check::error(s, "browser", chain(&e), "install Chrome, Edge, Chromium, Brave or Firefox")),
        Err(_) => {}
    }
    if !wasm.is_empty() {
        match kargo_toolchain::binaryen::find(config) {
            Some(opt) => out.push(Check::ok(s, "wasm-opt", opt.display().to_string())),
            None => out.push(Check::ok(
                s,
                "wasm-opt",
                format!("Binaryen {} is not installed yet; `kargo build --release` downloads it", kargo_toolchain::binaryen::VERSION),
            )),
        }
    }
}

fn network(config: &Config, project: Option<&Project>, out: &mut Vec<Check>) {
    let s = Section::Network;
    let settings = config.settings();
    match settings.proxy() {
        Some("") => out.push(Check::ok(s, "proxy", "none ([http] proxy = \"\")")),
        Some(proxy) => out.push(Check::ok(s, "proxy", format!("{} ([http] proxy)", redact_url(proxy)))),
        None => {
            let env = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
                .into_iter()
                .find_map(|v| Some((v, std::env::var(v).ok().filter(|p| !p.is_empty())?)));
            match env {
                Some((var, proxy)) => out.push(Check::ok(s, "proxy", format!("{} ({var})", redact_url(&proxy)))),
                None => out.push(Check::ok(s, "proxy", "none")),
            }
        }
    }
    if let Some(cainfo) = settings.cainfo() {
        out.push(Check::ok(s, "certificates", format!("{} ([http] cainfo)", cainfo.display())));
    }

    let declared = project.map_or_else(|| vec![Repository::maven_central()], Project::repositories);
    for missing in missing_credentials(config, project, &declared) {
        out.push(missing);
    }
    let repos = effective_repositories(config, &declared);
    if config.offline() {
        out.push(Check::ok(s, "repositories", "not contacted (offline)"));
        return;
    }
    let agent = kargo_util::http::agent(config, NETWORK_TIMEOUT);
    for repo in repos {
        out.push(probe(config, &agent, &repo));
    }
}

/// Whether a repository answers. Any HTTP answer but a denial counts: many repositories answer
/// 404 at their root.
fn probe(config: &Config, agent: &ureq::Agent, repo: &Repository) -> Check {
    let s = Section::Network;
    let name = format!("repository {}", repo.name);
    let url = redact_url(&repo.url);
    if repo.is_local() {
        let dir = repo.local_root();
        return if dir.is_dir() {
            Check::ok(s, name, format!("{url} (directory)"))
        } else {
            Check::error(s, name, format!("{} does not exist", dir.display()), "create it, or fix the repository's `url`")
        };
    }
    let mut request = agent.get(format!("{}/", repo.url));
    let credentials =
        repo.credentials.as_ref().map(|c| (c.username.clone(), c.password.clone())).or_else(|| config.settings().credentials(&repo.url));
    if let Some((user, password)) = &credentials {
        request = request.header("Authorization", kargo_util::http::basic_auth(user, password));
    }
    match request.call() {
        Ok(response) => match response.status().as_u16() {
            code @ (401 | 403) => Check::warning(
                s,
                name,
                format!("{url} denies access (HTTP {code})"),
                if credentials.is_some() {
                    "check the repository's credentials"
                } else {
                    "set `username-env` and `password-env` for the repository in kargo.toml, or credentials in ~/.kargo/config.toml"
                },
            ),
            code if code >= 500 => {
                Check::warning(s, name, format!("{url} answers HTTP {code}"), "the repository has trouble; try again later")
            }
            code => Check::ok(s, name, format!("{url} (HTTP {code})")),
        },
        Err(e) => Check::error(
            s,
            name,
            format!("{url} is unreachable: {e}"),
            "check the connection; behind a proxy set [http] proxy in ~/.kargo/config.toml (or HTTPS_PROXY), and [http] cainfo if it inspects TLS",
        ),
    }
}

/// Repositories whose `username-env` / `password-env` (in `kargo.toml` or the settings) name unset variables.
fn missing_credentials(config: &Config, project: Option<&Project>, declared: &[Repository]) -> Vec<Check> {
    let s = Section::Network;
    let mut out = Vec::new();
    let unset = |var: &str| std::env::var_os(var).is_none_or(|v| v.is_empty());
    for ws in project.map(Project::packages).unwrap_or_default() {
        let path = ws.manifest_path();
        let Ok(table) = std::fs::read_to_string(&path).unwrap_or_default().parse::<toml::Table>() else { continue };
        let Some(repos) = table.get("repositories").and_then(toml::Value::as_table) else { continue };
        for (name, repo) in repos {
            let vars: Vec<&str> =
                ["username-env", "password-env"].iter().filter_map(|k| repo.get(*k)?.as_str()).filter(|v| unset(v)).collect();
            if !vars.is_empty() {
                out.push(Check::warning(
                    s,
                    format!("repository {name}"),
                    format!("its credentials are not set ({} unset)", vars.join(", ")),
                    format!("set {} before building", vars.join(" and ")),
                ));
            }
        }
    }
    for repo in declared {
        if let Some(env) = config.settings().credentials_env(&repo.url)
            && config.settings().credentials(&repo.url).is_none()
        {
            let vars: Vec<&str> = [env.username_env.as_str(), env.password_env.as_str()].into_iter().filter(|v| unset(v)).collect();
            out.push(Check::warning(
                s,
                format!("repository {}", repo.name),
                format!("the credentials of config.toml are not set ({} unset)", vars.join(", ")),
                format!("set {} before building", vars.join(" and ")),
            ));
        }
    }
    out
}

fn project_checks(project: &Project, is_builtin_command: &dyn Fn(&str) -> bool, out: &mut Vec<Check>) {
    let s = Section::Project;
    for &i in project.members() {
        let ws = project.package(i);
        let messages = warnings::manifest(ws.manifest(), is_builtin_command);
        let name = format!("package {}", ws.name());
        if messages.is_empty() {
            out.push(Check::ok(s, name.clone(), format!("{} loads without warnings", ws.manifest_path().display())));
        }
        for message in messages {
            out.push(Check::warning(s, name.clone(), message, "see `reference/warnings.md` in the documentation"));
        }
    }
    build_directory_locks(project, out);
    lockfile(project, out);
}

/// Warns once per build directory (the members of a workspace share `target/`) that another process holds its lock.
fn build_directory_locks(project: &Project, out: &mut Vec<Check>) {
    let mut seen: Vec<&Path> = Vec::new();
    for &i in project.members() {
        let dir = project.package(i).target_dir();
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir);
        let lock = dir.join(BUILD_LOCK_NAME);
        if !flock::is_held(&lock) {
            continue;
        }
        let (what, fix) = match flock::holder(&lock) {
            Some(holder) => (
                format!("{holder} is using {}", dir.display()),
                format!("wait for it to finish, or stop process {} if it is hung", holder.pid),
            ),
            None => (
                format!("another Kargo process is using {}", dir.display()),
                "wait for it to finish; if no build is running, a hung `kargo` process holds the lock: stop it".to_owned(),
            ),
        };
        out.push(Check::warning(Section::Project, "build directory", what, fix));
    }
}

fn lockfile(project: &Project, out: &mut Vec<Check>) {
    let s = Section::Project;
    let path = project.lockfile_path();
    let declared: Vec<String> = project
        .members()
        .iter()
        .map(|&i| project.package(i).manifest())
        .filter(|m| m.targets.enabled().iter().any(|t| *t != TargetKind::Android))
        .flat_map(|m| m.dependencies.iter().chain(&m.dev_dependencies))
        .filter(|d| matches!(d.scope, Scope::Compile | Scope::Runtime | Scope::Provided))
        .map(|d| d.module.to_string())
        .collect();
    if !path.is_file() {
        if declared.is_empty() {
            out.push(Check::ok(s, "kargo.lock", "not created yet (no dependencies)"));
        } else {
            out.push(Check::warning(s, "kargo.lock", "missing", "`kargo fetch` (or any build) creates it; commit it"));
        }
        return;
    }
    let lock = match Lockfile::load(&path) {
        Ok(lock) => lock,
        Err(e) => {
            out.push(Check::error(s, "kargo.lock", chain(&e), "delete it; the next build writes it again"));
            return;
        }
    };
    // A Kotlin Multiplatform library is locked under its platform's module, such as `-jvm`.
    let locked = |module: &str| lock.contains_module(module) || lock.module_versions().keys().any(|m| m.starts_with(&format!("{module}-")));
    let unlocked: Vec<&String> = declared.iter().filter(|m| !locked(m)).collect();
    if unlocked.is_empty() {
        out.push(Check::ok(s, "kargo.lock", "in sync with kargo.toml"));
    } else {
        out.push(Check::warning(
            s,
            "kargo.lock",
            format!("does not lock {}", unlocked.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", ")),
            "run `kargo fetch` to update it",
        ));
    }
}

/// The user and password of a URL, replaced: `https://u:p@host` becomes `https://***@host`.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}://***{}", &rest[at..]),
        None => url.to_owned(),
    }
}

/// Renders the report as text, by section.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let mut section = None;
    for check in &report.checks {
        if section != Some(check.section) {
            if section.is_some() {
                out.push('\n');
            }
            let _ = writeln!(out, "{}", check.section.title());
            section = Some(check.section);
        }
        let _ = writeln!(out, "  {:<8} {}: {}", check.status.as_str(), check.name, check.message);
        if let Some(fix) = &check.fix {
            let _ = writeln!(out, "  {:<8} fix: {fix}", "");
        }
    }
    let (errors, warnings) = (report.count(Status::Error), report.count(Status::Warning));
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    out.push('\n');
    if errors + warnings == 0 {
        out.push_str("No problems found.\n");
    } else {
        let _ = writeln!(out, "{}, {}.", plural(errors, "error"), plural(warnings, "warning"));
    }
    out
}

/// The report as text to paste into a bug report: the home directory shortened to `~`, in a
/// Markdown code block.
pub fn bug_report(report: &Report) -> String {
    let mut text = render(report);
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy().into_owned();
        if home.len() > 1 {
            text = text.replace(&home, "~").replace(&home.replace('\\', "/"), "~");
        }
    }
    format!("```text\n$ kargo doctor\n{text}```\n")
}

/// Describes the report for `kargo doctor --format json`.
pub fn json(report: &Report) -> Value {
    let checks: Vec<Value> = report
        .checks
        .iter()
        .map(|c| {
            json!({
                "section": c.section.as_str(),
                "name": c.name,
                "status": c.status.as_str(),
                "message": c.message,
                "fix": c.fix,
                "fixable": c.repair.is_some(),
            })
        })
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "errors": report.count(Status::Error),
        "warnings": report.count(Status::Warning),
        "checks": checks,
        "fixed": report.fixed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_in_urls() {
        assert_eq!(redact_url("http://user:secret@proxy:8080"), "http://***@proxy:8080");
        assert_eq!(redact_url("https://repo.example.com/maven"), "https://repo.example.com/maven");
        assert_eq!(redact_url("https://host/path@x"), "https://host/path@x");
        assert_eq!(redact_url("proxy:3128"), "proxy:3128");
    }

    #[test]
    fn finds_incomplete_installs() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["21.0.1", "21.0.2", ".lock"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        let found = incomplete(dir.path(), &[dir.path().join("21.0.1")], "JDK");
        assert_eq!(found, vec![("JDK 21.0.2".to_owned(), dir.path().join("21.0.2"))]);
        assert!(incomplete(&dir.path().join("missing"), &[], "JDK").is_empty());
    }

    #[test]
    fn renders_sections_fixes_and_a_summary() {
        let report = Report {
            checks: vec![
                Check::ok(Section::Kargo, "version", "kargo 1.0"),
                Check::error(Section::Jdk, "project JDK", "too old", "install JDK 21"),
                Check::warning(Section::Jdk, "JAVA_HOME", "points nowhere", "unset it"),
            ],
            fixed: Vec::new(),
        };
        let text = render(&report);
        assert!(
            text.starts_with(
                "Kargo\n  ok       version: kargo 1.0\n\nJDK\n  error    project JDK: too old\n           fix: install JDK 21\n"
            )
        );
        assert!(text.ends_with("\n1 error, 1 warning.\n"));
        assert!(report.has_errors());
        assert_eq!(json(&report)["checks"][1]["status"], "error");
    }

    #[test]
    fn fix_removes_incomplete_installs() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("jdk-21");
        std::fs::create_dir_all(broken.join("lib")).unwrap();
        let mut report = Report {
            checks: vec![
                Check::warning(Section::Toolchains, "JDK jdk-21", "incomplete", "--fix").with_repair(Repair::RemoveDir(broken.clone())),
            ],
            fixed: Vec::new(),
        };
        fix(&mut report);
        assert!(!broken.exists());
        assert_eq!(report.checks[0].status, Status::Ok);
        assert_eq!(report.fixed.len(), 1);
    }
}
