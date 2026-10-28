//! Android devices for `kargo run -t android` and `kargo android`: what is connected, which one to use, the
//! virtual devices (AVDs) Kargo creates and starts, and launching the app on one.
//!
//! Everything goes through the SDK's own tools, so a fake `adb` / `emulator` / `avdmanager` can stand in for them:
//! `adb devices -l` and `getprop` describe the devices, `emulator -avd` boots a virtual device, `avdmanager` creates
//! one, and `am start -W` starts the app. Every `adb` call that targets a device passes `-s <serial>`, so a second
//! device that is attached is never touched.
//!
//! Kargo never starts or creates an emulator by itself: it asks first, or the user names the virtual device with
//! `--device`. Without a terminal there is nobody to ask, and the error lists what is available.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use kargo_manifest::{AndroidSdkRequest, Manifest};
use kargo_toolchain::android_sdk::{self, Licenses};
use kargo_util::config::Config;
use kargo_util::process;
use kargo_util::shell::Shell;

/// The environment variable that names the device to use, like `--device`.
pub const DEVICE_ENV: &str = "KARGO_ANDROID_DEVICE";

/// How long a started emulator has to finish booting.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a single `adb` query may take. A device that hangs must not hang Kargo.
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

/// The API level a new virtual device gets when neither the project nor an installed image names one.
const DEFAULT_API: u32 = 36;

/// The SDK and the tools of it that device commands use.
pub struct Toolbox<'a> {
    config: &'a Config,
    sdk: Option<PathBuf>,
    /// Whether the SDK is Kargo's own (`[toolchain] android-sdk`), whose virtual devices Kargo keeps next to it.
    managed: bool,
    /// The `compile-sdk` of the project, the API level of a new virtual device.
    compile_sdk: Option<u32>,
}

impl<'a> Toolbox<'a> {
    /// Finds the SDK without installing anything. A project with an Android target and `[toolchain] android-sdk` uses
    /// Kargo's SDK, one without it the system SDK; outside such a project the SDK of `ANDROID_HOME` (or Android Studio's
    /// default location), then Kargo's.
    pub fn locate(config: &'a Config, manifest: Option<&Manifest>) -> Self {
        let android = manifest.and_then(|m| m.targets.android.as_ref().map(|a| (m, a)));
        let compile_sdk = android.map(|(_, a)| a.config.compile_sdk);
        let managed_dir = config.android_sdk_dir();
        let (sdk, managed) = match android {
            Some((m, _)) if m.toolchain.android_sdk != AndroidSdkRequest::System => (Some(managed_dir), true),
            Some(_) => (crate::gradle::android_sdk(), false),
            None => match crate::gradle::android_sdk() {
                Some(sdk) => (Some(sdk), false),
                None if managed_dir.is_dir() => (Some(managed_dir), true),
                None => (None, false),
            },
        };
        Self { config, sdk, managed, compile_sdk }
    }

    pub fn sdk(&self) -> Option<&Path> {
        self.sdk.as_deref()
    }

    /// Whether the SDK is the one Kargo manages.
    pub fn is_managed(&self) -> bool {
        self.managed
    }

    /// The API level a new virtual device gets: the project's `compile-sdk`, else the newest system image that is
    /// installed, else [`DEFAULT_API`].
    pub fn default_api(&self) -> u32 {
        self.compile_sdk.or_else(|| self.sdk.as_deref().and_then(newest_image_api)).unwrap_or(DEFAULT_API)
    }

    /// `adb`: the SDK's, else the one on `PATH`.
    pub fn adb(&self) -> Result<Adb> {
        let path = self.sdk.as_deref().and_then(android_sdk::adb_in).or_else(|| process::find_on_path("adb")).with_context(|| {
            if self.managed {
                "`adb` not found; install it with `kargo toolchain install android-sdk`".to_owned()
            } else {
                "`adb` not found; install the Android SDK platform-tools and set ANDROID_HOME to the SDK, or put `adb` on PATH, \
                 or set `android-sdk = \"<api>\"` under [toolchain] in kargo.toml to have Kargo install it"
                    .to_owned()
            }
        })?;
        Ok(Adb { path })
    }

    /// The emulator, when it is installed.
    pub fn emulator(&self) -> Result<PathBuf> {
        self.sdk.as_deref().and_then(android_sdk::emulator_in).or_else(|| process::find_on_path("emulator")).with_context(|| {
            if self.managed {
                format!(
                    "the Android emulator is not installed; run `kargo toolchain install android-sdk {} --emulator`",
                    self.default_api()
                )
            } else {
                "the Android emulator is not installed; install the `emulator` package with Android Studio's SDK Manager or \
                 `sdkmanager emulator`, or set `android-sdk = \"<api>\"` under [toolchain] in kargo.toml to have Kargo manage the SDK"
                    .to_owned()
            }
        })
    }

    /// `avdmanager`, when the command-line tools are installed.
    pub fn avdmanager(&self) -> Result<PathBuf> {
        self.sdk.as_deref().and_then(android_sdk::avdmanager_in).context(
            "`avdmanager` is not installed; install the \"Android SDK Command-line Tools\" with Android Studio's SDK Manager, \
             or let Kargo manage the SDK (`kargo toolchain install android-sdk <api> --emulator`)",
        )
    }

    /// The directory that holds the virtual devices: Kargo's for its own SDK, else where Android Studio and
    /// `avdmanager` keep them.
    pub fn avd_home(&self) -> PathBuf {
        if self.managed {
            return self.config.android_avd_dir();
        }
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        var("ANDROID_AVD_HOME")
            .or_else(|| var("ANDROID_USER_HOME").map(|d| d.join("avd")))
            .or_else(|| var("ANDROID_SDK_HOME").map(|d| d.join(".android").join("avd")))
            .or_else(|| dirs::home_dir().map(|h| h.join(".android").join("avd")))
            .unwrap_or_else(|| PathBuf::from(".android/avd"))
    }

    /// A command for an SDK tool, with the environment that makes it use this SDK and these virtual devices.
    fn tool(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        if let Some(sdk) = &self.sdk {
            cmd.env("ANDROID_HOME", sdk).env("ANDROID_SDK_ROOT", sdk);
        }
        if self.managed {
            cmd.env("ANDROID_AVD_HOME", self.avd_home());
        }
        cmd
    }
}

/// The API level of the newest system image installed in `sdk`.
fn newest_image_api(sdk: &Path) -> Option<u32> {
    android_sdk::installed_system_images(sdk).iter().filter_map(|p| image_api(&p.path)).max()
}

/// The API level in the path of a system image package (`system-images;android-35;google_apis;x86_64`).
fn image_api(package: &str) -> Option<u32> {
    let platform = package.split(';').nth(1)?.strip_prefix("android-")?;
    platform.split('.').next()?.parse().ok()
}

/// The `adb` executable.
#[derive(Debug, Clone)]
pub struct Adb {
    path: PathBuf,
}

