//! The Android SDK of `[toolchain] android-sdk`: platforms, build-tools and platform-tools that Kargo installs.
//!
//! Kargo reads Google's SDK repository manifest ([`MANIFEST`] under [`REPOSITORY_URL`]), downloads the archives of the
//! packages a platform API level needs, checks each against the SHA-1 the manifest lists (the only checksum Google
//! publishes for them) and installs them into one SDK directory, `~/.kargo/toolchains/android-sdk`, in the layout
//! `sdkmanager` writes: `platforms/android-36/`, `build-tools/36.0.0/` and `platform-tools/`, each with the
//! `package.xml` the Android Gradle Plugin reads, and `licenses/` with the hashes of the accepted licenses.
//!
//! The packages need the user's acceptance of Google's license terms. Kargo never accepts them by itself: it shows
//! the terms and asks on a terminal, or the user passes `--accept-licenses` (see [`Licenses`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_manifest::AndroidSdkRequest;
use kargo_maven::Version;
use kargo_util::config::Config;

use crate::archive::{self, ArchiveKind};
use crate::install_lock;

/// Where Google publishes the SDK packages.
pub const REPOSITORY_URL: &str = "https://dl.google.com/android/repository";

/// The manifest of the packages, under [`REPOSITORY_URL`].
pub const MANIFEST: &str = "repository2-3.xml";

/// The manifest of the system images, in `sys-img/<tag family>/` under [`REPOSITORY_URL`]: Google keeps them apart
/// from [`MANIFEST`] (the `sys-img` add-ons `sdkmanager` reads), and their archives are relative to that directory.
const SYSTEM_IMAGE_MANIFEST: &str = "sys-img2-1.xml";

/// The package with `adb`.
pub const PLATFORM_TOOLS: &str = "platform-tools";

/// The id of the stable channel in the manifest; a package without a channel is on it.
const STABLE_CHANNEL: &str = "channel-0";

/// The build-tools version an Android Gradle Plugin version uses by default (its
/// `ToolsRevisionUtils.DEFAULT_BUILD_TOOLS_REVISION`), which Kargo installs with a platform.
pub fn build_tools_for_agp(agp: &str) -> &'static str {
    match crate::gradle::major_minor(agp) {
        Some((major, _)) if major >= 9 => "36.0.0",
        Some((8, minor)) if minor >= 8 => "35.0.0",
        Some((8, minor)) if minor >= 2 => "34.0.0",
        Some((8, 1)) => "33.0.1",
        Some((8, 0) | (7, 1..)) => "30.0.3",
        _ => "30.0.2",
    }
}

/// The package of platform `api`: `platforms;android-36` up to API 36, `platforms;android-37.0` after it, the
/// names the Android Gradle Plugin looks for.
pub fn platform_package(api: u32) -> String {
    if api <= 36 { format!("platforms;android-{api}") } else { format!("platforms;android-{api}.0") }
}

/// The package of build-tools `version`.
pub fn build_tools_package(version: &str) -> String {
    format!("build-tools;{version}")
}

/// The package of NDK `version`.
pub fn ndk_package(version: &str) -> String {
    format!("ndk;{version}")
}

/// The package of CMake `version`.
pub fn cmake_package(version: &str) -> String {
    format!("cmake;{version}")
}

/// The packages that build for platform `api` with the given build-tools.
pub fn packages_for(api: u32, build_tools: &str) -> Vec<String> {
    vec![platform_package(api), build_tools_package(build_tools), PLATFORM_TOOLS.to_owned()]
}

/// The package with the Android emulator.
pub const EMULATOR: &str = "emulator";

/// The package with `avdmanager`, which creates virtual devices.
pub const CMDLINE_TOOLS: &str = "cmdline-tools;latest";

/// The ABI of the system images that run well on this machine: the host's own, since the emulator only
/// virtualizes it.
pub fn host_abi() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64-v8a" } else { "x86_64" }
}

/// The system image tags Kargo uses, best first: Google APIs without the Play Store, which can be rooted and
/// logged out of, then the same with it, then the plain AOSP image.
const IMAGE_TAGS: [&str; 3] = ["google_apis", "google_apis_playstore", "default"];

/// The directory under `sys-img/` whose manifest lists the images of `tag`.
fn system_image_directory(tag: &str) -> &str {
    if tag == "default" { "android" } else { tag }
}

/// The packages that run virtual devices: the emulator, `avdmanager` and `adb`.
pub fn emulator_packages() -> Vec<String> {
    vec![EMULATOR.to_owned(), CMDLINE_TOOLS.to_owned(), PLATFORM_TOOLS.to_owned()]
}

/// The system image packages for `api` with `tag` and `abi`: `system-images;android-35;google_apis;x86_64`. The
/// platform naming rule applies, so the first one is what the repository most likely has, the second its other
/// spelling.
pub fn system_image_packages(api: u32, tag: &str, abi: &str) -> [String; 2] {
    let plain = format!("system-images;android-{api};{tag};{abi}");
    let minor = format!("system-images;android-{api}.0;{tag};{abi}");
    if api <= 36 { [plain, minor] } else { [minor, plain] }
}

/// The directory of package `path` (`build-tools;36.0.0`) in the SDK at `sdk`.
pub fn package_dir(sdk: &Path, path: &str) -> PathBuf {
    path.split(';').fold(sdk.to_path_buf(), |dir, part| dir.join(part))
}

