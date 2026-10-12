//! Web browsers: finds Chrome, Edge, Chromium, Brave or Firefox to run Kotlin/Wasm
//! tests (`environment = "browser"`) without a window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use kargo_util::process::find_on_path;

/// Environment variable naming the browser to use: an executable path, or a command on `PATH`.
pub const BROWSER_ENV: &str = "KARGO_BROWSER";
/// Environment variable with extra command-line arguments for the headless browser
/// (e.g. `--no-sandbox` in a container), split on whitespace.
pub const BROWSER_ARGS_ENV: &str = "KARGO_BROWSER_ARGS";
/// Environment variable with the number of seconds a browser may stay silent before its tests count as hung.
pub const BROWSER_TIMEOUT_ENV: &str = "KARGO_BROWSER_TIMEOUT";

/// Which command line a browser understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserFamily {
    /// Chrome, Edge, Chromium, Brave and the like.
    Chromium,
    Firefox,
}

/// A browser that can run a page without a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    pub exe: PathBuf,
    pub family: BrowserFamily,
}

/// Commands looked up on `PATH`, most preferred first.
const COMMANDS: [&str; 11] = [
    "google-chrome",
    "google-chrome-stable",
    "chrome",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "microsoft-edge-stable",
    "msedge",
    "brave-browser",
    "brave",
    "firefox",
];

/// Preferences for the throwaway Firefox profile: let the test page close its window (which is
/// how the browser is asked to quit), and keep first-run, telemetry and default-browser prompts out of the way.
const FIREFOX_PREFS: &str = r#"user_pref("dom.allow_scripts_to_close_windows", true);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
"#;

/// Finds a browser: `KARGO_BROWSER`, else the first installed one of Chrome, Chromium, Edge, Brave and Firefox.
pub fn browser() -> Result<Browser> {
    if let Some(name) = std::env::var_os(BROWSER_ENV).filter(|b| !b.is_empty()) {
        let given = PathBuf::from(&name);
        let exe = if given.is_file() { Some(given.clone()) } else { name.to_str().and_then(find_on_path) };
        let exe = exe.with_context(|| {
            format!("{BROWSER_ENV} is `{}`, which is neither an executable file nor a command on PATH", given.display())
        })?;
        return Ok(Browser::new(exe));
    }
    match installed_browsers().into_iter().next() {
        Some(browser) => Ok(browser),
        None => bail!(
            "no browser found to run the Wasm tests; install Chrome, Edge, Chromium, Brave or Firefox, or set {BROWSER_ENV} to the path of one"
        ),
    }
}

/// Every browser found on `PATH` or in its usual install location, most preferred first.
pub fn installed_browsers() -> Vec<Browser> {
    let on_path = COMMANDS.iter().filter_map(|name| find_on_path(name));
    let mut found: Vec<PathBuf> = Vec::new();
    for exe in on_path.chain(install_locations().into_iter().filter(|p| p.is_file())) {
        // `chrome` and `google-chrome` may be one program.
        let same = |other: &PathBuf| other == &exe || matches!((other.canonicalize(), exe.canonicalize()), (Ok(a), Ok(b)) if a == b);
        if !found.iter().any(same) {
            found.push(exe);
        }
    }
    found.into_iter().map(Browser::new).collect()
}

/// Where each platform installs its browsers, most preferred first.
fn install_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        let roots: Vec<PathBuf> =
            ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
        for relative in [
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
            r"Chromium\Application\chrome.exe",
            r"BraveSoftware\Brave-Browser\Application\brave.exe",
            r"Mozilla Firefox\firefox.exe",
        ] {
            out.extend(roots.iter().map(|root| root.join(relative)));
        }
    } else if cfg!(target_os = "macos") {
        let mut apps = vec![PathBuf::from("/Applications")];
        apps.extend(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications")));
        for relative in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Chromium.app/Contents/MacOS/Chromium",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
            "Firefox.app/Contents/MacOS/firefox",
        ] {
            out.extend(apps.iter().map(|dir| dir.join(relative)));
        }
    }
    out
}

fn family_of(exe: &Path) -> BrowserFamily {
    let name = exe.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.contains("firefox") { BrowserFamily::Firefox } else { BrowserFamily::Chromium }
}

impl Browser {
    pub fn new(exe: PathBuf) -> Self {
        let family = family_of(&exe);
        Self { exe, family }
    }