/// What a finished command wrote.
#[derive(Debug)]
pub struct Captured {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    /// The first line of the output that says something, for messages.
    fn message(&self) -> String {
        let text = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output").to_owned()
    }
}

/// Runs `cmd` with its output captured and killed after `timeout`.
fn run_captured(mut cmd: Command, timeout: Duration) -> Result<Captured> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let name = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().with_context(|| format!("failed to start `{name}`"))?;
    fn drain<R: Read + Send + 'static>(stream: Option<R>) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            bytes
        })
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().with_context(|| format!("failed to wait for `{name}`"))? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("`{name}` did not answer within {} seconds", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let text = |handle: thread::JoinHandle<Vec<u8>>| String::from_utf8_lossy(&handle.join().unwrap_or_default()).replace("\r\n", "\n");
    Ok(Captured { success: status.success(), stdout: text(out), stderr: text(err) })
}

impl Adb {
    /// `adb`, for the device `serial` when given.
    fn command(&self, serial: Option<&str>) -> Command {
        let mut cmd = Command::new(&self.path);
        if let Some(serial) = serial {
            cmd.arg("-s").arg(serial);
        }
        cmd
    }

    /// Runs `adb [-s serial] args` and captures its output.
    pub fn capture(&self, serial: Option<&str>, args: &[&str], timeout: Duration) -> Result<Captured> {
        let mut cmd = self.command(serial);
        cmd.args(args);
        run_captured(cmd, timeout)
    }

    /// The system property `name` of the device, when it has one.
    pub fn prop(&self, serial: &str, name: &str) -> Option<String> {
        let out = self.capture(Some(serial), &["shell", "getprop", name], QUERY_TIMEOUT).ok()?;
        let value = out.stdout.trim().to_owned();
        (out.success && !value.is_empty()).then_some(value)
    }

    /// The devices `adb` sees, in whatever state.
    pub fn devices(&self) -> Result<Vec<AdbEntry>> {
        let out = self.capture(None, &["devices", "-l"], Duration::from_secs(60))?;
        if !out.success {
            bail!("`adb devices` failed: {}", out.message());
        }
        Ok(parse_devices(&out.stdout))
    }

    /// Whether the device finished booting.
    pub fn is_booted(&self, serial: &str) -> bool {
        self.prop(serial, "sys.boot_completed").is_some_and(|v| v == "1")
    }

    /// The name of the virtual device behind the emulator `serial`.
    fn avd_name(&self, serial: &str) -> Option<String> {
        let out = self.capture(Some(serial), &["emu", "avd", "name"], QUERY_TIMEOUT).ok()?;
        let name = out.stdout.lines().map(str::trim).next().filter(|l| !l.is_empty() && *l != "OK")?;
        out.success.then(|| name.to_owned())
    }

    /// The process ids of the app `app_id` on the device.
    fn pids(&self, serial: &str, app_id: &str) -> Vec<u32> {
        match self.capture(Some(serial), &["shell", "pidof", app_id], QUERY_TIMEOUT) {
            Ok(out) if out.success => out.stdout.split_whitespace().filter_map(|p| p.parse().ok()).collect(),
            _ => Vec::new(),
        }
    }
}

/// A line of `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbEntry {
    pub serial: String,
    /// `device`, `unauthorized`, `offline`, `no permissions`, ...
    pub state: String,
    pub model: Option<String>,
}

/// Reads the output of `adb devices -l`.
pub fn parse_devices(text: &str) -> Vec<AdbEntry> {
    text.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('*') {
                return None;
            }
            let (serial, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            // "no permissions (user in plugdev group; are your udev rules wrong?); see [...]"
            let state = if rest.starts_with("no permissions") { "no permissions" } else { rest.split_whitespace().next()? };
            let model = rest.split_whitespace().find_map(|w| w.strip_prefix("model:")).map(|m| m.replace('_', " "));
            Some(AdbEntry { serial: serial.to_owned(), state: state.to_owned(), model })
        })
        .collect()
}

/// What a [`Device`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// A phone or tablet, over USB or Wi-Fi.
    Physical,
    /// A running emulator.
    Emulator,
    /// A virtual device that is installed but not running.
    Avd,
}

impl DeviceKind {
    fn label(self) -> &'static str {
        match self {
            DeviceKind::Physical => "device",
            DeviceKind::Emulator => "emulator",
            DeviceKind::Avd => "avd",
        }
    }
}

/// A target for `kargo run -t android`.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    /// The serial of a connected device, or the name of a virtual device that is not running.
    pub id: String,
    /// A readable name: the maker and model, or the name of the virtual device.
    pub name: String,
    pub kind: DeviceKind,
    /// `device` for a device that is ready, `unauthorized`, `offline` or `no permissions` for one that is not, and
    /// `stopped` for a virtual device that is not running.
    pub state: String,
    /// The Android API level.
    pub api: Option<u32>,
    /// The virtual device an emulator runs.
    pub avd: Option<String>,
}

impl Device {
    /// Whether apps can be installed on it now.
    pub fn is_ready(&self) -> bool {
        self.kind != DeviceKind::Avd && self.state == "device"
    }

    /// `emulator-5554  Pixel_8_API_35 (emulator)`, the line of the list the user picks from.
    fn describe(&self) -> String {
        let api = self.api.map(|a| format!(", API {a}")).unwrap_or_default();
        format!("{:<18} {} ({}{api})", self.id, self.name, self.kind.label())
    }

    /// What is wrong with a connected device, and what to do about it.
    fn problem(&self) -> Option<String> {
        let id = &self.id;
        match (self.kind, self.state.as_str()) {
            (DeviceKind::Avd, _) | (_, "device") => None,
            (_, "unauthorized") => Some(format!(
                "{id} is unauthorized: unlock the device and accept the \"Allow USB debugging?\" prompt (tick \"Always allow from this computer\"); \
                 if no prompt appears, unplug the cable, revoke the USB debugging authorizations in Developer options and plug it in again"
            )),
            (_, "offline") => Some(format!(
                "{id} is offline: reconnect the cable, turn USB debugging off and on in Developer options, or run `adb kill-server`; \
                 for a wireless device run `kargo android connect <host:port>` again"
            )),
            (_, "no permissions") => Some(format!(
                "{id} cannot be used because this user has no permission to access it: {}",
                if cfg!(windows) {
                    "install the USB driver of the device's maker (https://developer.android.com/studio/run/oem-usb), then reconnect it"
                } else if cfg!(target_os = "macos") {
                    "reconnect it and accept the prompt on the device; if it keeps failing, run `adb kill-server`"
                } else {
                    "add udev rules for its vendor (https://developer.android.com/studio/run/device#setting-up) and make sure you are in \
                     the plugdev group, then reconnect it"
                }
            )),
            (_, state) => {
                Some(format!("{id} is in the `{state}` state and cannot run apps; wait for it to finish starting, or reconnect it"))
            }
        }
    }
}

