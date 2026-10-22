//! Kargo-managed JDKs: Temurin builds downloaded on demand into
//! `~/.kargo/toolchains/jdk/<release>`, for a pinned `[toolchain] jdk` and for projects on the
//! system JDK when the machine has none.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use kargo_manifest::{JdkRequest, release_major};
use kargo_maven::Version;
use kargo_util::config::Config;

use crate::adoptium::{self, JdkPackage};
use crate::archive;
use crate::jdk::Jdk;
use crate::{AUTO_INSTALL_OFF, ensure_auto_install, install_lock};

/// The JDK feature versions with long-term support, oldest first, that Kargo picks from when a
/// project on the system JDK runs on a machine without one.
pub const LTS_JDKS: &[u32] = &[17, 21, 25];

/// The JDK Kargo installs for such a project unless it needs another one.
pub const DEFAULT_MANAGED_JDK: u32 = 21;

/// What a project needs of a JDK that Kargo chooses for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JdkNeeds {
    /// The oldest JDK that builds the project (Kargo's minimum, or the `jvm-target`).
    pub min: u32,
    /// The newest JDK its Kotlin compiler runs on, when there is a limit.
    pub max: Option<u32>,
}

impl Default for JdkNeeds {
    fn default() -> Self {
        Self { min: 17, max: None }
    }
}

impl JdkNeeds {
    fn fits(&self, major: u32) -> bool {
        major >= self.min && self.max.is_none_or(|max| major <= max)
    }

    /// The JDK to install: the first LTS from [`DEFAULT_MANAGED_JDK`] on that fits, else the
    /// oldest that is new enough (a `jvm-target` beyond the newest LTS).
    pub fn choose(&self) -> u32 {
        let floor = self.min.max(DEFAULT_MANAGED_JDK);
        LTS_JDKS.iter().copied().find(|&m| m >= floor && self.fits(m)).unwrap_or(floor)
    }
}

/// Written into each installation so it can be listed without probing `java`.
const METADATA_FILE: &str = "kargo-jdk.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Metadata {
    release: String,
    major: u32,
    vendor: String,
    /// JDK home relative to the installation directory (`Contents/Home` on macOS).
    home: String,
}

/// An installed, Kargo-managed JDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedJdk {
    pub release: String,
    pub major: u32,
    pub vendor: String,
    /// Installation directory (removed as a whole).
    pub dir: PathBuf,
    /// `JAVA_HOME` of this JDK.
    pub home: PathBuf,
}

impl ManagedJdk {
    pub fn jdk(&self) -> Jdk {
        Jdk { home: self.home.clone(), version: self.major }
    }

    fn sort_key(&self) -> (u32, Version) {
        (self.major, release_version(&self.release))
    }
}

fn release_version(release: &str) -> Version {
    Version::parse(release.trim_start_matches("jdk-").trim_start_matches("jdk"))
}

/// Directory names are release names; reject anything that could escape.
fn install_dir(config: &Config, release: &str) -> Result<PathBuf> {
    if release.is_empty() || release.contains(['/', '\\', ':']) || release.starts_with('.') {
        bail!("invalid JDK release name `{release}`");
    }
    Ok(config.jdks_dir().join(release))
}

/// Finds the JDK home (the directory containing `bin/java`) inside an installation.
fn find_home(dir: &Path) -> Option<PathBuf> {
    let java = if cfg!(windows) { "java.exe" } else { "java" };
    let candidates = [dir.to_path_buf(), dir.join("Contents").join("Home")];
    candidates.into_iter().find(|c| c.join("bin").join(java).is_file())
}

/// Lists installed JDKs, oldest first.
pub fn installed(config: &Config) -> Result<Vec<ManagedJdk>> {
    let root = config.jdks_dir();
    let Ok(entries) = std::fs::read_dir(&root) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(METADATA_FILE)) else { continue };
        let Ok(meta) = serde_json::from_str::<Metadata>(&text) else { continue };
        let home = if meta.home.is_empty() { dir.clone() } else { dir.join(&meta.home) };
        if !home.join("bin").is_dir() {
            continue;
        }
        out.push(ManagedJdk { release: meta.release, major: meta.major, vendor: meta.vendor, dir, home });
    }
    out.sort_by_key(ManagedJdk::sort_key);
    Ok(out)
}

/// The best installed JDK matching `request`, if any.
pub fn find_installed(config: &Config, request: &JdkRequest) -> Result<Option<ManagedJdk>> {
    let all = installed(config)?;
    Ok(match request {
        JdkRequest::System => None,
        JdkRequest::Major(m) => all.into_iter().filter(|j| j.major == *m).max_by_key(ManagedJdk::sort_key),
        JdkRequest::Release(r) => all.into_iter().find(|j| &j.release == r),
    })
}

