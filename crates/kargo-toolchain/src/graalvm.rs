//! GraalVM, which builds native executables with `native-image`: found through `GRAALVM_HOME`,
//! `JAVA_HOME` or `PATH`, or downloaded on demand into `~/.kargo/toolchains/graalvm/<distribution>-<version>`.
//!
//! Releases are looked up with the foojay Disco API (<https://api.foojay.io/swagger-ui>), which
//! lists GraalVM Community Edition and Oracle GraalVM builds with their SHA-256 checksums.
//!
//! Also the GraalVM reachability metadata repository: the reflection, resource and proxy
//! configuration of popular libraries, which `native-image` needs and the libraries don't ship.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use kargo_manifest::{GraalvmDistribution, GraalvmRequest};
use kargo_maven::Version;
use kargo_util::config::Config;
use kargo_util::process::find_on_path;

use crate::archive::{self, ArchiveKind};
use crate::install_lock;

pub const DISCO_API: &str = "https://api.foojay.io/disco/v3.0";

/// The pinned release of <https://github.com/oracle/graalvm-reachability-metadata>.
pub const REACHABILITY_METADATA_VERSION: &str = "1.0.20";
pub const REACHABILITY_METADATA_SHA256: &str = "7f03bde2feeac1a8e0e40e45c8d70f5236cace2db9d5f8d0f337002329eb1f23";

/// Written into each installation so it can be listed without probing `java`.
const METADATA_FILE: &str = "kargo-graalvm.json";

/// A GraalVM installation with `native-image`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graalvm {
    pub home: PathBuf,
    /// The Java feature version (`21`, `25`, ...).
    pub version: u32,
}

impl Graalvm {
    pub fn java(&self) -> PathBuf {
        self.home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
    }

    /// `native-image`, a `.cmd` script on Windows.
    pub fn native_image(&self) -> PathBuf {
        native_image_in(&self.home)
    }

    /// Finds the GraalVM of `GRAALVM_HOME`, else a `JAVA_HOME` with `native-image`, else `native-image` on `PATH`.
    pub fn detect() -> Result<Self> {
        for var in ["GRAALVM_HOME", "JAVA_HOME"] {
            if let Some(home) = std::env::var_os(var).filter(|h| !h.is_empty()) {
                let home = PathBuf::from(home);
                if native_image_in(&home).is_file() {
                    return Self::at(&home);
                }
                if var == "GRAALVM_HOME" {
                    bail!("GRAALVM_HOME ({}) has no bin/native-image; point it at a GraalVM installation", home.display());
                }
            }
        }
        let native_image = find_on_path("native-image")
            .context("no GraalVM found: set [toolchain] graalvm = \"25\" in kargo.toml to let Kargo download one, or set GRAALVM_HOME")?;
        let real = kargo_util::fs::dunce_canonicalize(&native_image);
        let home = real.parent().and_then(Path::parent).context("`native-image` on PATH is not inside a GraalVM's bin directory")?;
        Self::at(home)
    }

    /// The GraalVM at `home`, its version read from the `release` file.
    pub fn at(home: &Path) -> Result<Self> {
        let release = std::fs::read_to_string(home.join("release")).ok();
        let version = release.as_deref().and_then(crate::jdk::parse_release);
        let version = match version {
            Some(v) => v,
            None => crate::jdk::Jdk::probe(&home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }))?.version,
        };
        Ok(Self { home: home.to_path_buf(), version })
    }
}

fn native_image_in(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "native-image.cmd" } else { "native-image" })
}

/// A downloadable GraalVM build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraalvmPackage {
    pub distribution: GraalvmDistribution,
    /// The distribution's own version, e.g. `25.0.4.1.1`.
    pub version: String,
    pub major: u32,
    pub url: String,
    pub sha256: String,
    pub archive: ArchiveKind,
}

impl GraalvmPackage {
    fn release(&self) -> String {
        release_name(self.distribution, &self.version)
    }
}

fn release_name(distribution: GraalvmDistribution, version: &str) -> String {
    format!("{}-{version}", distribution.as_str())
}

/// foojay's names for the current OS and CPU, and the archive format GraalVM ships there.
fn platform() -> Result<(&'static str, &'static str, &'static str)> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_for(os: &str, arch: &str) -> Result<(&'static str, &'static str, &'static str)> {
    let (os, archive) = match os {
        "windows" => ("windows", "zip"),
        "linux" => ("linux", "tar.gz"),
        "macos" => ("macos", "tar.gz"),
        other => bail!("GraalVM is not available for {other}; install one yourself and set GRAALVM_HOME"),
    };
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => bail!("GraalVM is not available for the {other} architecture; install one yourself and set GRAALVM_HOME"),
    };
    Ok((os, arch, archive))
}