/// Whether package `path` is installed in `sdk`, with the `package.xml` that `sdkmanager`, Android Studio and Kargo
/// write, or the `source.properties` of older installs.
pub fn is_installed(sdk: &Path, path: &str) -> bool {
    let dir = package_dir(sdk, path);
    dir.join("package.xml").is_file() || dir.join("source.properties").is_file()
}

/// The `packages` that are not installed in `sdk`.
pub fn missing(sdk: &Path, packages: &[String]) -> Vec<String> {
    packages.iter().filter(|p| !is_installed(sdk, p)).cloned().collect()
}

/// Whether `sdk` has accepted the Android SDK license, which lets the Android Gradle Plugin download missing packages
/// itself.
pub fn has_accepted_license(sdk: &Path) -> bool {
    sdk.join("licenses").join("android-sdk-license").is_file()
}

/// An installed package of the managed SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub path: String,
    pub dir: PathBuf,
}

/// The packages installed in `sdk`: its platforms, build-tools and platform-tools.
pub fn installed(sdk: &Path) -> Vec<InstalledPackage> {
    let mut out = Vec::new();
    for kind in ["platforms", "build-tools"] {
        let Ok(entries) = std::fs::read_dir(sdk.join(kind)) else { continue };
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort_by(|a, b| Version::parse(a.trim_start_matches("android-")).cmp(&Version::parse(b.trim_start_matches("android-"))));
        for name in names {
            let path = format!("{kind};{name}");
            if is_installed(sdk, &path) {
                out.push(InstalledPackage { dir: package_dir(sdk, &path), path });
            }
        }
    }
    for path in [PLATFORM_TOOLS, EMULATOR] {
        if is_installed(sdk, path) {
            out.push(InstalledPackage { path: path.to_owned(), dir: package_dir(sdk, path) });
        }
    }
    for name in subdirectories(&sdk.join("cmdline-tools")) {
        let path = format!("cmdline-tools;{name}");
        if is_installed(sdk, &path) {
            out.push(InstalledPackage { dir: package_dir(sdk, &path), path });
        }
    }
    out.extend(installed_system_images(sdk));
    out
}

/// The names of the directories in `dir`, sorted.
fn subdirectories(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> =
        entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// The system images installed in `sdk`: `system-images/android-35/google_apis/x86_64`.
pub fn installed_system_images(sdk: &Path) -> Vec<InstalledPackage> {
    let root = sdk.join("system-images");
    let mut out = Vec::new();
    for platform in subdirectories(&root) {
        for tag in subdirectories(&root.join(&platform)) {
            for abi in subdirectories(&root.join(&platform).join(&tag)) {
                let path = format!("system-images;{platform};{tag};{abi}");
                if is_installed(sdk, &path) {
                    out.push(InstalledPackage { dir: package_dir(sdk, &path), path });
                }
            }
        }
    }
    out
}

/// The system image of `sdk` for platform `api` that runs on this machine, if one is installed.
pub fn installed_system_image(sdk: &Path, api: u32) -> Option<String> {
    IMAGE_TAGS.iter().flat_map(|tag| system_image_packages(api, tag, host_abi())).find(|path| is_installed(sdk, path))
}

/// The file name of the program `name` of the SDK: `adb.exe`, `avdmanager.bat` on Windows.
fn program_name(name: &str) -> String {
    match (cfg!(windows), name) {
        (true, "avdmanager" | "sdkmanager") => format!("{name}.bat"),
        (true, _) => format!("{name}.exe"),
        (false, _) => name.to_owned(),
    }
}

/// `adb` in `sdk`, if its platform-tools are installed.
pub fn adb_in(sdk: &Path) -> Option<PathBuf> {
    Some(sdk.join(PLATFORM_TOOLS).join(program_name("adb"))).filter(|p| p.is_file())
}

/// The `emulator` of `sdk`, if installed.
pub fn emulator_in(sdk: &Path) -> Option<PathBuf> {
    Some(sdk.join(EMULATOR).join(program_name("emulator"))).filter(|p| p.is_file())
}

/// `avdmanager` in `sdk`: the newest command-line tools, or the `tools/` of older SDKs.
pub fn avdmanager_in(sdk: &Path) -> Option<PathBuf> {
    let program = program_name("avdmanager");
    let mut versions = subdirectories(&sdk.join("cmdline-tools"));
    // `latest` is the one `sdkmanager` keeps up to date; numbers sort after it by version.
    versions.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("latest", "latest") => std::cmp::Ordering::Equal,
        ("latest", _) => std::cmp::Ordering::Less,
        (_, "latest") => std::cmp::Ordering::Greater,
        _ => Version::parse(b).cmp(&Version::parse(a)),
    });
    versions
        .into_iter()
        .map(|v| sdk.join("cmdline-tools").join(v).join("bin").join(&program))
        .chain(std::iter::once(sdk.join("tools").join("bin").join(&program)))
        .find(|p| p.is_file())
}

/// Picks the system image to install for platform `api` from `manifests`: the first tag and spelling that Google has
/// for this machine's ABI. Returns the package and the index of the manifest it is in.
pub fn system_image_for(manifests: &[Repository], api: u32) -> Option<(String, usize)> {
    IMAGE_TAGS.iter().flat_map(|tag| system_image_packages(api, tag, host_abi())).find_map(|path| {
        let index = manifests.iter().position(|repo| repo.find(&path).is_some())?;
        Some((path, index))
    })
}