/// Downloads, verifies and installs `package` (no-op if already installed).
pub fn install_package(config: &Config, package: &JdkPackage) -> Result<ManagedJdk> {
    let dir = install_dir(config, &package.release)?;
    let _lock = install_lock(config, &package.release)?;
    if let Some(existing) = installed(config)?.into_iter().find(|j| j.release == package.release) {
        return Ok(existing);
    }
    let downloads = config.toolchains_dir().join("downloads");
    let archive_path = downloads.join(format!("{}.{}", package.release.replace('+', "_"), package.archive.extension()));
    let cached = archive_path.is_file() && crate::download::verify(&archive_path, &package.sha256).is_ok();
    if !cached {
        crate::download::download(config, &package.url, &archive_path, Some(&package.sha256))?;
    }
    config.shell().status("Installing", format!("JDK {} (Temurin)", package.release));
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    archive::install(&archive_path, package.archive, &dir)?;
    let _ = std::fs::remove_file(&archive_path);

    let home = find_home(&dir).with_context(|| format!("the downloaded JDK {} has no bin/java", package.release))?;
    let rel = home.strip_prefix(&dir).unwrap_or(Path::new("")).to_string_lossy().replace('\\', "/");
    let meta = Metadata { release: package.release.clone(), major: package.major, vendor: "temurin".to_owned(), home: rel };
    kargo_util::fs::write_string_atomic(&dir.join(METADATA_FILE), &serde_json::to_string_pretty(&meta)?)?;
    Ok(ManagedJdk { release: meta.release, major: meta.major, vendor: meta.vendor, dir, home })
}

/// Installs the JDK described by `request`. For a major version this is the
/// newest available release, even if an older one is installed.
pub fn install(config: &Config, request: &JdkRequest) -> Result<ManagedJdk> {
    let package = match request {
        JdkRequest::System => bail!("`system` refers to the JDK on JAVA_HOME/PATH and cannot be installed"),
        JdkRequest::Major(m) => adoptium::latest(config, *m)?,
        JdkRequest::Release(r) => adoptium::release(config, r)?,
    };
    install_package(config, &package)
}

/// Returns the JDK to use for `request`, installing it if necessary (see [`resolve_for`]).
pub fn resolve(config: &Config, request: &JdkRequest) -> Result<Jdk> {
    resolve_for(config, request, JdkNeeds::default())
}

/// Returns the JDK to use for `request`, installing it if necessary. For `system`, that is the
/// JDK on `JAVA_HOME` or `PATH`; without one, a managed JDK that fits `needs`, installed if
/// there is none.
pub fn resolve_for(config: &Config, request: &JdkRequest, needs: JdkNeeds) -> Result<Jdk> {
    match request {
        JdkRequest::System => match Jdk::find()? {
            Some(jdk) => Ok(jdk),
            None => managed_fallback(config, needs),
        },
        other => {
            if let Some(found) = find_installed(config, other)? {
                return Ok(found.jdk());
            }
            let install_command = format!("kargo toolchain install jdk {other}");
            if config.offline() {
                bail!("JDK {other} is not installed and Kargo is offline; run `{install_command}` while online");
            }
            ensure_auto_install(config, &format!("JDK {other}"), &install_command)?;
            Ok(install(config, other)?.jdk())
        }
    }
}

/// The installed managed JDK a project on the system JDK uses when the machine has none: the
/// newest that fits `needs`.
pub fn find_fallback(config: &Config, needs: JdkNeeds) -> Result<Option<ManagedJdk>> {
    Ok(installed(config)?.into_iter().filter(|j| needs.fits(j.major)).max_by_key(ManagedJdk::sort_key))
}

/// The JDK of a project on the system JDK when the machine has none: [`find_fallback`], else a
/// new install of the one [`JdkNeeds::choose`] picks.
fn managed_fallback(config: &Config, needs: JdkNeeds) -> Result<Jdk> {
    if let Some(found) = find_fallback(config, needs)? {
        return Ok(found.jdk());
    }
    let major = needs.choose();
    let none = "no Java installation found on JAVA_HOME or PATH";
    let install_command = format!("kargo toolchain install jdk {major}");
    if config.offline() {
        bail!(
            "{none}, and Kargo is offline so it can't download JDK {major}; run `{install_command}` while online, or install a JDK and set JAVA_HOME"
        );
    }
    if config.locked() {
        bail!(
            "{none}, and with --locked Kargo doesn't choose a JDK for the project; run `{install_command}`, \
             set `[toolchain] jdk = \"{major}\"` in kargo.toml, or install a JDK and set JAVA_HOME"
        );
    }
    if !config.auto_install() {
        bail!("{none}, and {AUTO_INSTALL_OFF}; run `{install_command}`, or install a JDK and set JAVA_HOME");
    }
    config
        .shell()
        .note(format!("no JDK on JAVA_HOME or PATH; installing Temurin JDK {major} for Kargo (KARGO_AUTO_INSTALL=0 turns this off)"));
    Ok(install(config, &JdkRequest::Major(major))?.jdk())
}