/// A virtual device on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avd {
    pub name: String,
    pub dir: PathBuf,
    pub api: Option<u32>,
    /// The system image package it runs.
    pub image: Option<String>,
    /// The hardware profile, such as `pixel_8`.
    pub hardware: Option<String>,
}

/// Reads a `key=value` file, as `config.ini` and the AVD `.ini` files are.
fn properties(text: &str) -> BTreeMap<String, String> {
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect()
}

/// The virtual devices in `home`.
pub fn avds(home: &Path) -> Vec<Avd> {
    let Ok(entries) = std::fs::read_dir(home) else { return Vec::new() };
    let mut out: Vec<Avd> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.strip_suffix(".ini")?.to_owned();
            let ini = properties(&std::fs::read_to_string(&path).ok()?);
            let dir = ini.get("path").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| home.join(format!("{name}.avd")));
            let config = std::fs::read_to_string(dir.join("config.ini")).map(|t| properties(&t)).unwrap_or_default();
            // `image.sysdir.1=system-images/android-35/google_apis/x86_64/`
            let image = config.get("image.sysdir.1").map(|d| d.trim_matches(['/', '\\']).replace(['/', '\\'], ";"));
            let api = image.as_deref().and_then(image_api).or_else(|| {
                ini.get("target").and_then(|t| t.strip_prefix("android-")).and_then(|a| a.split('.').next()).and_then(|a| a.parse().ok())
            });
            Some(Avd { name, dir, api, image, hardware: config.get("hw.device.name").cloned() })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Everything `kargo run` can target: the devices `adb` sees, then the virtual devices that are not running.
pub fn list(tb: &Toolbox, shell: &Shell) -> Result<Vec<Device>> {
    let mut out = Vec::new();
    let mut running: Vec<String> = Vec::new();
    match tb.adb() {
        Ok(adb) => {
            for entry in adb.devices()? {
                let emulator = entry.serial.starts_with("emulator-");
                let ready = entry.state == "device";
                let prop = |name: &str| if ready { adb.prop(&entry.serial, name) } else { None };
                let avd = if emulator && ready { adb.avd_name(&entry.serial) } else { None };
                let api = prop("ro.build.version.sdk").and_then(|a| a.parse().ok());
                let name = if let Some(avd) = &avd {
                    avd.clone()
                } else {
                    let maker = prop("ro.product.manufacturer");
                    let model = prop("ro.product.model").or_else(|| entry.model.clone());
                    match (maker, model) {
                        (Some(maker), Some(model)) if !model.to_lowercase().starts_with(&maker.to_lowercase()) => {
                            format!("{maker} {model}")
                        }
                        (_, Some(model)) => model,
                        (Some(maker), None) => maker,
                        (None, None) => entry.serial.clone(),
                    }
                };
                running.extend(avd.clone());
                let kind = if emulator { DeviceKind::Emulator } else { DeviceKind::Physical };
                out.push(Device { id: entry.serial, name, kind, state: entry.state, api, avd });
            }
        }
        Err(e) => shell.verbose("Skipping", format!("connected devices: {e:#}")),
    }
    for avd in avds(&tb.avd_home()) {
        if !running.contains(&avd.name) {
            out.push(Device {
                id: avd.name.clone(),
                name: avd.name,
                kind: DeviceKind::Avd,
                state: "stopped".to_owned(),
                api: avd.api,
                avd: None,
            });
        }
    }
    Ok(out)
}

/// The table `kargo android devices` prints.
pub fn render_table(devices: &[Device]) -> String {
    if devices.is_empty() {
        return "No devices or virtual devices found.\n".to_owned();
    }
    let width = |f: fn(&Device) -> usize| devices.iter().map(f).max().unwrap_or(0);
    let (id_w, name_w) = (width(|d| d.id.len()).max(2), width(|d| d.name.chars().count()).max(4));
    let mut out = format!("{:<id_w$}  {:<name_w$}  {:<8}  {:<4}  STATE\n", "ID", "NAME", "TYPE", "API");
    for d in devices {
        let api = d.api.map(|a| a.to_string()).unwrap_or_else(|| "-".to_owned());
        out.push_str(&format!("{:<id_w$}  {:<name_w$}  {:<8}  {:<4}  {}\n", d.id, d.name, d.kind.label(), api, d.state));
    }
    out
}

/// The devices as a list for an error message.
fn available(devices: &[Device]) -> String {
    if devices.is_empty() {
        return "no devices or virtual devices found".to_owned();
    }
    let lines: Vec<String> = devices.iter().map(|d| format!("  {}  [{}]", d.describe(), d.state)).collect();
    format!("available:\n{}", lines.join("\n"))
}

/// How an emulator starts.
#[derive(Debug, Clone)]
pub struct EmulatorOptions {
    /// Without a window or sound (`--headless`).
    pub headless: bool,
    /// Boot without the saved snapshot (`--cold-boot`).
    pub cold_boot: bool,
    /// Erase the user data first (`--wipe-data`).
    pub wipe_data: bool,
    /// How long to wait for the boot (`--boot-timeout`).
    pub timeout: Duration,
}

impl Default for EmulatorOptions {
    fn default() -> Self {
        Self { headless: false, cold_boot: false, wipe_data: false, timeout: BOOT_TIMEOUT }
    }
}

/// The Android options of `kargo run`.
#[derive(Debug, Clone, Default)]
pub struct AndroidRun {
    /// `--device`: a serial or the name of a virtual device. Without it the device is chosen as [`select`] says.
    pub device: Option<String>,
    /// `--no-logcat`: do not show the app's log, and leave when it started.
    pub no_logcat: bool,
    /// How an emulator that has to be started boots.
    pub emulator: EmulatorOptions,
    /// `--stop-emulator`: shut down an emulator that this run started once the app is done.
    pub stop_emulator: bool,
    /// `--watch`: keep following the log of the app while the next build installs.
    pub watch: bool,
}

impl AndroidRun {
    /// The first flag of an Android run that was given, for refusing it on another target. `--device` is not one, as
    /// `KARGO_ANDROID_DEVICE` sets it for every run.
    pub fn flag_set(&self) -> Option<&'static str> {
        [
            (self.no_logcat, "--no-logcat"),
            (self.stop_emulator, "--stop-emulator"),
            (self.emulator.headless, "--headless"),
            (self.emulator.cold_boot, "--cold-boot"),
            (self.emulator.wipe_data, "--wipe-data"),
        ]
        .into_iter()
        .find_map(|(given, flag)| given.then_some(flag))
    }
}

/// The device a run uses.
#[derive(Debug, Clone)]
pub struct Selection {
    pub serial: String,
    pub name: String,
    /// The emulator was started for this run.
    pub started: bool,
}