/// Installs what a virtual device for platform `api` needs and returns the SDK directory and the system image: the
/// emulator, `avdmanager`, `adb` and the image. An image that is installed already is kept.
pub fn ensure_emulator(config: &Config, api: u32, licenses: Licenses) -> Result<(PathBuf, String)> {
    let install = format!("kargo toolchain install android-sdk {api} --emulator");
    let sdk = config.android_sdk_dir();
    let mut packages = emulator_packages();
    let mut extra = Vec::new();
    let image = match installed_system_image(&sdk, api) {
        Some(image) => image,
        None => {
            // The images are in one manifest per tag family; fetch them in order of preference, until one has an image.
            let mut manifests = Vec::new();
            let mut found = None;
            let mut seen = Vec::new();
            for tag in IMAGE_TAGS {
                let directory = system_image_directory(tag);
                if seen.contains(&directory) {
                    continue;
                }
                seen.push(directory);
                manifests.push(Repository::fetch_system_images(config, tag)?);
                found = system_image_for(&manifests, api);
                if found.is_some() {
                    break;
                }
            }
            let (image, index) = found.with_context(|| {
                format!(
                    "Google's repository has no `{}` system image for Android {api} on this machine ({REPOSITORY_URL}/sys-img/); \
                     pick another API level with `--api`",
                    host_abi()
                )
            })?;
            extra.push(manifests.swap_remove(index));
            image
        }
    };
    packages.push(image.clone());
    ensure_from(config, &packages, licenses, &install, &extra)?;
    Ok((sdk, image))
}

/// How the licenses of the packages to install get accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Licenses {
    /// `--accept-licenses`: the user accepts them. Kargo still shows them.
    Accept,
    /// Show them and ask on the terminal; fail without one.
    Ask,
}

/// One download of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Archive {
    url: String,
    size: u64,
    sha1: String,
    host_os: Option<String>,
    host_arch: Option<String>,
}

impl Archive {
    /// Whether it runs on this machine: 0 not at all, 1 for any host, 2 for this host's OS and architecture.
    fn fit(&self) -> u8 {
        let os = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macosx"
        } else {
            "linux"
        };
        let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x64" };
        if self.host_os.as_deref().is_some_and(|o| o != os) || self.host_arch.as_deref().is_some_and(|a| a != arch) {
            0
        } else if self.host_arch.is_some() {
            2
        } else {
            1
        }
    }
}

/// A package of the repository manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePackage {
    /// The `sdkmanager` path, such as `platforms;android-36`.
    pub path: String,
    /// The package's revision, such as `36.0.0` or `2`.
    pub revision: String,
    pub display_name: String,
    /// The id of its license.
    pub license: Option<String>,
    stable: bool,
    archives: Vec<Archive>,
    /// The URL that the relative URLs of its archives are relative to.
    base: String,
    /// Its element as a `localPackage`, the way `package.xml` repeats it.
    local_xml: String,
}

impl RemotePackage {
    /// The archive for this machine.
    fn archive(&self) -> Option<&Archive> {
        self.archives.iter().filter(|a| a.fit() > 0).max_by_key(|a| a.fit())
    }
}

/// Google's repository manifest.
#[derive(Debug, Clone)]
pub struct Repository {
    packages: Vec<RemotePackage>,
    /// License texts by id.
    licenses: BTreeMap<String, String>,
    /// The `xmlns` declarations of the manifest's root, which the types in `package.xml` refer to.
    namespaces: String,
    /// The prefix of the common repository namespace, that `package.xml`'s root is in.
    common_prefix: String,
}

impl Repository {
    /// Downloads and parses the manifest.
    pub fn fetch(config: &Config) -> Result<Self> {
        let url = format!("{REPOSITORY_URL}/{MANIFEST}");
        let text = crate::download::get_text(config, &url).context("failed to download the Android SDK repository manifest")?;
        Self::parse(&text).with_context(|| format!("failed to read {url}"))
    }

    /// Downloads and parses the manifest of the system images of `tag`, which [`MANIFEST`] does not list.
    pub fn fetch_system_images(config: &Config, tag: &str) -> Result<Self> {
        let base = format!("{REPOSITORY_URL}/sys-img/{}", system_image_directory(tag));
        let url = format!("{base}/{SYSTEM_IMAGE_MANIFEST}");
        let text = crate::download::get_text(config, &url).context("failed to download the Android SDK system image manifest")?;
        Self::parse_at(&text, &base).with_context(|| format!("failed to read {url}"))
    }

