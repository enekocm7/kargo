//! `kargo self`: updating and removing Kargo itself, and the notice about a newer release.
//!
//! Releases live on GitHub, laid out like this under the releases URL (`KARGO_RELEASES_URL` overrides it, and may be a
//! `file://` directory):
//!
//! ```text
//! latest/download/VERSION                          the newest version, e.g. `0.3.0`
//! download/v0.3.0/SHA256SUMS                       `<sha256>  <file>` for every archive
//! download/v0.3.0/kargo-0.3.0-<target>.tar.gz      `.zip` on Windows; the `kargo` binary in a folder of the same name
//! ```
//!
//! `install.sh`, `install.ps1`, the release workflow and the package templates in `packaging/` use the same names.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use semver::Version;

use kargo_manifest::KARGO_VERSION;
use kargo_toolchain::archive::{self, ArchiveKind};
use kargo_toolchain::download;
use kargo_util::config::Config;

/// Where releases are published.
pub const DEFAULT_RELEASES_URL: &str = "https://github.com/enekocm7/kargo/releases";
/// Overrides [`DEFAULT_RELEASES_URL`].
pub const RELEASES_URL_ENV: &str = "KARGO_RELEASES_URL";
/// Turns off the notice about newer releases.
pub const NO_UPDATE_CHECK_ENV: &str = "KARGO_NO_UPDATE_CHECK";

/// The target this Kargo was built for, such as `x86_64-unknown-linux-musl`.
pub const TARGET: &str = env!("KARGO_TARGET");

/// How often Kargo asks for the newest release, and how often it mentions one.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The check on `kargo --version` must not make the command feel slow.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Markers around the lines the installers add to shell startup files.
const PATH_BLOCK_START: &str = "# >>> kargo >>>";
const PATH_BLOCK_END: &str = "# <<< kargo <<<";

/// The releases URL in effect, without a trailing slash.
pub fn releases_url() -> String {
    let url = std::env::var(RELEASES_URL_ENV).ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_RELEASES_URL.to_owned());
    url.trim().trim_end_matches('/').to_owned()
}

/// The file name of the release archive of `version` for `target`.
pub fn archive_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") { "zip" } else { "tar.gz" };
    format!("kargo-{version}-{target}.{ext}")
}

/// Where the newest version is published.
pub fn latest_url(base: &str) -> String {
    format!("{base}/latest/download/VERSION")
}

/// Where `file` of the release `version` is published.
pub fn release_file_url(base: &str, version: &Version, file: &str) -> String {
    format!("{base}/download/v{version}/{file}")
}

/// Reads a version such as `0.3.0` or `v0.3.0`.
pub fn parse_version(text: &str) -> Result<Version> {
    let text = text.trim();
    Version::parse(text.strip_prefix('v').unwrap_or(text)).with_context(|| format!("`{text}` is not a version like 0.3.0"))
}

/// The SHA-256 `sums` (the `SHA256SUMS` file of a release) lists for `file`.
pub fn checksum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file && !hash.is_empty()).then(|| hash.to_ascii_lowercase())
    })
}

/// A package manager that put the `kargo` binary where it is, and the command that updates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    pub manager: &'static str,
    pub update_command: String,
}

/// Tells whether `exe` belongs to a package manager, judging by where it is. Kargo only replaces binaries the installers
/// or the user put there; the others are updated by whatever installed them.
pub fn detect_manager(exe: &Path, cargo_home: Option<&Path>) -> Option<Managed> {
    let normalize = |p: &Path| p.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let path = normalize(exe);
    let managed = |manager: &'static str, command: &str| Some(Managed { manager, update_command: command.to_owned() });
    if path.contains("/cellar/") || path.contains("/homebrew/") || path.contains("/linuxbrew/") {
        return managed("Homebrew", "brew upgrade kargo");
    }
    if path.contains("/scoop/apps/") || path.contains("/scoop/shims/") {
        return managed("Scoop", "scoop update kargo");
    }
    if path.contains("/winget/") {
        return managed("winget", "winget upgrade kargo");
    }
    if path.contains("/nix/store/") {
        return managed("Nix", "nix profile upgrade kargo");
    }
    if path.contains("/.sdkman/") {
        return managed("SDKMAN!", "sdk upgrade kargo");
    }
    let cargo_bin = cargo_home.map(|h| normalize(&h.join("bin")));
    if path.contains("/.cargo/bin/") || cargo_bin.is_some_and(|bin| path.starts_with(&format!("{bin}/"))) {
        return managed("Cargo", "cargo install --locked --force --git https://github.com/enekocm7/kargo kargo");
    }
    if path.starts_with("/usr/bin/") || path.starts_with("/bin/") || path.starts_with("/usr/lib/") {
        return managed("your system's package manager", "the update command of your distribution (on Arch Linux: `yay -Syu kargo-bin`)");
    }
    None
}

fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to find the running kargo binary")?;
    Ok(exe.canonicalize().map(|p| kargo_util::fs::dunce_canonicalize(&p)).unwrap_or(exe))
}

fn managed_by_package_manager(exe: &Path) -> Option<Managed> {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    detect_manager(exe, cargo_home.as_deref())
}

fn refuse_managed(managed: &Managed, action: &str) -> anyhow::Error {
    anyhow::anyhow!("this kargo was installed by {}, which also {action} it: run `{}` instead", managed.manager, managed.update_command)
}

/// Options of `kargo self update`.
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Install this version instead of the newest one (an older one downgrades).
    pub version: Option<String>,
    /// Only report whether a newer release exists.
    pub check: bool,
}

/// `kargo self update`.
pub fn update(config: &Config, options: &UpdateOptions) -> Result<()> {
    let shell = config.shell();
    let base = releases_url();
    let current = parse_version(KARGO_VERSION)?;
    let exe = current_exe()?;

    let wanted = match &options.version {
        Some(v) => parse_version(v)?,
        None => {
            shell.status("Checking", "for the latest release");
            latest_version(config, &base)?
        }
    };
    if options.check {
        if wanted > current {
            shell.status("Available", format!("kargo {wanted} (this is kargo {current}); run `kargo self update` to install it"));
        } else {
            shell.status("Up to date", format!("kargo {current}"));
        }
        return Ok(());
    }
    if let Some(managed) = managed_by_package_manager(&exe) {
        return Err(refuse_managed(&managed, "updates"));
    }
    if wanted == current || (options.version.is_none() && wanted < current) {
        shell.status("Up to date", format!("kargo {current}"));
        return Ok(());
    }

    let archive = archive_name(&wanted.to_string(), TARGET);
    let sums = download::get_text(config, &release_file_url(&base, &wanted, "SHA256SUMS"))
        .with_context(|| format!("failed to find release v{wanted} of kargo"))?;
    let expected =
        checksum_for(&sums, &archive).with_context(|| format!("release v{wanted} has no {archive} for this platform ({TARGET})"))?;

    let staging = config.home().join("tmp").join(format!("self-{}", std::process::id()));
    kargo_util::fs::remove_dir_all_if_exists(&staging)?;
    fs::create_dir_all(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    let result = fetch_and_replace(config, &base, &wanted, &archive, &expected, &staging, &exe);
    let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
    result?;

    let verb = if wanted < current { "Downgraded" } else { "Updated" };
    shell.status(verb, format!("kargo {current} -> {wanted} ({})", exe.display()));
    Ok(())
}

fn fetch_and_replace(
    config: &Config,
    base: &str,
    version: &Version,
    archive_file: &str,
    expected: &str,
    staging: &Path,
    exe: &Path,
) -> Result<()> {
    let archive_path = staging.join(archive_file);
    download::download(config, &release_file_url(base, version, archive_file), &archive_path, Some(expected))
        .context("the downloaded archive was not installed")?;
    let unpacked = staging.join("unpacked");
    match ArchiveKind::from_name(archive_file)? {
        ArchiveKind::Zip => archive::extract_zip(&archive_path, &unpacked)?,
        ArchiveKind::TarGz => archive::extract_tar_gz(&archive_path, &unpacked)?,
    }
    let binary = find_binary(&unpacked)?;
    replace_executable(exe, &binary)
}

/// The `kargo` binary in an unpacked release: at the top or in the one folder the archive holds.
fn find_binary(dir: &Path) -> Result<PathBuf> {
    let name = if cfg!(windows) { "kargo.exe" } else { "kargo" };
    let mut folders = vec![dir.to_path_buf()];
    if let Some(child) = archive::single_child_dir(dir)? {
        folders.push(child);
    }
    folders
        .into_iter()
        .map(|f| f.join(name))
        .find(|p| p.is_file())
        .with_context(|| format!("the release archive does not contain `{name}`"))
}

/// The name the running executable is moved to on Windows, where it can be renamed but not overwritten or deleted.
fn stale_name(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

/// Puts `new` where `exe` is. The new file is written next to `exe` first, so the swap is one rename and an interrupted
/// update never leaves a half-written binary.
fn replace_executable(exe: &Path, new: &Path) -> Result<()> {
    let dir = exe.parent().context("the kargo binary has no directory")?;
    let staged = dir.join(format!(".kargo-update-{}", std::process::id()));
    fs::copy(new, &staged)
        .with_context(|| format!("cannot write to {}: run `kargo self update` with the permissions to change it", dir.display()))?;
    let swapped = (|| -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        }
        if cfg!(windows) {
            // A running .exe cannot be overwritten, but it can be renamed; the next kargo to start deletes the old file.
            let stale = stale_name(exe);
            let _ = fs::remove_file(&stale);
            fs::rename(exe, &stale).with_context(|| format!("failed to move {} aside", exe.display()))?;
            if let Err(e) = fs::rename(&staged, exe) {
                let _ = fs::rename(&stale, exe);
                return Err(e).with_context(|| format!("failed to replace {}", exe.display()));
            }
            let _ = fs::remove_file(&stale);
        } else {
            fs::rename(&staged, exe).with_context(|| format!("failed to replace {}", exe.display()))?;
        }
        Ok(())
    })();
    if swapped.is_err() {
        let _ = fs::remove_file(&staged);
    }
    swapped
}