fn api_distribution(distribution: GraalvmDistribution) -> &'static str {
    match distribution {
        GraalvmDistribution::Community => "graalvm_community",
        GraalvmDistribution::Oracle => "graalvm",
    }
}

#[derive(Debug, Deserialize)]
struct Packages {
    result: Vec<PackageEntry>,
}

#[derive(Debug, Deserialize)]
struct PackageEntry {
    id: String,
    distribution_version: String,
    major_version: u32,
    filename: String,
    #[serde(default)]
    latest_build_available: bool,
}

#[derive(Debug, Deserialize)]
struct PackageInfos {
    result: Vec<PackageInfo>,
}

#[derive(Debug, Deserialize)]
struct PackageInfo {
    direct_download_uri: String,
    checksum: String,
    checksum_type: String,
}

/// The newest entry of a `/packages` response: its id, version and file name.
fn parse_packages(json: &str) -> Result<Option<(String, String, u32, String)>> {
    let packages: Packages = serde_json::from_str(json).context("unexpected response from the foojay Disco API")?;
    let newest = packages
        .result
        .into_iter()
        .max_by_key(|p| (p.latest_build_available, Version::parse(&p.distribution_version)))
        .map(|p| (p.id, p.distribution_version, p.major_version, p.filename));
    Ok(newest)
}

/// The download URL and SHA-256 of an `/ids/{id}` response.
fn parse_info(json: &str) -> Result<(String, String)> {
    let infos: PackageInfos = serde_json::from_str(json).context("unexpected response from the foojay Disco API")?;
    let info = infos.result.into_iter().next().context("the foojay Disco API returned no download for this package")?;
    if !info.checksum_type.eq_ignore_ascii_case("sha256") || info.checksum.is_empty() {
        bail!("the foojay Disco API has no SHA-256 checksum for {}", info.direct_download_uri);
    }
    Ok((info.direct_download_uri, info.checksum))
}

/// The newest GraalVM build of a distribution and Java feature version.
pub fn latest(config: &Config, distribution: GraalvmDistribution, major: u32) -> Result<GraalvmPackage> {
    let (os, arch, archive) = platform()?;
    let url = format!(
        "{DISCO_API}/packages?version={major}&distribution={}&architecture={arch}&archive_type={archive}&operating_system={os}&package_type=jdk&release_status=ga&latest=available",
        api_distribution(distribution)
    );
    let title = distribution.title();
    let body = crate::download::get_text(config, &url).with_context(|| format!("failed to look up {title} {major}"))?;
    let (id, version, found_major, filename) =
        parse_packages(&body)?.with_context(|| format!("no {title} {major} build is available for {os}/{arch}"))?;
    let info = crate::download::get_text(config, &format!("{DISCO_API}/ids/{id}"))
        .with_context(|| format!("failed to look up the download of {title} {version}"))?;
    let (url, sha256) = parse_info(&info)?;
    Ok(GraalvmPackage { distribution, version, major: found_major, url, sha256, archive: ArchiveKind::from_name(&filename)? })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Metadata {
    distribution: String,
    version: String,
    major: u32,
    /// The GraalVM home relative to the installation directory (`Contents/Home` on macOS).
    home: String,
}

/// An installed, Kargo-managed GraalVM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGraalvm {
    pub distribution: GraalvmDistribution,
    pub version: String,
    pub major: u32,
    /// Installation directory (removed as a whole).
    pub dir: PathBuf,
    pub home: PathBuf,
}

impl ManagedGraalvm {
    pub fn graalvm(&self) -> Graalvm {
        Graalvm { home: self.home.clone(), version: self.major }
    }

    /// `community-25.0.4.1.1`: the directory name, and what `kargo toolchain remove graalvm` accepts.
    pub fn release(&self) -> String {
        release_name(self.distribution, &self.version)
    }

    fn sort_key(&self) -> (GraalvmDistribution, u32, Version) {
        (self.distribution, self.major, Version::parse(&self.version))
    }
}