    /// Parses a manifest whose archives are relative to [`REPOSITORY_URL`].
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_at(text, REPOSITORY_URL)
    }

    /// Parses a manifest whose archives are relative to `base`.
    pub fn parse_at(text: &str, base: &str) -> Result<Self> {
        let doc = roxmltree::Document::parse(text).context("invalid XML")?;
        let root = doc.root_element();
        let mut namespaces = String::new();
        let mut common_prefix = None;
        for ns in root.namespaces() {
            let Some(name) = ns.name().filter(|n| *n != "xml") else { continue };
            let _ = write!(namespaces, " xmlns:{name}=\"{}\"", escape(ns.uri()));
            if ns.uri().starts_with("http://schemas.android.com/repository/android/common/") {
                common_prefix = Some(name.to_owned());
            }
        }
        // The system image manifests do not declare the common namespace, but `package.xml` needs it.
        let common_prefix = common_prefix.unwrap_or_else(|| {
            namespaces.push_str(" xmlns:common=\"http://schemas.android.com/repository/android/common/02\"");
            "common".to_owned()
        });
        let mut licenses = BTreeMap::new();
        let mut packages = Vec::new();
        for node in root.children().filter(|n| n.is_element()) {
            match node.tag_name().name() {
                "license" => {
                    let id = node.attribute("id").context("a license has no id")?;
                    licenses.insert(id.to_owned(), node.text().unwrap_or_default().to_owned());
                }
                "remotePackage" => packages.push(remote_package(text, node, base)?),
                _ => {}
            }
        }
        Ok(Self { packages, licenses, namespaces, common_prefix })
    }

    /// The newest stable release of package `path` with an archive for this machine.
    pub fn find(&self, path: &str) -> Option<&RemotePackage> {
        self.stable().filter(|p| p.path == path).max_by(|a, b| Version::parse(&a.revision).cmp(&Version::parse(&b.revision)))
    }

    /// The stable packages with an archive for this machine.
    pub fn stable(&self) -> impl Iterator<Item = &RemotePackage> {
        self.packages.iter().filter(|p| p.stable && p.archive().is_some())
    }

    /// The platform API levels and the build-tools versions Kargo can install on this machine, newest first.
    pub fn available(&self) -> (Vec<u32>, Vec<String>) {
        let mut platforms: Vec<u32> = self
            .stable()
            .filter_map(|p| {
                let name = p.path.strip_prefix("platforms;android-")?;
                let api = name.strip_suffix(".0").unwrap_or(name).parse().ok()?;
                (platform_package(api) == p.path && api >= AndroidSdkRequest::MIN_API_LEVEL).then_some(api)
            })
            .collect();
        platforms.sort_unstable_by(|a, b| b.cmp(a));
        platforms.dedup();
        let mut build_tools: Vec<String> =
            self.stable().filter_map(|p| p.path.strip_prefix("build-tools;")).filter(|v| !v.contains('-')).map(str::to_owned).collect();
        build_tools.sort_by(|a, b| Version::parse(b).cmp(&Version::parse(a)));
        build_tools.dedup();
        (platforms, build_tools)
    }

    /// The text of license `id`.
    pub fn license(&self, id: &str) -> Option<&str> {
        self.licenses.get(id).map(String::as_str)
    }

    /// The `package.xml` of `package` once installed.
    fn package_xml(&self, package: &RemotePackage) -> String {
        let p = &self.common_prefix;
        let mut xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<{p}:repository{}>", self.namespaces);
        if let Some((id, text)) = package.license.as_deref().and_then(|id| Some((id, self.license(id)?))) {
            let _ = write!(xml, "<license id=\"{}\" type=\"text\">{}</license>", escape(id), escape(text));
        }
        let _ = writeln!(xml, "{}</{p}:repository>", package.local_xml);
        xml
    }
}