/// Picks the device to run on. `request` is `--device` (a serial, a `host:port` of a wireless device, or the name of a
/// virtual device); without it one device is used as it is, several are chosen from on the terminal, and with none the
/// user is asked whether to start or create an emulator.
pub fn select(tb: &Toolbox, shell: &Shell, request: Option<&str>, boot: &EmulatorOptions) -> Result<Selection> {
    let adb = tb.adb()?;
    let mut devices = list(tb, shell)?;
    if let Some(request) = request.map(str::trim).filter(|r| !r.is_empty()) {
        return pick_requested(tb, shell, &adb, &mut devices, request, boot);
    }
    let ready: Vec<&Device> = devices.iter().filter(|d| d.is_ready()).collect();
    match ready.as_slice() {
        [one] => {
            let one = (*one).clone();
            shell.status("Using", one.describe());
            Ok(Selection { serial: one.id, name: one.name, started: false })
        }
        [] => {
            let problems: Vec<String> = devices.iter().filter_map(Device::problem).collect();
            if !problems.is_empty() {
                bail!("no device is ready to run the app:\n  {}", problems.join("\n  "));
            }
            none_connected(tb, shell, &adb, &devices, boot)
        }
        many => {
            let items: Vec<String> = many.iter().map(|d| d.describe()).collect();
            if !shell.is_interactive() {
                bail!(
                    "more than one device is connected and there is no terminal to choose on; pass `--device <serial>` (or set {DEVICE_ENV}). \
                     Ready devices:\n  {}",
                    items.join("\n  ")
                );
            }
            match shell.select("Run on which device?", &items).context("failed to read the choice")? {
                Some(i) => {
                    let chosen = many[i].clone();
                    shell.status("Using", chosen.describe());
                    Ok(Selection { serial: chosen.id, name: chosen.name, started: false })
                }
                None => bail!("no device chosen"),
            }
        }
    }
}

/// `--device <request>`: a connected device, a virtual device (started if it is not running), or a wireless address.
fn pick_requested(
    tb: &Toolbox,
    shell: &Shell,
    adb: &Adb,
    devices: &mut Vec<Device>,
    request: &str,
    boot: &EmulatorOptions,
) -> Result<Selection> {
    let matches = |d: &Device| d.id == request || d.avd.as_deref() == Some(request) || d.name.eq_ignore_ascii_case(request);
    if !devices.iter().any(&matches) && request.contains(':') && request.parse::<std::net::SocketAddr>().is_ok() {
        connect(tb, shell, request)?;
        *devices = list(tb, shell)?;
    }
    let found: Vec<&Device> = devices.iter().filter(|d| matches(d)).collect();
    // A serial wins over a name, and a running emulator over its stopped virtual device.
    let found = found
        .iter()
        .copied()
        .find(|d| d.id == request)
        .or_else(|| found.iter().copied().find(|d| d.kind != DeviceKind::Avd))
        .or(found.first().copied());
    let Some(device) = found.cloned() else {
        bail!("no device or virtual device named `{request}`; {}", available(devices));
    };
    if device.kind == DeviceKind::Avd {
        let started = start_emulator(tb, shell, adb, &device.id, boot)?;
        return Ok(Selection { serial: started, name: device.name, started: true });
    }
    if let Some(problem) = device.problem() {
        bail!("{problem}");
    }
    shell.status("Using", device.describe());
    Ok(Selection { serial: device.id, name: device.name, started: false })
}

/// Nothing is connected: ask what to do, and only do it once the user chose.
fn none_connected(tb: &Toolbox, shell: &Shell, adb: &Adb, devices: &[Device], boot: &EmulatorOptions) -> Result<Selection> {
    let stopped: Vec<&Device> = devices.iter().filter(|d| d.kind == DeviceKind::Avd).collect();
    let how = "connect a device with USB debugging on, `kargo android connect <host:port>` a wireless one, or start a virtual device with \
               `kargo android emulator start <name>`";
    if !shell.is_interactive() {
        let avds = if stopped.is_empty() {
            "there is no virtual device yet; `kargo android avd create` makes one".to_owned()
        } else {
            format!(
                "virtual devices you can start with `--device <name>`:\n  {}",
                stopped.iter().map(|d| d.describe()).collect::<Vec<_>>().join("\n  ")
            )
        };
        bail!("no device or emulator is connected and there is no terminal to ask on; {how}, or pass `--device`. {avds}");
    }
    let mut items: Vec<String> = stopped.iter().map(|d| format!("Start the virtual device {}", d.describe())).collect();
    let create = items.len();
    items.push(format!("Create a new virtual device (Android {})", tb.default_api()));
    let cancel = items.len();
    items.push("Cancel".to_owned());
    shell.note("no device or emulator is connected");
    match shell.select("What should Kargo do?", &items).context("failed to read the choice")? {
        Some(i) if i < create => {
            let device = stopped[i];
            let serial = start_emulator(tb, shell, adb, &device.id, boot)?;
            Ok(Selection { serial, name: device.name.clone(), started: true })
        }
        Some(i) if i == create => {
            let name = create_avd(tb, shell, &NewAvd::new(tb.default_api()), Licenses::Ask)?;
            let serial = start_emulator(tb, shell, adb, &name, boot)?;
            Ok(Selection { serial, name, started: true })
        }
        Some(i) => {
            debug_assert_eq!(i, cancel);
            bail!("cancelled; no device to run on")
        }
        None => bail!("cancelled; no device to run on"),
    }
}

/// Starts the virtual device `avd` and waits until it booted. An emulator that already runs it is used as it is.
/// Returns the serial.
pub fn start_emulator(tb: &Toolbox, shell: &Shell, adb: &Adb, avd: &str, opts: &EmulatorOptions) -> Result<String> {
    if !avds(&tb.avd_home()).iter().any(|a| a.name == avd) {
        bail!("no virtual device named `{avd}` in {}; `kargo android avd list` shows them", tb.avd_home().display());
    }
    let before = adb.devices().unwrap_or_default();
    for entry in &before {
        if entry.serial.starts_with("emulator-") && adb.avd_name(&entry.serial).as_deref() == Some(avd) {
            shell.note(format!("{avd} is already running as {}", entry.serial));
            wait_for_boot(adb, shell, &entry.serial, avd, opts.timeout, None)?;
            return Ok(entry.serial.clone());
        }
    }
    let emulator = tb.emulator()?;
    let mut cmd = tb.tool(&emulator);
    cmd.args(["-avd", avd]);
    if opts.headless {
        cmd.args(["-no-window", "-no-audio", "-no-boot-anim"]);
    }
    if opts.cold_boot {
        cmd.arg("-no-snapshot-load");
    }
    if opts.wipe_data {
        cmd.arg("-wipe-data");
    }
    let log = std::env::temp_dir().join(format!("kargo-emulator-{avd}.log"));
    let log_file = std::fs::File::create(&log).with_context(|| format!("failed to create {}", log.display()))?;
    cmd.stdin(Stdio::null()).stdout(log_file.try_clone()?).stderr(log_file);
    shell.status("Starting", format!("the emulator {avd}{}", if opts.headless { " (headless)" } else { "" }));
    shell.verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = process::spawn_detached(&mut cmd).with_context(|| format!("failed to start `{}`", emulator.display()))?;

    let deadline = Instant::now() + opts.timeout;
    let started = Instant::now();
    let serial = loop {
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "the emulator exited ({}) before {avd} started; its output is in {}:\n{}",
                process::describe_status(status),
                log.display(),
                tail(&log)
            );
        }
        let now = adb.devices().unwrap_or_default();
        let new: Vec<&AdbEntry> =
            now.iter().filter(|e| e.serial.starts_with("emulator-") && !before.iter().any(|b| b.serial == e.serial)).collect();
        let named = new.iter().find(|e| adb.avd_name(&e.serial).as_deref() == Some(avd));
        if let Some(entry) = named.copied().or(if new.len() == 1 { new.first().copied() } else { None }) {
            break entry.serial.clone();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            bail!(
                "{avd} did not show up in `adb devices` within {} seconds; the emulator's output is in {}:\n{}",
                opts.timeout.as_secs(),
                log.display(),
                tail(&log)
            );
        }
        thread::sleep(Duration::from_millis(500));
    };
    let remaining = opts.timeout.saturating_sub(started.elapsed());
    wait_for_boot(adb, shell, &serial, avd, remaining, Some((&mut child, &log)))?;
    Ok(serial)
}