/// Deletes the `kargo.exe.old` that an update on Windows left behind. Best effort, and a no-op elsewhere.
pub fn remove_stale_binary() {
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = fs::remove_file(stale_name(&exe));
    }
}

fn latest_version(config: &Config, base: &str) -> Result<Version> {
    let text = download::get_text(config, &latest_url(base)).context("failed to look up the latest release of kargo")?;
    parse_version(&text).context("the latest release has an unreadable version")
}

/// `kargo self uninstall`: removes the binary and the PATH entries the installers added, and with `purge` everything under
/// `~/.kargo` (downloaded toolchains, the Maven cache, installed tools).
pub fn uninstall(config: &Config, purge: bool) -> Result<()> {
    let shell = config.shell();
    let exe = current_exe()?;
    if let Some(managed) = managed_by_package_manager(&exe) {
        return Err(refuse_managed(&managed, "removes"));
    }
    let home = config.home().to_path_buf();
    let bin_dir = config.bin_dir();
    if purge {
        // KARGO_HOME is the user's to set; never delete a directory that is plainly not a Kargo home.
        let user_home = dirs::home_dir();
        if home.parent().is_none() || user_home.as_deref() == Some(home.as_path()) {
            bail!("refusing to delete {}: KARGO_HOME must be a directory of its own, such as ~/.kargo", home.display());
        }
        kargo_compiler::daemon::stop_all(&home);
    }

    shell.status("Removing", exe.display());
    remove_executable(&exe)?;

    for path in remove_path_entries(&bin_dir, dirs::home_dir().as_deref()) {
        shell.status("Cleaned", path);
    }
    if purge {
        shell.status("Removing", home.display());
        remove_tree(&home)?;
    } else {
        shell.note(format!("kept {} (toolchains, caches and installed tools); delete it to free the space", home.display()));
    }
    Ok(())
}

fn remove_executable(exe: &Path) -> Result<()> {
    if !cfg!(windows) {
        return fs::remove_file(exe).with_context(|| format!("failed to remove {}", exe.display()));
    }
    let stale = stale_name(exe);
    let _ = fs::remove_file(&stale);
    fs::rename(exe, &stale).with_context(|| format!("failed to remove {}", exe.display()))?;
    if fs::remove_file(&stale).is_err() {
        // The process still runs from it; a helper deletes it once this one exits.
        schedule_removal(&stale);
    }
    Ok(())
}

fn remove_tree(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) if cfg!(windows) => {
            // The running binary may sit inside; a helper finishes the job once this process exits.
            schedule_removal(dir);
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", dir.display())),
    }
}