/// Reads a `remotePackage` element.
fn remote_package(text: &str, node: roxmltree::Node, base: &str) -> Result<RemotePackage> {
    fn child<'a, 'input>(parent: roxmltree::Node<'a, 'input>, name: &str) -> Option<roxmltree::Node<'a, 'input>> {
        parent.children().find(|c| c.has_tag_name(name))
    }
    fn child_text(parent: roxmltree::Node, name: &str) -> Option<String> {
        child(parent, name).and_then(|c| c.text()).map(|t| t.trim().to_owned())
    }
    let path = node.attribute("path").context("a package has no path")?.to_owned();
    let revision = match child(node, "revision") {
        Some(rev) => {
            let mut version: Vec<String> = ["major", "minor", "micro"].iter().filter_map(|n| child_text(rev, n)).collect();
            if version.is_empty() {
                bail!("package `{path}` has no revision");
            }
            if let Some(preview) = child_text(rev, "preview") {
                version.last_mut().expect("not empty").push_str(&format!("-rc{preview}"));
            }
            version.join(".")
        }
        None => bail!("package `{path}` has no revision"),
    };
    let channel = child(node, "channelRef").and_then(|c| c.attribute("ref")).unwrap_or(STABLE_CHANNEL);
    let mut archives = Vec::new();
    for archive in child(node, "archives").into_iter().flat_map(|a| a.children()).filter(|a| a.has_tag_name("archive")) {
        let Some(complete) = child(archive, "complete") else { continue };
        let checksum = child(complete, "checksum").filter(|c| c.attribute("type").is_none_or(|t| t == "sha1"));
        let (Some(url), Some(sha1)) = (child_text(complete, "url"), checksum.and_then(|c| c.text()).map(|t| t.trim().to_owned())) else {
            bail!("an archive of package `{path}` has no URL or SHA-1");
        };
        archives.push(Archive {
            url,
            sha1,
            size: child_text(complete, "size").and_then(|s| s.parse().ok()).unwrap_or(0),
            host_os: child_text(archive, "host-os"),
            host_arch: child_text(archive, "host-arch"),
        });
    }

    // `package.xml` repeats the element without its downloads and channel.
    let range = node.range();
    let mut local = String::new();
    let mut at = range.start;
    for skipped in node.children().filter(|c| c.has_tag_name("archives") || c.has_tag_name("channelRef")) {
        local.push_str(&text[at..skipped.range().start]);
        at = skipped.range().end;
    }
    local.push_str(&text[at..range.end]);
    let local = local
        .strip_prefix("<remotePackage")
        .and_then(|l| l.strip_suffix("</remotePackage>"))
        .with_context(|| format!("package `{path}` is not a plain <remotePackage> element"))?;
    let local_xml = format!("<localPackage{local}</localPackage>");

    Ok(RemotePackage {
        path,
        revision,
        display_name: child_text(node, "display-name").unwrap_or_default(),
        license: child(node, "uses-license").and_then(|l| l.attribute("ref")).map(str::to_owned),
        stable: channel == STABLE_CHANNEL,
        archives,
        base: base.to_owned(),
        local_xml,
    })
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The hash `licenses/<id>` records for an accepted license: the SHA-1 of its text, as `sdkmanager` computes it.
fn license_hash(text: &str) -> String {
    kargo_util::hash::checksums_bytes(text.as_bytes()).sha1
}

/// Whether license `id` with `text` is accepted in `sdk`.
fn is_accepted(sdk: &Path, id: &str, text: &str) -> bool {
    let hash = license_hash(text);
    std::fs::read_to_string(sdk.join("licenses").join(id)).is_ok_and(|f| f.lines().any(|l| l.trim() == hash))
}

/// Records the acceptance of license `id` in `sdk`, the way `sdkmanager --licenses` does.
fn record_acceptance(sdk: &Path, id: &str, text: &str) -> Result<()> {
    let file = sdk.join("licenses").join(id);
    let mut content = std::fs::read_to_string(&file).unwrap_or_default();
    content.push('\n');
    content.push_str(&license_hash(text));
    kargo_util::fs::write_string_atomic(&file, &content).with_context(|| format!("failed to write {}", file.display()))
}

/// Gets the user's acceptance of the licenses of `packages` not yet accepted in `sdk`, and records it.
fn accept_licenses(config: &Config, sdk: &Path, packages: &[(&Repository, &RemotePackage)], licenses: Licenses) -> Result<()> {
    let mut ids: Vec<&str> = packages.iter().filter_map(|(_, p)| p.license.as_deref()).collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        let text = packages
            .iter()
            .find_map(|(repo, _)| repo.license(id))
            .with_context(|| format!("the repository manifest lacks the text of license `{id}`"))?;
        if is_accepted(sdk, id, text) {
            continue;
        }
        let names: Vec<&str> = packages.iter().filter(|(_, p)| p.license.as_deref() == Some(id)).map(|(_, p)| p.path.as_str()).collect();
        let shell = config.shell();
        match licenses {
            Licenses::Accept => {
                shell.text(text.trim());
                shell.note(format!("accepted the license `{id}` above for {} (--accept-licenses)", names.join(", ")));
            }
            Licenses::Ask => {
                let question = format!("Do you accept the license `{id}` above for {}?", names.join(", "));
                match shell.confirm(text.trim(), &question) {
                    Some(true) => {}
                    Some(false) => bail!("the license `{id}` was not accepted, so nothing was installed"),
                    None => bail!(
                        "installing {} needs you to accept Google's license `{id}`; run `kargo toolchain install android-sdk \
                         --accept-licenses` to read and accept it, or run this command in a terminal to be asked",
                        names.join(", ")
                    ),
                }
            }
        }
        record_acceptance(sdk, id, text)?;
    }
    Ok(())
}

/// Installs the `packages` (`sdkmanager` paths) missing from the managed SDK and returns its directory. `install` is
/// the command that installs them by hand, for when automatic installs are off.
pub fn ensure(config: &Config, packages: &[String], licenses: Licenses, install: &str) -> Result<PathBuf> {
    ensure_from(config, packages, licenses, install, &[])
}

/// [`ensure`] with `extra` manifests (those of the system images) to look in after the main one.
fn ensure_from(config: &Config, packages: &[String], licenses: Licenses, install: &str, extra: &[Repository]) -> Result<PathBuf> {
    let sdk = config.android_sdk_dir();
    let _lock = install_lock(config, "android-sdk")?;
    let missing = missing(&sdk, packages);
    if missing.is_empty() {
        return Ok(sdk);
    }
    crate::ensure_auto_install(config, &format!("the Android SDK package {}", missing.join(", ")), install)?;
    let repo = Repository::fetch(config)?;
    let mut remote = Vec::new();
    for path in &missing {
        let Some(package) = std::iter::once(&repo).chain(extra).find_map(|r| r.find(path).map(|p| (r, p))) else {
            bail!(
                "the Android SDK package `{path}` is not in Google's repository for this platform ({REPOSITORY_URL}/{MANIFEST}); \
                 `kargo toolchain available android-sdk` lists the platforms you can install"
            );
        };
        remote.push(package);
    }
    accept_licenses(config, &sdk, &remote, licenses)?;
    for (repo, package) in remote {
        install_package(config, &sdk, repo, package)
            .with_context(|| format!("failed to install the Android SDK package `{}`", package.path))?;
    }
    Ok(sdk)
}