/// Removes installed JDKs matching `selector` (a major version or release name).
pub fn remove(config: &Config, selector: &str) -> Result<Vec<ManagedJdk>> {
    let request: JdkRequest = selector.parse()?;
    let matching: Vec<ManagedJdk> = installed(config)?
        .into_iter()
        .filter(|j| match &request {
            JdkRequest::System => false,
            JdkRequest::Major(m) => j.major == *m,
            JdkRequest::Release(r) => &j.release == r,
        })
        .collect();
    if matching.is_empty() {
        bail!("no installed JDK matches `{selector}`; see `kargo toolchain list`");
    }
    for jdk in &matching {
        let _lock = install_lock(config, &jdk.release)?;
        kargo_util::fs::remove_dir_all_if_exists(&jdk.dir)?;
    }
    Ok(matching)
}

/// Major version of a release name, for display.
pub fn major_of(release: &str) -> Option<u32> {
    release_major(release)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(dir: &Path) -> Config {
        Config::new(dir.to_path_buf(), Shell::new(Verbosity::Quiet))
    }

    /// Fakes an installed JDK.
    fn fake_install(config: &Config, release: &str, major: u32, mac_layout: bool) -> PathBuf {
        let dir = config.jdks_dir().join(release);
        let home = if mac_layout { dir.join("Contents/Home") } else { dir.clone() };
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }), "").unwrap();
        let meta = Metadata {
            release: release.into(),
            major,
            vendor: "temurin".into(),
            home: if mac_layout { "Contents/Home".into() } else { String::new() },
        };
        std::fs::write(dir.join(METADATA_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        dir
    }

    #[test]
    fn lists_and_selects_installed_jdks() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        assert!(installed(&config).unwrap().is_empty());
        fake_install(&config, "jdk-21.0.2+13", 21, false);
        fake_install(&config, "jdk-21.0.10+7", 21, true);
        fake_install(&config, "jdk-17.0.9+9", 17, false);
        std::fs::create_dir_all(config.jdks_dir().join(".staging-x")).unwrap();
        std::fs::create_dir_all(config.jdks_dir().join("not-a-jdk")).unwrap();

        let all: Vec<String> = installed(&config).unwrap().into_iter().map(|j| j.release).collect();
        assert_eq!(all, ["jdk-17.0.9+9", "jdk-21.0.2+13", "jdk-21.0.10+7"]);

        let newest = find_installed(&config, &JdkRequest::Major(21)).unwrap().unwrap();
        assert_eq!(newest.release, "jdk-21.0.10+7", "numeric, not lexical, ordering");
        assert!(newest.home.ends_with("Home"));
        let exact = find_installed(&config, &"21.0.2+13".parse().unwrap()).unwrap().unwrap();
        assert_eq!(exact.release, "jdk-21.0.2+13");
        assert!(find_installed(&config, &JdkRequest::Major(11)).unwrap().is_none());
        assert!(find_installed(&config, &JdkRequest::System).unwrap().is_none());

        // An installed JDK resolves without network access.
        let offline = config.clone().with_offline(true);
        assert_eq!(resolve(&offline, &JdkRequest::Major(17)).unwrap().version, 17);
        let err = resolve(&offline, &JdkRequest::Major(11)).unwrap_err().to_string();
        assert!(err.contains("offline"), "{err}");
    }

    #[test]
    fn chooses_an_lts_the_project_can_use() {
        let needs = |min, max| JdkNeeds { min, max };
        assert_eq!(JdkNeeds::default().choose(), DEFAULT_MANAGED_JDK);
        // Kotlin before 2.1.10 runs on JDK 24 at most.
        assert_eq!(needs(17, Some(24)).choose(), 21);
        // jvm-target 22..25 needs the next LTS; one beyond the newest LTS gets itself.
        assert_eq!(needs(22, None).choose(), 25);
        assert_eq!(needs(26, None).choose(), 26);
        assert!(needs(17, Some(24)).fits(17) && !needs(17, Some(24)).fits(25) && !needs(17, None).fits(11));
    }

    #[test]
    fn falls_back_to_a_managed_jdk_that_fits() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path()).with_offline(true);
        let needs = JdkNeeds { min: 17, max: Some(24) };
        let err = managed_fallback(&config, needs).unwrap_err().to_string();
        assert!(err.contains("offline") && err.contains("kargo toolchain install jdk 21"), "{err}");
        let online = config.clone().with_offline(false);
        let err = managed_fallback(&online.clone().with_auto_install(false), needs).unwrap_err().to_string();
        assert!(err.contains("KARGO_AUTO_INSTALL=0") && err.contains("kargo toolchain install jdk 21"), "{err}");
        let err = managed_fallback(&online.with_locked(true), needs).unwrap_err().to_string();
        assert!(err.contains("--locked") && err.contains("[toolchain] jdk = \"21\""), "{err}");

        // Installed JDKs are used without a download: the newest that fits.
        fake_install(&config, "jdk-25.0.1+8", 25, false);
        fake_install(&config, "jdk-17.0.9+9", 17, false);
        fake_install(&config, "jdk-21.0.4+7", 21, false);
        assert_eq!(managed_fallback(&config, needs).unwrap().version, 21);
        assert_eq!(managed_fallback(&config, JdkNeeds::default()).unwrap().version, 25);
    }

    #[test]
    fn pinned_jdks_follow_the_auto_install_setting() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path()).with_auto_install(false);
        let err = resolve(&config, &JdkRequest::Major(21)).unwrap_err().to_string();
        assert!(err.contains("KARGO_AUTO_INSTALL=0") && err.contains("kargo toolchain install jdk 21"), "{err}");
    }

    #[test]
    fn removes_by_major_or_release() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        fake_install(&config, "jdk-21.0.2+13", 21, false);
        fake_install(&config, "jdk-21.0.10+7", 21, false);
        fake_install(&config, "jdk-17.0.9+9", 17, false);
        let removed = remove(&config, "jdk-17.0.9+9").unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!removed[0].dir.exists());
        assert_eq!(remove(&config, "21").unwrap().len(), 2);
        assert!(installed(&config).unwrap().is_empty());
        assert!(remove(&config, "21").is_err());
        assert!(remove(&config, "system").is_err());
    }

    #[test]
    fn rejects_bad_release_names_and_system_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        assert!(install_dir(&config, "../evil").is_err());
        assert!(install_dir(&config, "").is_err());
        assert!(install(&config, &JdkRequest::System).is_err());
        assert_eq!(major_of("jdk-25.0.1+8"), Some(25));
    }

    /// Pre-seeds the download cache with a JDK archive for `release`, so no network is needed.
    fn seed_archive(config: &Config, release: &str, major: u32) -> JdkPackage {
        let archive_path = config.toolchains_dir().join("downloads").join(format!("{}.zip", release.replace('+', "_")));
        std::fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        {
            use std::io::Write;
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
            let java = if cfg!(windows) { format!("{release}/bin/java.exe") } else { format!("{release}/bin/java") };
            zip.start_file(java, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"").unwrap();
            zip.finish().unwrap();
        }
        let sha256 = kargo_util::hash::sha256_file(&archive_path).unwrap();
        JdkPackage {
            release: release.into(),
            major,
            url: "https://example.invalid/jdk.zip".into(),
            sha256,
            size: 0,
            archive: archive::ArchiveKind::Zip,
        }
    }

    #[test]
    fn installs_from_a_verified_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path()).with_offline(true);
        let package = seed_archive(&config, "jdk-99.0.1+1", 99);
        let archive_path = config.toolchains_dir().join("downloads").join("jdk-99.0.1_1.zip");
        let jdk = install_package(&config, &package).unwrap();
        assert_eq!(jdk.major, 99);
        assert!(jdk.home.join("bin").is_dir());
        assert!(!archive_path.exists(), "archive removed after install");
        assert_eq!(installed(&config).unwrap().len(), 1);
        // Installing again is a no-op.
        assert_eq!(install_package(&config, &package).unwrap(), jdk);

        // A corrupted cached archive is not trusted (and offline mode prevents a re-download).
        let bad = JdkPackage { release: "jdk-98.0.1+1".into(), major: 98, sha256: "00".into(), ..package };
        std::fs::write(config.toolchains_dir().join("downloads").join("jdk-98.0.1_1.zip"), "junk").unwrap();
        assert!(install_package(&config, &bad).is_err());
    }

    /// Another Kargo process installing the same JDK holds its lock; this process must wait for
    /// it rather than clear the directory the other one is writing.
    #[test]
    fn installs_wait_for_the_toolchain_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path()).with_offline(true);
        let package = seed_archive(&config, "jdk-97.0.1+1", 97);
        let other_process = install_lock(&config, &package.release).unwrap();
        let worker = {
            let (config, package) = (config.clone(), package.clone());
            std::thread::spawn(move || install_package(&config, &package))
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!worker.is_finished(), "the install ran while another process held the lock");
        drop(other_process);
        let jdk = worker.join().unwrap().unwrap();
        assert!(jdk.home.join("bin").is_dir());
        assert_eq!(installed(&config).unwrap().len(), 1);
    }
}