    /// Creates the browser's profile directory, with the settings the tests need.
    pub fn prepare_profile(&self, profile: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(profile)?;
        match self.family {
            BrowserFamily::Chromium => Ok(()),
            BrowserFamily::Firefox => std::fs::write(profile.join("user.js"), FIREFOX_PREFS),
        }
    }

    /// Whether the process Kargo starts may not be the browser itself, so that stopping it
    /// does not stop the browser: Firefox applies a pending update and starts again, and
    /// distributions wrap it in launchers. Such a browser is asked to close its window instead.
    pub fn needs_to_be_asked_to_quit(&self) -> bool {
        self.family == BrowserFamily::Firefox
    }

    /// The command that opens `url` without a window, with its state in `profile` so a
    /// browser that is already running on this machine is left alone.
    pub fn headless_command(&self, url: &str, profile: &Path) -> Command {
        let mut cmd = Command::new(&self.exe);
        match self.family {
            BrowserFamily::Chromium => {
                cmd.args([
                    "--headless=new",
                    "--disable-gpu",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-extensions",
                    "--disable-background-networking",
                    "--disable-component-update",
                    "--disable-sync",
                    "--mute-audio",
                ])
                .arg(format!("--user-data-dir={}", profile.display()));
            }
            BrowserFamily::Firefox => {
                cmd.args(["-headless", "-no-remote", "-profile"]).arg(profile);
            }
        }
        if let Ok(extra) = std::env::var(BROWSER_ARGS_ENV) {
            cmd.args(extra.split_whitespace());
        }
        cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(browser: &str) -> Vec<String> {
        let command = Browser::new(browser.into()).headless_command("http://127.0.0.1:1/", Path::new("profile"));
        command.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn family_follows_the_executable_name() {
        assert_eq!(Browser::new("/usr/bin/firefox".into()).family, BrowserFamily::Firefox);
        assert_eq!(Browser::new(r"C:\Program Files\Mozilla Firefox\firefox.exe".into()).family, BrowserFamily::Firefox);
        assert_eq!(Browser::new("/usr/bin/google-chrome".into()).family, BrowserFamily::Chromium);
        assert_eq!(Browser::new(r"C:\Edge\msedge.exe".into()).family, BrowserFamily::Chromium);
        assert!(Browser::new("firefox".into()).needs_to_be_asked_to_quit());
        assert!(!Browser::new("chrome".into()).needs_to_be_asked_to_quit());
    }

    #[test]
    fn headless_command_lines() {
        let chrome = args("chrome");
        assert!(chrome.contains(&"--headless=new".to_owned()));
        assert!(chrome.contains(&"--user-data-dir=profile".to_owned()));
        assert_eq!(chrome.last().unwrap(), "http://127.0.0.1:1/", "the page comes last");
        let firefox = args("firefox");
        assert_eq!(firefox[..4], ["-headless", "-no-remote", "-profile", "profile"]);
        assert_eq!(firefox.last().unwrap(), "http://127.0.0.1:1/");
    }

    #[test]
    fn firefox_profiles_get_their_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let firefox = dir.path().join("firefox-profile");
        Browser::new("firefox".into()).prepare_profile(&firefox).unwrap();
        let prefs = std::fs::read_to_string(firefox.join("user.js")).unwrap();
        assert!(prefs.contains("dom.allow_scripts_to_close_windows"), "{prefs}");
        let chrome = dir.path().join("chrome-profile");
        Browser::new("chrome".into()).prepare_profile(&chrome).unwrap();
        assert!(chrome.is_dir() && !chrome.join("user.js").exists());
    }

    #[test]
    fn installed_browsers_are_listed_once() {
        let browsers = installed_browsers();
        let mut exes: Vec<PathBuf> = browsers.iter().map(|b| b.exe.canonicalize().unwrap_or_else(|_| b.exe.clone())).collect();
        exes.sort();
        exes.dedup();
        assert_eq!(exes.len(), browsers.len(), "{browsers:?}");
    }

    #[test]
    fn known_install_locations_are_per_platform() {
        let paths = install_locations();
        if cfg!(windows) || cfg!(target_os = "macos") {
            assert!(paths.iter().any(|p| p.to_string_lossy().contains("Chrome")));
            assert!(paths.iter().any(|p| p.to_string_lossy().contains("Edge")));
            assert!(paths.iter().any(|p| p.to_string_lossy().to_lowercase().contains("firefox")));
        } else {
            assert!(paths.is_empty(), "other systems put browsers on PATH");
        }
    }
}