/// Downloads, verifies and unpacks one package into `sdk`.
fn install_package(config: &Config, sdk: &Path, repo: &Repository, package: &RemotePackage) -> Result<()> {
    let archive = package.archive().context("it has no archive for this platform")?;
    let url = if archive.url.contains("://") { archive.url.clone() } else { format!("{}/{}", package.base, archive.url) };
    let name = archive.url.rsplit('/').next().unwrap_or(&archive.url);
    let file = config.toolchains_dir().join("downloads").join(format!("android-{name}"));
    if !(file.is_file() && verify_sha1(&file, &archive.sha1).is_ok()) {
        let _ = std::fs::remove_file(&file);
        crate::download::download(config, &url, &file, None)?;
        verify_sha1(&file, &archive.sha1).inspect_err(|_| {
            let _ = std::fs::remove_file(&file);
        })?;
    }
    config.shell().status("Installing", format!("{} ({})", package.display_name, package.path));
    let dir = package_dir(sdk, &package.path);
    kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    archive::install(&file, ArchiveKind::Zip, &dir)?;
    let _ = std::fs::remove_file(&file);
    kargo_util::fs::write_string_atomic(&dir.join("package.xml"), &repo.package_xml(package))
}

/// Checks a file's SHA-1 against `expected`.
fn verify_sha1(file: &Path, expected: &str) -> Result<()> {
    let actual = kargo_util::hash::sha1_file(file).with_context(|| format!("failed to hash {}", file.display()))?;
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!("checksum mismatch for {}: expected sha1 {expected}, got {actual}", file.display());
    }
    Ok(())
}

/// The package `selector` names: a platform API level (`36`) or an `sdkmanager` path (`build-tools;36.0.0`).
pub fn package_for(selector: &str) -> Result<String> {
    let selector = selector.trim();
    if let Ok(api) = selector.parse::<u32>() {
        return Ok(platform_package(api));
    }
    let valid =
        |part: &str| !part.is_empty() && part != ".." && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !selector.split(';').all(valid) {
        bail!("invalid Android SDK package `{selector}` (expected an API level like `36` or a package like `build-tools;36.0.0`)");
    }
    Ok(selector.to_owned())
}