/// The last lines of a log, for an error message.
fn tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(15).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Waits for `sys.boot_completed`, with progress output.
fn wait_for_boot(
    adb: &Adb,
    shell: &Shell,
    serial: &str,
    name: &str,
    timeout: Duration,
    mut emulator: Option<(&mut Child, &Path)>,
) -> Result<()> {
    let start = Instant::now();
    let mut last = Duration::ZERO;
    shell.status("Booting", format!("{name} ({serial})"));
    loop {
        if adb.is_booted(serial) {
            shell.status("Booted", format!("{name} in {:.0}s", start.elapsed().as_secs_f64()));
            return Ok(());
        }
        if let Some((child, log)) = emulator.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            bail!(
                "the emulator exited ({}) while {name} was booting; its output is in {}:\n{}",
                process::describe_status(status),
                log.display(),
                tail(log)
            );
        }
        if start.elapsed() >= timeout {
            bail!(
                "{name} ({serial}) did not finish booting within {} seconds; raise `--boot-timeout`, or look at the emulator window",
                timeout.as_secs()
            );
        }
        if start.elapsed() - last >= Duration::from_secs(10) {
            last = start.elapsed();
            shell.status("Waiting", format!("for {name} to boot ({}s)", last.as_secs()));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Shuts the emulator `serial` down.
pub fn stop_emulator(adb: &Adb, shell: &Shell, serial: &str) -> Result<()> {
    let out = adb.capture(Some(serial), &["emu", "kill"], QUERY_TIMEOUT)?;
    if !out.success {
        bail!("failed to stop {serial}: {}", out.message());
    }
    shell.status("Stopped", format!("the emulator {serial}"));
    Ok(())
}

/// A virtual device to create.
#[derive(Debug, Clone)]
pub struct NewAvd {
    /// Its name; `Kargo_API_<api>` when not given.
    pub name: Option<String>,
    pub api: u32,
    /// The hardware profile, such as `pixel_8` (`avdmanager list device` shows them).
    pub hardware: Option<String>,
    /// Replace a virtual device of the same name.
    pub force: bool,
}

impl NewAvd {
    pub fn new(api: u32) -> Self {
        Self { name: None, api, hardware: None, force: false }
    }
}

/// Whether `name` is a valid name of a virtual device.
fn valid_avd_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Creates a virtual device, installing the emulator and the system image of Kargo's SDK first (after asking). Returns
/// its name.
pub fn create_avd(tb: &Toolbox, shell: &Shell, new: &NewAvd, licenses: Licenses) -> Result<String> {
    let name = new.name.clone().unwrap_or_else(|| format!("Kargo_API_{}", new.api));
    if !valid_avd_name(&name) {
        bail!("`{name}` is not a valid name for a virtual device; use letters, digits, `.`, `_` and `-`");
    }
    if !new.force && avds(&tb.avd_home()).iter().any(|a| a.name == name) {
        bail!("a virtual device named `{name}` exists already; pick another name, or pass `--force` to replace it");
    }
    let image = system_image(tb, shell, new.api, licenses)?;
    let avdmanager = tb.avdmanager()?;
    let home = tb.avd_home();
    std::fs::create_dir_all(&home).with_context(|| format!("failed to create {}", home.display()))?;
    let mut cmd = tb.tool(&avdmanager);
    cmd.args(["create", "avd", "--name", &name, "--package", &image]);
    if let Some(hardware) = &new.hardware {
        cmd.args(["--device", hardware]);
    }
    if new.force {
        cmd.arg("--force");
    }
    if let Some(jdk) = avdmanager_jdk(tb.config)? {
        cmd.env("JAVA_HOME", jdk);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    shell.status("Creating", format!("the virtual device {name} ({image})"));
    shell.verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.spawn().with_context(|| format!("failed to start `{}`", avdmanager.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // "Do you wish to create a custom hardware profile? [no]"
        let _ = stdin.write_all(b"no\n");
    }
    let out = child.wait_with_output().context("failed to wait for avdmanager")?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_owned();
        bail!("`avdmanager create avd` failed ({}): {text}", process::describe_status(out.status));
    }
    shell.status("Created", format!("the virtual device {name} in {}", home.display()));
    Ok(name)
}

/// The system image for `api`: installed already, or (in Kargo's SDK, after asking) downloaded with the emulator.
fn system_image(tb: &Toolbox, shell: &Shell, api: u32, licenses: Licenses) -> Result<String> {
    if let Some(sdk) = tb.sdk()
        && let Some(image) = android_sdk::installed_system_image(sdk, api)
        && tb.emulator().is_ok()
        && tb.avdmanager().is_ok()
    {
        return Ok(image);
    }
    if !tb.is_managed() {
        bail!(
            "the SDK{} has no emulator, command-line tools and system image for Android {api}; install the packages `emulator`, \
             `cmdline-tools;latest` and `{}` with Android Studio's SDK Manager or `sdkmanager`, or set `android-sdk = \"<api>\"` \
             under [toolchain] in kargo.toml to have Kargo manage the SDK",
            tb.sdk().map(|s| format!(" at {}", s.display())).unwrap_or_default(),
            android_sdk::system_image_packages(api, "google_apis", android_sdk::host_abi())[0]
        );
    }
    let question = format!(
        "Kargo will download the Android emulator, the command-line tools and a system image for Android {api} (about 1.5 GB) into {}. Continue?",
        tb.config.android_sdk_dir().display()
    );
    match shell.confirm("", question) {
        Some(true) => {}
        Some(false) => bail!("cancelled; nothing was downloaded"),
        // Nobody to ask, but the command was `avd create` or `--accept-licenses`: an explicit request.
        None if licenses == Licenses::Accept => {}
        None => bail!(
            "creating a virtual device downloads the emulator and a system image; run `kargo toolchain install android-sdk {api} --emulator` \
             first, or run this in a terminal to be asked"
        ),
    }
    android_sdk::ensure_emulator(&tb.config.clone().with_auto_install(true), api, licenses).map(|(_, image)| image)
}

/// The `JAVA_HOME` for `avdmanager`, which needs Java 17 or newer: `JAVA_HOME` or `PATH` if that is new enough, else a
/// JDK Kargo manages (installed when automatic installs are on). `None` leaves `avdmanager` to look for Java itself.
fn avdmanager_jdk(config: &Config) -> Result<Option<PathBuf>> {
    use kargo_manifest::JdkRequest;
    if let Some(jdk) = kargo_toolchain::Jdk::find().ok().flatten()
        && jdk.version >= 17
    {
        return Ok(Some(jdk.home));
    }
    if !config.auto_install() {
        return Ok(None);
    }
    Ok(Some(kargo_toolchain::jdks::resolve(config, &JdkRequest::Major(21))?.home))
}

/// Deletes the virtual device `name`.
pub fn delete_avd(tb: &Toolbox, shell: &Shell, name: &str) -> Result<()> {
    let home = tb.avd_home();
    let Some(avd) = avds(&home).into_iter().find(|a| a.name == name) else {
        bail!("no virtual device named `{name}` in {}; `kargo android avd list` shows them", home.display());
    };
    if let Ok(adb) = tb.adb()
        && let Ok(entries) = adb.devices()
        && entries.iter().any(|e| e.serial.starts_with("emulator-") && adb.avd_name(&e.serial).as_deref() == Some(name))
    {
        bail!("{name} is running; stop it first with `kargo android emulator stop {name}`");
    }
    std::fs::remove_file(home.join(format!("{name}.ini"))).with_context(|| format!("failed to remove the {name}.ini file"))?;
    kargo_util::fs::remove_dir_all_if_exists(&avd.dir)?;
    shell.status("Deleted", format!("the virtual device {name}"));
    Ok(())
}

/// The serial of the emulator running the virtual device `name`.
pub fn running_serial(tb: &Toolbox, name: &str) -> Result<Option<String>> {
    let adb = tb.adb()?;
    for entry in adb.devices()? {
        if entry.serial.starts_with("emulator-") && adb.avd_name(&entry.serial).as_deref() == Some(name) {
            return Ok(Some(entry.serial));
        }
    }
    Ok(None)
}

/// `kargo android connect <host:port>`: connects to a device over Wi-Fi.
pub fn connect(tb: &Toolbox, shell: &Shell, address: &str) -> Result<()> {
    let adb = tb.adb()?;
    let out = adb.capture(None, &["connect", address], Duration::from_secs(30))?;
    let text = format!("{}{}", out.stdout, out.stderr);
    let text = text.trim();
    let lower = text.to_lowercase();
    if !out.success || lower.contains("failed") || lower.contains("cannot") || lower.contains("unable") || !lower.contains("connected") {
        bail!(
            "could not connect to {address}: {}\nOn the device, turn on Wireless debugging in Developer options and use the IP address and port \
             it shows (not the pairing port); pair first with `kargo android pair <host:port>` if it asks for a pairing code",
            if text.is_empty() { "no answer" } else { text }
        );
    }
    shell.status("Connected", format!("to {address}"));
    Ok(())
}

/// `kargo android pair <host:port> [code]`: pairs with a device for wireless debugging. Without a code, `adb` asks for it.
pub fn pair(tb: &Toolbox, shell: &Shell, address: &str, code: Option<&str>) -> Result<()> {
    let adb = tb.adb()?;
    match code {
        Some(code) => {
            let out = adb.capture(None, &["pair", address, code], Duration::from_secs(30))?;
            let text = format!("{}{}", out.stdout, out.stderr);
            if !out.success || text.to_lowercase().contains("failed") || !text.to_lowercase().contains("success") {
                bail!(
                    "could not pair with {address}: {}\nUse the pairing address and code from \"Pair device with pairing code\" on the device",
                    text.trim()
                );
            }
            shell.status("Paired", format!("with {address}"));
        }
        None => {
            let mut cmd = adb.command(None);
            cmd.args(["pair", address]);
            process::run(shell, &mut cmd, "adb pair")?;
        }
    }
    shell.note("now connect with `kargo android connect <host:port>`, using the address shown on the Wireless debugging screen");
    Ok(())
}

/// `kargo android disconnect [host:port]`.
pub fn disconnect(tb: &Toolbox, shell: &Shell, address: Option<&str>) -> Result<()> {
    let adb = tb.adb()?;
    let mut args = vec!["disconnect"];
    args.extend(address);
    let out = adb.capture(None, &args, Duration::from_secs(30))?;
    if !out.success {
        bail!("`adb disconnect` failed: {}", out.message());
    }
    shell.status("Disconnected", address.unwrap_or("all wireless devices").to_owned());
    Ok(())
}

const MAIN_ACTION: &str = "android.intent.action.MAIN";
const LAUNCHER_CATEGORY: &str = "android.intent.category.LAUNCHER";

/// The launcher activity of `app_id` as `package/.Activity`, or `None` when the device (an old one has no `cmd package`) cannot say.
fn launcher_activity(adb: &Adb, serial: &str, app_id: &str) -> Option<String> {
    let args = ["shell", "cmd", "package", "resolve-activity", "--brief", "-c", LAUNCHER_CATEGORY, app_id];
    let out = adb.capture(Some(serial), &args, Duration::from_secs(30)).ok()?;
    out.success.then(|| resolved_component(&out.stdout, app_id)).flatten()
}

/// The component `resolve-activity --brief` printed, which is its last line (after a `priority=... isDefault=...` line). An answer
/// that is not an activity of `app_id`, such as "No activity found" or the system's chooser, is not one.
fn resolved_component(output: &str, app_id: &str) -> Option<String> {
    let line = output.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let (package, class) = line.split_once('/')?;
    (package == app_id && !class.is_empty() && !line.contains(char::is_whitespace)).then(|| line.to_owned())
}

/// Starts the launcher activity of `app_id` on `serial` and waits until it is up. Returns the activity.
///
/// The activity is resolved first and started by name, like Android Studio does: `startActivity` adds `CATEGORY_DEFAULT` to an
/// implicit intent, so that never matches the usual launcher filter, which has no `DEFAULT` category.
pub fn launch(adb: &Adb, serial: &str, app_id: &str) -> Result<String> {
    let component = launcher_activity(adb, serial, app_id);
    let args: Vec<&str> = match &component {
        Some(component) => vec!["shell", "am", "start", "-W", "-n", component, "-a", MAIN_ACTION, "-c", LAUNCHER_CATEGORY],
        None => vec!["shell", "am", "start", "-W", "-a", MAIN_ACTION, "-c", LAUNCHER_CATEGORY, "-p", app_id],
    };
    let out = adb.capture(Some(serial), &args, Duration::from_secs(60))?;
    let text = format!("{}\n{}", out.stdout, out.stderr);
    // `am` exits with 0 whatever happens; its output says.
    let status = text.lines().find_map(|l| l.trim().strip_prefix("Status:")).map(|s| s.trim().to_owned());
    let errors: Vec<&str> = text.lines().map(str::trim).filter(|l| l.starts_with("Error") || l.contains("Exception")).collect();
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    if !out.success || error.is_some() || status.as_deref().is_some_and(|s| s != "ok") {
        let why = error.or_else(|| status.map(|s| format!("status {s}"))).unwrap_or_else(|| out.message());
        let hint = if component.is_none() && why.contains("unable to resolve Intent") {
            "check that the app has an activity with the LAUNCHER category in android/AndroidManifest.xml; if it has one, its intent \
             filter may lack android.intent.category.DEFAULT, which only lets the device start it by name"
        } else {
            "check that the app has an activity with the LAUNCHER category in android/AndroidManifest.xml"
        };
        bail!("starting {app_id} on {serial} failed: {why}; {hint}");
    }
    let activity = text.lines().find_map(|l| l.trim().strip_prefix("Activity:")).map(|a| a.trim().to_owned());
    Ok(activity.unwrap_or_else(|| component.unwrap_or_else(|| app_id.to_owned())))
}

/// The process id of the running app, waiting a few seconds for it to appear.
pub fn wait_for_pid(adb: &Adb, serial: &str, app_id: &str) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = adb.pids(serial, app_id).first() {
            return Some(*pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// The `adb logcat` command of the process `pid`, or of the whole device when the process is not known.
pub fn logcat_command(adb: &Adb, serial: &str, pid: Option<u32>) -> Command {
    let mut cmd = adb.command(Some(serial));
    cmd.args(["logcat", "-v", "threadtime"]);
    match pid {
        Some(pid) => cmd.args(["--pid", &pid.to_string()]),
        None => cmd.args(["-T", "1"]),
    };
    cmd
}

/// Set by Ctrl+C while an app is followed.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Makes Ctrl+C set [`INTERRUPTED`] instead of ending Kargo, so the app can be stopped on the device.
fn catch_ctrl_c() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
    });
    INTERRUPTED.store(false, Ordering::SeqCst);
}

/// Whether a logcat line reports that the app died.
fn is_crash(line: &str, app_id: &str) -> bool {
    line.contains("FATAL EXCEPTION")
        || line.contains("Fatal signal")
        || (line.contains("Process: ") && line.contains(app_id) && line.contains("PID:"))
}

/// What happened to a followed app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The app's process ended.
    Exited,
    /// The app crashed.
    Crashed,
    /// The user pressed Ctrl+C, and the app was stopped.
    Stopped,
}

impl Outcome {
    /// The exit code of `kargo run`.
    pub fn code(self) -> i32 {
        match self {
            Outcome::Exited => 0,
            Outcome::Crashed => 1,
            Outcome::Stopped => 130,
        }
    }
}

/// Shows the log of the app on stdout until its process ends or the user presses Ctrl+C, which stops the app.
pub fn follow(adb: &Adb, shell: &Shell, serial: &str, app_id: &str, pid: u32) -> Result<Outcome> {
    catch_ctrl_c();
    shell.status("Logcat", format!("{app_id} (pid {pid}); Ctrl+C stops the app"));
    let mut cmd = logcat_command(adb, serial, Some(pid));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    shell.verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.spawn().context("failed to start `adb logcat`")?;
    let crashed = Arc::new(AtomicBool::new(false));
    let reader = {
        let (crashed, app_id, stdout) = (crashed.clone(), app_id.to_owned(), child.stdout.take());
        thread::spawn(move || {
            let Some(stdout) = stdout else { return };
            let mut lines = BufReader::new(stdout);
            let mut buf = Vec::new();
            let out = std::io::stdout();
            while matches!(lines.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
                if is_crash(&line, &app_id) {
                    crashed.store(true, Ordering::SeqCst);
                }
                let mut out = out.lock();
                if writeln!(out, "{line}").is_err() {
                    break;
                }
                buf.clear();
            }
        })
    };
    let mut last_check = Instant::now();
    let outcome = loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break Outcome::Stopped;
        }
        thread::sleep(Duration::from_millis(100));
        if last_check.elapsed() >= Duration::from_secs(1) {
            last_check = Instant::now();
            if !adb.pids(serial, app_id).contains(&pid) {
                // Let logcat print the last lines before it is cut off.
                thread::sleep(Duration::from_millis(500));
                break if crashed.load(Ordering::SeqCst) { Outcome::Crashed } else { Outcome::Exited };
            }
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    match outcome {
        Outcome::Stopped => {
            let _ = adb.capture(Some(serial), &["shell", "am", "force-stop", app_id], QUERY_TIMEOUT);
            shell.status("Stopped", app_id.to_owned());
        }
        Outcome::Crashed => shell.error(format!("{app_id} crashed; the log above has the stack trace")),
        Outcome::Exited => shell.status("Finished", format!("{app_id} exited")),
    }
    Ok(outcome)
}

/// What a check of the devices found, for `kargo doctor`.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub adb: Option<(PathBuf, String)>,
    pub devices: Vec<Device>,
    pub avds: Vec<Avd>,
    pub emulator: Option<PathBuf>,
    pub avdmanager: Option<PathBuf>,
    /// How the machine accelerates emulators, or why it cannot.
    pub acceleration: Acceleration,
}