fn parse_distribution(name: &str) -> Option<GraalvmDistribution> {
    match name {
        "community" => Some(GraalvmDistribution::Community),
        "oracle" => Some(GraalvmDistribution::Oracle),
        _ => None,
    }
}

/// Finds the GraalVM home (the directory containing `bin/native-image`) inside an installation.
fn find_home(dir: &Path) -> Option<PathBuf> {
    [dir.to_path_buf(), dir.join("Contents").join("Home")].into_iter().find(|c| native_image_in(c).is_file())
}

/// Lists installed GraalVMs, oldest first.
pub fn installed(config: &Config) -> Result<Vec<ManagedGraalvm>> {
    let Ok(entries) = std::fs::read_dir(config.graalvm_dir()) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(METADATA_FILE)) else { continue };
        let Ok(meta) = serde_json::from_str::<Metadata>(&text) else { continue };
        let Some(distribution) = parse_distribution(&meta.distribution) else { continue };
        let home = if meta.home.is_empty() { dir.clone() } else { dir.join(&meta.home) };
        if !home.join("bin").is_dir() {
            continue;
        }
        out.push(ManagedGraalvm { distribution, version: meta.version, major: meta.major, dir, home });
    }
    out.sort_by_key(ManagedGraalvm::sort_key);
    Ok(out)
}

/// The newest installed GraalVM matching `request`, if any.
pub fn find_installed(config: &Config, request: &GraalvmRequest) -> Result<Option<ManagedGraalvm>> {
    let GraalvmRequest::Major { distribution, major } = request else { return Ok(None) };
    Ok(installed(config)?.into_iter().filter(|g| g.distribution == *distribution && g.major == *major).max_by_key(ManagedGraalvm::sort_key))
}

/// Downloads, verifies and installs `package` (no-op if already installed).
pub fn install_package(config: &Config, package: &GraalvmPackage) -> Result<ManagedGraalvm> {
    let release = package.release();
    if package.version.is_empty() || package.version.contains(['/', '\\', ':']) || package.version.starts_with('.') {
        bail!("invalid GraalVM version `{}`", package.version);
    }
    let dir = config.graalvm_dir().join(&release);
    let _lock = install_lock(config, &format!("graalvm-{release}"))?;
    if let Some(existing) = installed(config)?.into_iter().find(|g| g.release() == release) {
        return Ok(existing);
    }
    let archive_path = config.toolchains_dir().join("downloads").join(format!("graalvm-{release}.{}", package.archive.extension()));
    let cached = archive_path.is_file() && crate::download::verify(&archive_path, &package.sha256).is_ok();
    if !cached {
        crate::download::download(config, &package.url, &archive_path, Some(&package.sha256))?;
    }
    config.shell().status("Installing", format!("{} {}", package.distribution.title(), package.version));
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    archive::install(&archive_path, package.archive, &dir)?;
    let _ = std::fs::remove_file(&archive_path);

    let home = find_home(&dir)
        .with_context(|| format!("the downloaded {} {} has no bin/native-image", package.distribution.title(), package.version))?;
    let rel = home.strip_prefix(&dir).unwrap_or(Path::new("")).to_string_lossy().replace('\\', "/");
    let meta = Metadata {
        distribution: package.distribution.as_str().to_owned(),
        version: package.version.clone(),
        major: package.major,
        home: rel,
    };
    kargo_util::fs::write_string_atomic(&dir.join(METADATA_FILE), &serde_json::to_string_pretty(&meta)?)?;
    Ok(ManagedGraalvm { distribution: package.distribution, version: meta.version, major: meta.major, dir, home })
}

/// Installs the newest GraalVM of `request`, even if an older one is installed. Offline, a
/// matching installed one is the newest Kargo can know of.
pub fn install(config: &Config, request: &GraalvmRequest) -> Result<ManagedGraalvm> {
    if config.offline()
        && let Some(found) = find_installed(config, request)?
    {
        return Ok(found);
    }
    match request {
        GraalvmRequest::System => bail!("`system` refers to the GraalVM on GRAALVM_HOME/JAVA_HOME/PATH and cannot be installed"),
        GraalvmRequest::Major { distribution, major } => install_package(config, &latest(config, *distribution, *major)?),
    }
}