/// Removes a package from the managed SDK: a platform by API level, or any package by its path.
pub fn remove(config: &Config, selector: &str) -> Result<String> {
    let path = package_for(selector)?;
    let sdk = config.android_sdk_dir();
    let _lock = install_lock(config, "android-sdk")?;
    if !is_installed(&sdk, &path) {
        bail!("the Android SDK package `{path}` is not installed in {}; see `kargo toolchain list`", sdk.display());
    }
    kargo_util::fs::remove_dir_all_if_exists(&package_dir(&sdk, &path))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    /// A manifest in the shape of Google's, with one package per kind.
    fn manifest(archives: &[(&str, &str)]) -> String {
        let archive = |name: &str| {
            let (url, sha1) = archives.iter().find(|(n, _)| *n == name).copied().unwrap_or((name, "0000"));
            format!("<archive><complete><size>1</size><checksum type=\"sha1\">{sha1}</checksum><url>{url}</url></complete></archive>")
        };
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<sdk:sdk-repository xmlns:sdk="http://schemas.android.com/sdk/android/repo/repository2/03" xmlns:common="http://schemas.android.com/repository/android/common/02" xmlns:generic="http://schemas.android.com/repository/android/generic/02" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <license id="android-sdk-license" type="text">Terms &amp; conditions
</license>
  <channel id="channel-0">stable</channel>
  <channel id="channel-1">beta</channel>
  <remotePackage path="platforms;android-36">
    <type-details xsi:type="sdk:platformDetailsType"><api-level>36</api-level><layoutlib api="15"/></type-details>
    <revision><major>2</major></revision>
    <display-name>Android SDK Platform 36</display-name>
    <uses-license ref="android-sdk-license"/>
    <channelRef ref="channel-0"/>
    <archives>{}</archives>
  </remotePackage>
  <remotePackage path="build-tools;36.0.0">
    <type-details xsi:type="generic:genericDetailsType"/>
    <revision><major>36</major><minor>0</minor><micro>0</micro></revision>
    <display-name>Android SDK Build-Tools 36</display-name>
    <uses-license ref="android-sdk-license"/>
    <archives>{}</archives>
  </remotePackage>
  <remotePackage path="build-tools;37.0.0-rc1">
    <type-details xsi:type="generic:genericDetailsType"/>
    <revision><major>37</major><minor>0</minor><micro>0</micro><preview>1</preview></revision>
    <display-name>Android SDK Build-Tools 37-rc1</display-name>
    <uses-license ref="android-sdk-license"/>
    <channelRef ref="channel-1"/>
    <archives>{}</archives>
  </remotePackage>
  <remotePackage path="platform-tools">
    <type-details xsi:type="generic:genericDetailsType"/>
    <revision><major>37</major><minor>0</minor><micro>1</micro></revision>
    <display-name>Android SDK Platform-Tools</display-name>
    <uses-license ref="android-sdk-license"/>
    <channelRef ref="channel-0"/>
    <archives>
      <archive><complete><size>1</size><checksum type="sha1">aaaa</checksum><url>pt-elsewhere.zip</url></complete><host-os>none</host-os></archive>
      {}
    </archives>
  </remotePackage>
</sdk:sdk-repository>
"#,
            archive("platform.zip"),
            archive("build-tools.zip"),
            archive("build-tools-rc.zip"),
            archive("platform-tools.zip"),
        )
    }

    #[test]
    fn package_names_follow_agp() {
        assert_eq!(platform_package(36), "platforms;android-36");
        assert_eq!(platform_package(35), "platforms;android-35");
        assert_eq!(platform_package(37), "platforms;android-37.0");
        assert_eq!(build_tools_for_agp("9.4.1"), "36.0.0");
        assert_eq!(build_tools_for_agp("10.0.0"), "36.0.0");
        assert_eq!(build_tools_for_agp("8.13.0"), "35.0.0");
        assert_eq!(build_tools_for_agp("8.7.3"), "34.0.0");
        assert_eq!(build_tools_for_agp("8.1.4"), "33.0.1");
        assert_eq!(build_tools_for_agp("7.4.2"), "30.0.3");
        assert_eq!(build_tools_for_agp("7.0.4"), "30.0.2");
        assert_eq!(package_dir(Path::new("sdk"), "build-tools;36.0.0"), Path::new("sdk").join("build-tools").join("36.0.0"));
        assert_eq!(package_for("36").unwrap(), "platforms;android-36");
        assert_eq!(package_for("build-tools;36.0.0").unwrap(), "build-tools;36.0.0");
        assert!(package_for("platforms;..").is_err());
        assert!(package_for("a/b").is_err());
    }

    #[test]
    fn parses_the_repository_manifest() {
        let repo = Repository::parse(&manifest(&[])).unwrap();
        let platform = repo.find("platforms;android-36").unwrap();
        assert_eq!(platform.revision, "2");
        assert_eq!(platform.display_name, "Android SDK Platform 36");
        assert_eq!(platform.license.as_deref(), Some("android-sdk-license"));
        assert_eq!(platform.archive().unwrap().url, "platform.zip");
        // Without a channel, a package is stable; previews are not.
        assert_eq!(repo.find("build-tools;36.0.0").unwrap().revision, "36.0.0");
        assert!(repo.find("build-tools;37.0.0-rc1").is_none());
        // The archive for another OS is skipped.
        assert_eq!(repo.find(PLATFORM_TOOLS).unwrap().archive().unwrap().url, "platform-tools.zip");
        assert_eq!(repo.license("android-sdk-license"), Some("Terms & conditions\n"));
        assert_eq!(repo.available(), (vec![36], vec!["36.0.0".to_owned()]));

        let xml = repo.package_xml(platform);
        assert!(xml.contains("<common:repository xmlns:sdk="), "{xml}");
        assert!(xml.contains("<license id=\"android-sdk-license\" type=\"text\">Terms &amp; conditions\n</license>"), "{xml}");
        assert!(xml.contains("<localPackage path=\"platforms;android-36\">"), "{xml}");
        assert!(
            xml.contains("xsi:type=\"sdk:platformDetailsType\"") && xml.trim_end().ends_with("</localPackage></common:repository>"),
            "{xml}"
        );
        assert!(!xml.contains("archives") && !xml.contains("channelRef"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
    }

    #[test]
    fn finds_emulator_packages_and_system_images() {
        assert_eq!(system_image_packages(35, "google_apis", "x86_64")[0], "system-images;android-35;google_apis;x86_64");
        assert_eq!(system_image_packages(37, "google_apis", "arm64-v8a")[0], "system-images;android-37.0;google_apis;arm64-v8a");
        assert_eq!(emulator_packages(), ["emulator", "cmdline-tools;latest", "platform-tools"]);

        // The main manifest lists no system images; Google's own are in manifests of their own, without the common
        // namespace, whose archives are relative to their directory.
        let abi = host_abi();
        let image = format!("system-images;android-35;google_apis_playstore;{abi}");
        let plain = format!("system-images;android-36;default;{abi}");
        let sys_img = |path: &str| {
            format!(
                "<?xml version=\"1.0\"?><sys-img:sdk-sys-img xmlns:sys-img=\"http://schemas.android.com/sdk/android/repo/sys-img2/01\" \
                 xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><license id=\"android-sdk-license\" type=\"text\">Terms</license>\
                 <remotePackage path=\"{path}\"><type-details xsi:type=\"sys-img:sysImgDetailsType\"><api-level>35</api-level></type-details>\
                 <revision><major>1</major></revision><display-name>Image</display-name><uses-license ref=\"android-sdk-license\"/>\
                 <archives><archive><complete><size>1</size><checksum>aa</checksum><url>image.zip</url></complete></archive></archives>\
                 </remotePackage></sys-img:sdk-sys-img>"
            )
        };
        assert!(Repository::parse(&manifest(&[])).unwrap().find(&image).is_none());
        let playstore = Repository::parse_at(&sys_img(&image), "base/google_apis_playstore").unwrap();
        let android = Repository::parse_at(&sys_img(&plain), "base/android").unwrap();
        let found = playstore.find(&image).unwrap();
        assert_eq!(found.base, "base/google_apis_playstore");
        assert_eq!(found.archive().unwrap().url, "image.zip");
        let xml = playstore.package_xml(found);
        assert!(
            xml.contains("<common:repository ") && xml.contains("xmlns:common=\"http://schemas.android.com/repository/android/common/02\"")
        );
        assert!(xml.contains("<localPackage path=\"system-images;android-35;google_apis_playstore;"), "{xml}");
        roxmltree::Document::parse(&xml).unwrap();
        let manifests = [playstore, android];
        assert_eq!(system_image_for(&manifests, 35), Some((image.clone(), 0)));
        // The plain image is the fallback when no Google APIs image exists, and it comes from its own manifest.
        assert_eq!(system_image_for(&manifests, 36), Some((plain, 1)));
        assert_eq!(system_image_for(&manifests, 34), None);

        // Installed images, the tools and the emulator are read from the SDK's layout.
        let sdk = tempfile::tempdir().unwrap();
        let dir = package_dir(sdk.path(), &image);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.xml"), "<x/>").unwrap();
        for tool in ["emulator", "cmdline-tools;latest"] {
            std::fs::create_dir_all(package_dir(sdk.path(), tool)).unwrap();
            std::fs::write(package_dir(sdk.path(), tool).join("package.xml"), "<x/>").unwrap();
        }
        assert_eq!(installed_system_image(sdk.path(), 35).as_deref(), Some(image.as_str()));
        assert_eq!(installed_system_image(sdk.path(), 36), None);
        let paths: Vec<String> = installed(sdk.path()).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["emulator".to_owned(), "cmdline-tools;latest".to_owned(), image.clone()]);
        assert_eq!(emulator_in(sdk.path()), None);
        assert_eq!(avdmanager_in(sdk.path()), None);
        let write = |path: PathBuf| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            path
        };
        let emulator = write(sdk.path().join("emulator").join(program_name("emulator")));
        let avdmanager = write(sdk.path().join("cmdline-tools/latest/bin").join(program_name("avdmanager")));
        assert_eq!(emulator_in(sdk.path()), Some(emulator));
        assert_eq!(avdmanager_in(sdk.path()), Some(avdmanager));
        // A newer numbered version of the tools is not preferred over `latest`, but older SDKs' `tools/` still work.
        let numbered = write(sdk.path().join("cmdline-tools/19.0/bin").join(program_name("avdmanager")));
        assert_ne!(avdmanager_in(sdk.path()), Some(numbered));
    }

    #[test]
    fn records_accepted_licenses_like_sdkmanager() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_accepted(tmp.path(), "android-sdk-license", "terms"));
        record_acceptance(tmp.path(), "android-sdk-license", "terms").unwrap();
        record_acceptance(tmp.path(), "android-sdk-license", "new terms").unwrap();
        assert!(is_accepted(tmp.path(), "android-sdk-license", "terms") && is_accepted(tmp.path(), "android-sdk-license", "new terms"));
        let file = std::fs::read_to_string(tmp.path().join("licenses/android-sdk-license")).unwrap();
        assert_eq!(file, format!("\n{}\n{}", license_hash("terms"), license_hash("new terms")));
        assert!(has_accepted_license(tmp.path()));
    }

    #[test]
    fn installs_verifies_and_removes_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let served = tmp.path().join("served");
        std::fs::create_dir_all(&served).unwrap();
        let zip = |name: &str, top: &str, file: &str| -> String {
            let path = served.join(name);
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            writer.start_file(format!("{top}/{file}"), zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut writer, b"x").unwrap();
            writer.finish().unwrap();
            kargo_util::hash::sha1_file(&path).unwrap()
        };
        let sums = [
            ("platform.zip", zip("platform.zip", "android-36", "android.jar")),
            ("build-tools.zip", zip("build-tools.zip", "android-16", "aapt2")),
            ("platform-tools.zip", zip("platform-tools.zip", "platform-tools", "adb")),
        ];
        let archives: Vec<(&str, &str)> = sums.iter().map(|(n, s)| (*n, s.as_str())).collect();
        std::fs::write(served.join(MANIFEST), manifest(&archives)).unwrap();
        let url = format!("file:///{}", served.display().to_string().replace('\\', "/").trim_start_matches('/'));
        std::fs::write(tmp.path().join("config.toml"), format!("[toolchain-mirrors]\n\"{REPOSITORY_URL}\" = \"{url}\"\n")).unwrap();
        let settings = kargo_util::settings::Settings::load(tmp.path(), None).unwrap();
        let config = Config::new(tmp.path().join("home"), Shell::new(Verbosity::Quiet)).with_settings(settings).unwrap();

        let packages = packages_for(36, "36.0.0");
        let sdk = ensure(&config, &packages, Licenses::Accept, "kargo toolchain install android-sdk").unwrap();
        assert_eq!(sdk, config.android_sdk_dir());
        assert!(sdk.join("platforms/android-36/android.jar").is_file());
        assert!(sdk.join("build-tools/36.0.0/aapt2").is_file() && sdk.join("platform-tools/adb").is_file());
        assert!(sdk.join("platforms/android-36/package.xml").is_file());
        assert!(missing(&sdk, &packages).is_empty());
        let paths: Vec<String> = installed(&sdk).into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["platforms;android-36", "build-tools;36.0.0", "platform-tools"]);
        // Accepted now, so asking is not needed again.
        remove(&config, "36").unwrap();
        ensure(&config, &packages, Licenses::Ask, "kargo toolchain install android-sdk").unwrap();

        // A download that does not match its checksum is not installed.
        remove(&config, "36").unwrap();
        std::fs::write(served.join("platform.zip"), b"tampered").unwrap();
        let err = format!("{:#}", ensure(&config, &packages, Licenses::Accept, "x").unwrap_err());
        assert!(err.contains("platforms;android-36") && err.contains("checksum mismatch"), "{err}");
        assert!(!sdk.join("platforms/android-36").exists());

        let err = format!("{:#}", ensure(&config, &packages_for(30, "36.0.0"), Licenses::Accept, "x").unwrap_err());
        assert!(err.contains("`platforms;android-30` is not in Google's repository"), "{err}");
        assert!(remove(&config, "36").unwrap_err().to_string().contains("not installed"));
        assert_eq!(remove(&config, "build-tools;36.0.0").unwrap(), "build-tools;36.0.0");
    }
}