/// Windows: deletes `path` a few seconds from now, after this process has exited.
#[cfg(windows)]
fn schedule_removal(path: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let target = path.display();
    let command = format!("ping -n 4 127.0.0.1 >nul & if exist \"{target}\\*\" (rmdir /s /q \"{target}\") else (del /f /q \"{target}\")");
    let _ = std::process::Command::new("cmd").arg("/C").raw_arg(format!("\"{command}\"")).creation_flags(CREATE_NO_WINDOW).spawn();
}

#[cfg(not(windows))]
fn schedule_removal(_path: &Path) {}

/// Removes what the installers added to `PATH` for `bin_dir`, and returns what it changed.
fn remove_path_entries(bin_dir: &Path, home: Option<&Path>) -> Vec<String> {
    let mut changed = Vec::new();
    if cfg!(windows) {
        if remove_from_windows_path(bin_dir) {
            changed.push("the user PATH".to_owned());
        }
        return changed;
    }
    let Some(home) = home else { return changed };
    let mut mentions = vec![bin_dir.display().to_string()];
    if bin_dir == home.join(".kargo").join("bin") {
        mentions.push("$HOME/.kargo/bin".to_owned());
    }
    let fish_config =
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from).unwrap_or_else(|| home.to_path_buf());
    let candidates = [
        home.join(".profile"),
        home.join(".bash_profile"),
        home.join(".bashrc"),
        zdotdir.join(".zshenv"),
        zdotdir.join(".zshrc"),
        fish_config.join("fish/conf.d/kargo.fish"),
        fish_config.join("fish/config.fish"),
    ];
    for path in candidates {
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let Some(stripped) = strip_path_block(&text, &mentions) else { continue };
        let written =
            if stripped.trim().is_empty() && path.ends_with("kargo.fish") { fs::remove_file(&path) } else { fs::write(&path, stripped) };
        if written.is_ok() {
            changed.push(path.display().to_string());
        }
    }
    changed
}

/// `text` without the block the installer added (between the `kargo` markers), or `None` when there is no such block that
/// mentions one of `mentions`. A block for another Kargo home stays.
pub fn strip_path_block(text: &str, mentions: &[String]) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut kept = Vec::new();
    let mut found = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == PATH_BLOCK_START
            && let Some(len) = lines[i..].iter().position(|l| l.trim() == PATH_BLOCK_END)
        {
            let block = &lines[i..=i + len];
            if block.iter().any(|l| mentions.iter().any(|m| l.contains(m.as_str()))) {
                found = true;
                i += len + 1;
                // The blank line the installer put before the block goes too.
                if kept.last().is_some_and(|l: &&str| l.trim().is_empty()) {
                    kept.pop();
                }
                continue;
            }
        }
        kept.push(lines[i]);
        i += 1;
    }
    found.then(|| kept.concat())
}

/// `path` (a `;`-separated `PATH`) without the entries that name `dir`, or `None` when it has none. Entries compare without
/// case and without a trailing backslash, and empty ones are dropped.
pub fn path_without(path: &str, dir: &str) -> Option<String> {
    let norm = |entry: &str| entry.trim().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let target = norm(dir);
    let entries: Vec<&str> = path.split(';').filter(|e| !e.trim().is_empty()).collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|e| norm(e) != target).collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

/// Windows: takes `bin_dir` out of the user's `PATH`. Returns whether it was there.
fn remove_from_windows_path(bin_dir: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let powershell = |script: &str, new_path: Option<&str>| {
        let mut cmd = std::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        if let Some(value) = new_path {
            cmd.env("KARGO_NEW_PATH", value);
        }
        cmd.output().ok().filter(|o| o.status.success())
    };
    let Some(current) = powershell("[Environment]::GetEnvironmentVariable('Path', 'User')", None) else { return false };
    let current = String::from_utf8_lossy(&current.stdout).trim().to_owned();
    let Some(updated) = path_without(&current, &bin_dir.to_string_lossy()) else { return false };
    powershell("[Environment]::SetEnvironmentVariable('Path', $env:KARGO_NEW_PATH, 'User')", Some(&updated)).is_some()
}