/// Returns the GraalVM to use for `request`, installing it if necessary.
pub fn resolve(config: &Config, request: &GraalvmRequest) -> Result<Graalvm> {
    if *request == GraalvmRequest::System {
        return Graalvm::detect();
    }
    if let Some(found) = find_installed(config, request)? {
        return Ok(found.graalvm());
    }
    if config.offline() {
        bail!("GraalVM {request} is not installed and Kargo is offline; run `kargo toolchain install graalvm {request}` while online");
    }
    crate::ensure_auto_install(config, &format!("GraalVM {request}"), &format!("kargo toolchain install graalvm {request}"))?;
    Ok(install(config, request)?.graalvm())
}

/// Removes installed GraalVMs matching `selector`: a request such as `25` or `oracle-25`, or a
/// release such as `community-25.0.4.1.1`.
pub fn remove(config: &Config, selector: &str) -> Result<Vec<ManagedGraalvm>> {
    let all = installed(config)?;
    let matching: Vec<ManagedGraalvm> = match selector.parse::<GraalvmRequest>() {
        Ok(GraalvmRequest::Major { distribution, major }) => {
            all.into_iter().filter(|g| g.distribution == distribution && g.major == major).collect()
        }
        _ => all.into_iter().filter(|g| g.release() == selector).collect(),
    };
    if matching.is_empty() {
        bail!("no installed GraalVM matches `{selector}`; see `kargo toolchain list`");
    }
    for graalvm in &matching {
        let _lock = install_lock(config, &format!("graalvm-{}", graalvm.release()))?;
        kargo_util::fs::remove_dir_all_if_exists(&graalvm.dir)?;
    }
    Ok(matching)
}