/// Hardware acceleration of the emulator.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Acceleration {
    /// `KVM`, `WHPX`, `Hypervisor.framework`.
    Available(&'static str),
    /// Missing, and what to do about it.
    Missing(String),
    /// Not checked.
    #[default]
    Unknown,
}

/// Looks at what the machine has for running Android apps. Nothing is installed or started.
pub fn report(tb: &Toolbox, shell: &Shell) -> Report {
    let mut report = Report::default();
    if let Ok(adb) = tb.adb() {
        let version = adb
            .capture(None, &["version"], QUERY_TIMEOUT)
            .ok()
            .and_then(|o| o.stdout.lines().find(|l| l.contains("Android Debug Bridge version")).map(|l| l.trim().to_owned()))
            .unwrap_or_else(|| "version unknown".to_owned());
        report.adb = Some((adb.path.clone(), version));
        report.devices = list(tb, shell).unwrap_or_default().into_iter().filter(|d| d.kind != DeviceKind::Avd).collect();
    }
    report.avds = avds(&tb.avd_home());
    report.emulator = tb.emulator().ok();
    report.avdmanager = tb.avdmanager().ok();
    report.acceleration = acceleration();
    report
}

/// Whether this machine can run emulators with hardware acceleration.
pub fn acceleration() -> Acceleration {
    if cfg!(target_os = "linux") {
        if Path::new("/dev/kvm").exists() {
            match std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm") {
                Ok(_) => Acceleration::Available("KVM"),
                Err(_) => Acceleration::Missing(
                    "/dev/kvm exists but this user cannot use it; add yourself to the `kvm` group (`sudo usermod -aG kvm $USER`) and log in again".to_owned(),
                ),
            }
        } else {
            Acceleration::Missing(
                "/dev/kvm is missing; enable virtualization (VT-x / AMD-V) in the firmware and install KVM (`sudo apt install qemu-kvm`)"
                    .to_owned(),
            )
        }
    } else if cfg!(target_os = "macos") {
        // Hypervisor.framework is part of macOS 10.10 and later on Intel and on Apple silicon.
        Acceleration::Available("Hypervisor.framework")
    } else if cfg!(windows) {
        let enabled = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform).State"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        match enabled.as_deref() {
            Some("Enabled") => Acceleration::Available("WHPX"),
            Some(_) => Acceleration::Missing(
                "the Windows Hypervisor Platform is off; turn it on in \"Turn Windows features on or off\" (needs virtualization enabled in the firmware), then restart"
                    .to_owned(),
            ),
            None => Acceleration::Unknown,
        }
    } else {
        Acceleration::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_component_resolve_activity_prints() {
        let id = "com.example.sosial";
        let brief = "priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=false\ncom.example.sosial/.MainActivity\n";
        assert_eq!(resolved_component(brief, id).as_deref(), Some("com.example.sosial/.MainActivity"));
        assert_eq!(
            resolved_component("com.example.sosial/com.example.sosial.ui.Main\r\n", id).as_deref(),
            Some("com.example.sosial/com.example.sosial.ui.Main")
        );
        // Nothing found, an empty answer and the chooser of several activities are not an activity of the app.
        assert_eq!(resolved_component("No activity found\n", id), None);
        assert_eq!(resolved_component("", id), None);
        assert_eq!(resolved_component("priority=0 isDefault=true\nandroid/com.android.internal.app.ResolverActivity\n", id), None);
    }

    #[test]
    fn parses_adb_devices() {
        let text = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\nList of devices attached\n\
                    emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:1\n\
                    R58M123ABC             unauthorized usb:1-1 transport_id:2\n\
                    0123456789ABCDEF       no permissions (user in plugdev group; are your udev rules wrong?); see [http://developer.android.com/tools/device.html] usb:1-2 transport_id:3\n\
                    192.168.1.5:5555       offline product:x model:Pixel_7 device:y transport_id:4\n\n";
        let entries = parse_devices(text);
        let states: Vec<(&str, &str)> = entries.iter().map(|e| (e.serial.as_str(), e.state.as_str())).collect();
        assert_eq!(
            states,
            [
                ("emulator-5554", "device"),
                ("R58M123ABC", "unauthorized"),
                ("0123456789ABCDEF", "no permissions"),
                ("192.168.1.5:5555", "offline")
            ]
        );
        assert_eq!(entries[0].model.as_deref(), Some("sdk gphone64 x86 64"));
        assert_eq!(entries[3].model.as_deref(), Some("Pixel 7"));
        assert!(parse_devices("List of devices attached\n\n").is_empty());
        assert!(parse_devices("").is_empty());
    }

    #[test]
    fn reads_virtual_devices() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("Pixel_8_API_35.avd");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            home.path().join("Pixel_8_API_35.ini"),
            format!("avd.ini.encoding=UTF-8\npath={}\ntarget=android-35\n", dir.display()),
        )
        .unwrap();
        std::fs::write(dir.join("config.ini"), "image.sysdir.1=system-images/android-35/google_apis/x86_64/\nhw.device.name=pixel_8\n")
            .unwrap();
        std::fs::write(home.path().join("Broken.ini"), "target=android-30\n").unwrap();
        let found = avds(home.path());
        assert_eq!(found.len(), 2);
        let pixel = found.iter().find(|a| a.name == "Pixel_8_API_35").unwrap();
        assert_eq!(pixel.api, Some(35));
        assert_eq!(pixel.image.as_deref(), Some("system-images;android-35;google_apis;x86_64"));
        assert_eq!(pixel.hardware.as_deref(), Some("pixel_8"));
        // Without a config the target names the API level.
        assert_eq!(found.iter().find(|a| a.name == "Broken").unwrap().api, Some(30));
    }

    #[test]
    fn names_the_problem_of_a_device() {
        let device = |state: &str| Device {
            id: "R58M123ABC".to_owned(),
            name: "Samsung SM-S918B".to_owned(),
            kind: DeviceKind::Physical,
            state: state.to_owned(),
            api: Some(34),
            avd: None,
        };
        assert!(device("device").problem().is_none() && device("device").is_ready());
        assert!(device("unauthorized").problem().unwrap().contains("Allow USB debugging"));
        assert!(device("offline").problem().unwrap().contains("adb kill-server"));
        assert!(device("no permissions").problem().unwrap().contains("permission"));
        assert!(device("recovery").problem().unwrap().contains("`recovery` state"));
        assert!(!device("unauthorized").is_ready());
    }

    #[test]
    fn renders_the_device_table() {
        let devices = vec![
            Device {
                id: "emulator-5554".into(),
                name: "Pixel_8_API_35".into(),
                kind: DeviceKind::Emulator,
                state: "device".into(),
                api: Some(35),
                avd: Some("Pixel_8_API_35".into()),
            },
            Device { id: "Old_AVD".into(), name: "Old_AVD".into(), kind: DeviceKind::Avd, state: "stopped".into(), api: None, avd: None },
        ];
        let table = render_table(&devices);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[0].starts_with("ID") && lines[0].ends_with("STATE"), "{table}");
        assert!(lines[1].contains("emulator-5554") && lines[1].contains("emulator") && lines[1].ends_with("device"), "{table}");
        assert!(lines[2].contains("avd") && lines[2].contains(" -  ") && lines[2].ends_with("stopped"), "{table}");
        assert_eq!(render_table(&[]), "No devices or virtual devices found.\n");
        let json = serde_json::to_value(&devices[0]).unwrap();
        assert_eq!(json["kind"], "emulator");
        assert_eq!(json["api"], 35);
    }

    #[test]
    fn validates_names_and_crashes() {
        assert!(valid_avd_name("Pixel_8-API.35") && !valid_avd_name("my phone") && !valid_avd_name(""));
        assert_eq!(image_api("system-images;android-37.0;google_apis;x86_64"), Some(37));
        assert_eq!(image_api("platforms;android-36"), Some(36));
        assert!(is_crash("E AndroidRuntime: FATAL EXCEPTION: main", "com.x"));
        assert!(is_crash("F libc    : Fatal signal 11 (SIGSEGV)", "com.x"));
        assert!(!is_crash("I ActivityManager: Start proc", "com.x"));
    }
}