/// The note to print after `kargo --version` when a newer release exists, at most once a day. Never fails and never waits
/// long: offline, in CI, without a terminal or with `KARGO_NO_UPDATE_CHECK` set, it does nothing.
pub fn update_notice(config: &Config) -> Option<String> {
    if config.offline() || std::env::var_os(NO_UPDATE_CHECK_ENV).is_some_and(|v| !v.is_empty()) || std::env::var_os("CI").is_some() {
        return None;
    }
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let base = releases_url();
    let command = current_exe().ok().and_then(|exe| managed_by_package_manager(&exe)).map(|m| m.update_command);
    notice(config.home(), SystemTime::now(), command.as_deref(), || download::get_text_within(&latest_url(&base), CHECK_TIMEOUT))
}

/// [`update_notice`] with the clock and the lookup of the newest version passed in.
fn notice(home: &Path, now: SystemTime, command: Option<&str>, fetch: impl FnOnce() -> Result<String>) -> Option<String> {
    let path = home.join("update-check.json");
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let now = secs(now);
    let state: serde_json::Value = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let number = |key: &str| state[key].as_u64().unwrap_or(0);
    let (mut checked, mut notified) = (number("checked"), number("notified"));
    let mut latest = state["latest"].as_str().map(str::to_owned);
    let interval = CHECK_INTERVAL.as_secs();

    let mut changed = false;
    if now.saturating_sub(checked) >= interval {
        if let Ok(text) = fetch()
            && let Ok(version) = parse_version(&text)
        {
            latest = Some(version.to_string());
        }
        // A failed lookup is not retried before the next day either: a flaky network must not slow every `kargo --version`.
        checked = now;
        changed = true;
    }
    let current = parse_version(KARGO_VERSION).ok()?;
    let newer = latest.as_deref().and_then(|v| parse_version(v).ok()).filter(|v| *v > current);
    let message = newer.filter(|_| now.saturating_sub(notified) >= interval).map(|version| {
        notified = now;
        changed = true;
        let how = command.map(str::to_owned).unwrap_or_else(|| "kargo self update".to_owned());
        format!("update available: kargo {version} (this is kargo {current}); run `{how}`")
    });
    if changed {
        let state = serde_json::json!({ "checked": checked, "notified": notified, "latest": latest });
        let _ = fs::create_dir_all(home);
        let _ = fs::write(&path, state.to_string());
    }
    message
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use super::*;

    #[test]
    fn archives_are_named_by_version_and_target() {
        assert_eq!(archive_name("0.3.0", "x86_64-unknown-linux-gnu"), "kargo-0.3.0-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(archive_name("0.3.0", "aarch64-apple-darwin"), "kargo-0.3.0-aarch64-apple-darwin.tar.gz");
        assert_eq!(archive_name("0.3.0", "x86_64-pc-windows-msvc"), "kargo-0.3.0-x86_64-pc-windows-msvc.zip");
        assert_eq!(archive_name("0.3.0", "aarch64-pc-windows-msvc"), "kargo-0.3.0-aarch64-pc-windows-msvc.zip");
    }

    #[test]
    fn release_urls() {
        let v = parse_version("v0.3.0").unwrap();
        assert_eq!(latest_url("https://h/releases"), "https://h/releases/latest/download/VERSION");
        assert_eq!(release_file_url("https://h/releases", &v, "SHA256SUMS"), "https://h/releases/download/v0.3.0/SHA256SUMS");
        assert!(parse_version("latest").is_err());
        assert_eq!(parse_version(" 0.3.0\n").unwrap(), v);
    }

    #[test]
    fn finds_checksums() {
        let sums = "AAAA  kargo-0.3.0-a.tar.gz\nbbbb *kargo-0.3.0-b.zip\n\ncccc  other\n";
        assert_eq!(checksum_for(sums, "kargo-0.3.0-a.tar.gz").as_deref(), Some("aaaa"));
        assert_eq!(checksum_for(sums, "kargo-0.3.0-b.zip").as_deref(), Some("bbbb"));
        assert_eq!(checksum_for(sums, "kargo-0.3.0-c.zip"), None);
    }

    #[test]
    fn recognizes_package_manager_installs() {
        let manager = |p: &str| detect_manager(Path::new(p), None).map(|m| m.manager);
        assert_eq!(manager("/opt/homebrew/Cellar/kargo/0.3.0/bin/kargo"), Some("Homebrew"));
        assert_eq!(manager("/home/linuxbrew/.linuxbrew/Cellar/kargo/0.3.0/bin/kargo"), Some("Homebrew"));
        assert_eq!(manager("C:\\Users\\me\\scoop\\apps\\kargo\\current\\kargo.exe"), Some("Scoop"));
        assert_eq!(manager("C:\\Users\\me\\AppData\\Local\\Microsoft\\WinGet\\Packages\\kargo\\kargo.exe"), Some("winget"));
        assert_eq!(manager("/nix/store/abc-kargo-0.3.0/bin/kargo"), Some("Nix"));
        assert_eq!(manager("/home/me/.sdkman/candidates/kargo/current/bin/kargo"), Some("SDKMAN!"));
        assert_eq!(manager("/home/me/.cargo/bin/kargo"), Some("Cargo"));
        assert_eq!(manager("/usr/bin/kargo"), Some("your system's package manager"));
        assert_eq!(manager("/home/me/.kargo/bin/kargo"), None);
        assert_eq!(manager("/usr/local/bin/kargo"), None);
        assert_eq!(detect_manager(Path::new("/data/cargo/bin/kargo"), Some(Path::new("/data/cargo"))).map(|m| m.manager), Some("Cargo"));
    }

    #[test]
    fn strips_the_installers_path_block() {
        let mentions = vec!["$HOME/.kargo/bin".to_owned()];
        let rc = "export A=1\n\n# >>> kargo >>>\nexport PATH=\"$HOME/.kargo/bin:$PATH\"\n# <<< kargo <<<\nexport B=2\n";
        assert_eq!(strip_path_block(rc, &mentions).unwrap(), "export A=1\nexport B=2\n");
        assert_eq!(strip_path_block("export A=1\n", &mentions), None);
        // A block for another Kargo home stays.
        let other = "# >>> kargo >>>\nexport PATH=\"/opt/k/bin:$PATH\"\n# <<< kargo <<<\n";
        assert_eq!(strip_path_block(other, &mentions), None);
        assert_eq!(strip_path_block(other, &["/opt/k/bin".to_owned()]).unwrap(), "");
    }

    #[test]
    fn takes_a_directory_out_of_the_windows_path() {
        let dir = r"C:\Users\me\.kargo\bin";
        let path = r"C:\Windows;c:\users\ME\.kargo\bin\;;D:\tools";
        assert_eq!(path_without(path, dir).as_deref(), Some(r"C:\Windows;D:\tools"));
        assert_eq!(path_without(r"C:\Windows;D:\tools", dir), None);
        assert_eq!(path_without(dir, dir).as_deref(), Some(""));
        // Only whole entries match.
        assert_eq!(path_without(r"C:\Users\me\.kargo\bin2", dir), None);
    }

    #[test]
    fn replaces_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("kargo");
        let new = dir.path().join("new");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();
        replace_executable(&exe, &new).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    }

    fn tomorrow(now: SystemTime) -> SystemTime {
        now + CHECK_INTERVAL + Duration::from_secs(1)
    }

    #[test]
    fn mentions_a_newer_release_at_most_once_a_day() {
        let home = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        // Nothing newer: no notice.
        assert_eq!(notice(home.path(), now, None, || Ok("0.0.1\n".into())), None);
        // A newer release the next day.
        let first = notice(home.path(), tomorrow(now), None, || Ok("99.0.0\n".into())).unwrap();
        assert!(first.contains("kargo 99.0.0") && first.contains("kargo self update"), "{first}");
        // The same day: neither a lookup nor a second notice.
        let again = notice(home.path(), tomorrow(now) + Duration::from_secs(60), None, || panic!("asked twice in a day"));
        assert_eq!(again, None);
        // The day after, from what is remembered when the lookup fails.
        let later = tomorrow(tomorrow(now));
        let repeated = notice(home.path(), later, Some("brew upgrade kargo"), || bail!("offline")).unwrap();
        assert!(repeated.contains("`brew upgrade kargo`"), "{repeated}");
    }

    #[test]
    fn a_failed_lookup_is_not_repeated_the_same_day() {
        let home = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert_eq!(notice(home.path(), now, None, || bail!("offline")), None);
        assert_eq!(notice(home.path(), now + Duration::from_secs(5), None, || panic!("retried")), None);
    }
}