/// The reachability metadata repository, downloaded and extracted on first use into
/// `~/.kargo/toolchains/graalvm-reachability-metadata/<version>`.
pub fn reachability_metadata(config: &Config) -> Result<PathBuf> {
    let version = REACHABILITY_METADATA_VERSION;
    let dir = config.toolchains_dir().join("graalvm-reachability-metadata").join(version);
    let done = dir.join(".kargo-complete");
    if done.is_file() {
        return Ok(dir);
    }
    let _lock = install_lock(config, &format!("graalvm-reachability-metadata-{version}"))?;
    if done.is_file() {
        return Ok(dir);
    }
    if config.offline() {
        bail!(
            "the GraalVM reachability metadata repository is not downloaded and Kargo is offline; build once while online, \
             or set [package.native-image] metadata-repository = false"
        );
    }
    let url = format!(
        "https://github.com/oracle/graalvm-reachability-metadata/releases/download/{version}/graalvm-reachability-metadata-{version}.zip"
    );
    let archive_path = config.toolchains_dir().join("downloads").join(format!("graalvm-reachability-metadata-{version}.zip"));
    crate::download::download(config, &url, &archive_path, Some(REACHABILITY_METADATA_SHA256))?;
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    archive::install(&archive_path, ArchiveKind::Zip, &dir)?;
    let _ = std::fs::remove_file(&archive_path);
    kargo_util::fs::write_string_atomic(&done, version)?;
    Ok(dir)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct IndexEntry {
    metadata_version: String,
    #[serde(default)]
    tested_versions: Vec<String>,
    #[serde(default)]
    latest: bool,
}

/// The repository's metadata directory for a library version: the entry that lists `version`
/// among its tested versions, else the one marked `latest`.
pub fn metadata_dir(repository: &Path, group: &str, artifact: &str, version: &str) -> Option<PathBuf> {
    let module = repository.join(group).join(artifact);
    let text = std::fs::read_to_string(module.join("index.json")).ok()?;
    let entries: Vec<IndexEntry> = serde_json::from_str(&text).ok()?;
    let entry = entries.iter().find(|e| e.tested_versions.iter().any(|v| v == version)).or_else(|| entries.iter().find(|e| e.latest))?;
    let dir = module.join(&entry.metadata_version);
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(dir: &Path) -> Config {
        Config::new(dir.to_path_buf(), Shell::new(Verbosity::Quiet))
    }

    fn fake_install(config: &Config, distribution: &str, version: &str, major: u32) -> PathBuf {
        let dir = config.graalvm_dir().join(format!("{distribution}-{version}"));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(native_image_in(&dir), "").unwrap();
        let meta = Metadata { distribution: distribution.into(), version: version.into(), major, home: String::new() };
        std::fs::write(dir.join(METADATA_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        dir
    }

    #[test]
    fn parses_disco_responses() {
        let packages = r#"{"result":[
            {"id":"old","distribution_version":"25.0.4","major_version":25,"filename":"graalvm-community-jdk-25.0.4_linux-x64_bin.tar.gz","latest_build_available":false},
            {"id":"new","distribution_version":"25.0.4.1.1","major_version":25,"filename":"graalvm-community-jdk-25.0.4.1.1_windows-x64_bin.zip","latest_build_available":true}],"message":""}"#;
        let (id, version, major, filename) = parse_packages(packages).unwrap().unwrap();
        assert_eq!((id.as_str(), version.as_str(), major), ("new", "25.0.4.1.1", 25));
        assert_eq!(ArchiveKind::from_name(&filename).unwrap(), ArchiveKind::Zip);
        assert!(parse_packages(r#"{"result":[]}"#).unwrap().is_none());
        assert!(parse_packages("{").is_err());

        let info = r#"{"result":[{"filename":"g.zip","direct_download_uri":"https://github.com/g.zip","checksum":"2733","checksum_type":"sha256"}],"message":""}"#;
        assert_eq!(parse_info(info).unwrap(), ("https://github.com/g.zip".to_owned(), "2733".to_owned()));
        let md5 = r#"{"result":[{"direct_download_uri":"u","checksum":"1","checksum_type":"md5"}]}"#;
        assert!(parse_info(md5).is_err());
    }

    #[test]
    fn platform_names() {
        assert_eq!(platform_for("macos", "aarch64").unwrap(), ("macos", "aarch64", "tar.gz"));
        assert_eq!(platform_for("windows", "x86_64").unwrap(), ("windows", "x64", "zip"));
        assert!(platform_for("freebsd", "x86_64").is_err());
        assert!(platform_for("linux", "riscv64").is_err());
    }

    #[test]
    fn lists_selects_and_removes_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        assert!(installed(&config).unwrap().is_empty());
        fake_install(&config, "community", "25.0.4", 25);
        fake_install(&config, "community", "25.0.4.1.1", 25);
        fake_install(&config, "oracle", "25.4.4.1.1", 25);
        fake_install(&config, "community", "21.0.2", 21);
        std::fs::create_dir_all(config.graalvm_dir().join(".staging-x")).unwrap();

        let community = |major| GraalvmRequest::Major { distribution: GraalvmDistribution::Community, major };
        let newest = find_installed(&config, &community(25)).unwrap().unwrap();
        assert_eq!(newest.release(), "community-25.0.4.1.1");
        let oracle = find_installed(&config, &"oracle-25".parse().unwrap()).unwrap().unwrap();
        assert_eq!(oracle.distribution, GraalvmDistribution::Oracle);
        assert!(find_installed(&config, &community(17)).unwrap().is_none());

        // An installed GraalVM resolves without network access.
        let offline = config.clone().with_offline(true);
        assert_eq!(resolve(&offline, &community(21)).unwrap().version, 21);
        let err = resolve(&offline, &community(17)).unwrap_err().to_string();
        assert!(err.contains("offline") && err.contains("kargo toolchain install graalvm 17"), "{err}");

        assert_eq!(remove(&config, "community-25.0.4").unwrap().len(), 1);
        assert_eq!(remove(&config, "25").unwrap().len(), 1, "only the community build of 25 is left");
        assert_eq!(remove(&config, "oracle-25").unwrap().len(), 1);
        assert!(remove(&config, "oracle-25").is_err());
        assert_eq!(installed(&config).unwrap().len(), 1);
        assert!(install(&config, &GraalvmRequest::System).is_err());
    }

    #[test]
    fn finds_reachability_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let module = repo.join("io.netty").join("netty-common");
        std::fs::create_dir_all(module.join("4.1.80.Final")).unwrap();
        std::fs::create_dir_all(module.join("5.0.0")).unwrap();
        std::fs::write(
            module.join("index.json"),
            r#"[{"metadata-version":"4.1.80.Final","tested-versions":["4.1.80.Final","4.1.100.Final"]},
                {"latest":true,"metadata-version":"5.0.0","tested-versions":["5.0.0"]}]"#,
        )
        .unwrap();
        assert_eq!(metadata_dir(repo, "io.netty", "netty-common", "4.1.100.Final"), Some(module.join("4.1.80.Final")));
        assert_eq!(metadata_dir(repo, "io.netty", "netty-common", "4.2.0.Final"), Some(module.join("5.0.0")), "falls back to latest");
        assert_eq!(metadata_dir(repo, "io.netty", "netty-buffer", "4.1.100.Final"), None);
    }
}
