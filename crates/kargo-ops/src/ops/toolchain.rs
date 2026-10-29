//! `kargo toolchain`: managing JDKs, GraalVMs, Kotlin/Native and Gradle distributions, the Android
//! SDK, and the Kotlin language servers behind `kargo lsp`.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use kargo_manifest::{AndroidSdkRequest, GraalvmRequest, JdkRequest, Manifest, TargetKind};
use kargo_maven::MavenClient;
use kargo_toolchain::android_sdk::{self, Licenses};
use kargo_toolchain::archive::{dir_size, human_size};
use kargo_toolchain::jdks::{self, ManagedJdk};
use kargo_toolchain::native::{buildable_on_host, resolve_platform};
use kargo_toolchain::{Graalvm, GradleDistribution, Jdk, LanguageServer, NativeToolchain, ServerKind, adoptium, graalvm};
use kargo_util::config::Config;

/// Kinds of toolchains Kargo manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Jdk,
    /// A GraalVM for `kargo package --format native-image`.
    Graalvm,
    Native,
    Gradle,
    /// Packages of the Kargo-managed Android SDK: a platform, its build-tools and platform-tools.
    AndroidSdk,
    /// A Kotlin language server for `kargo lsp`.
    Server(ServerKind),
}

/// The managed Gradle version an Android project uses, if it uses one: none for a
/// project with its own `gradlew`, or for a project without the Android target.
fn project_gradle(project: &Manifest) -> Result<Option<String>> {
    let Some(android) = &project.targets.android else { return Ok(None) };
    if let Some(version) = &android.config.gradle_version {
        kargo_toolchain::gradle::check_compatible(crate::gradle::agp_version(&android.config), version)?;
        return Ok(Some(version.clone()));
    }
    Ok(Some(kargo_toolchain::gradle::gradle_for_agp(crate::gradle::agp_version(&android.config))?.version().to_owned()))
}

/// The SDK packages an Android project builds with, if it uses the Kargo-managed SDK.
fn project_android_sdk(project: &Manifest) -> Option<Vec<String>> {
    let android = project.targets.android.as_ref()?;
    project.toolchain.android_sdk.api_level()?;
    Some(crate::gradle::sdk_packages(&android.config))
}

/// The Konan targets of the project's `[target.native] platforms`, `host` resolved; none
/// without the Native target.
fn native_platforms(project: &Manifest) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in project.targets.native.iter().flat_map(|n| &n.platforms) {
        let resolved = resolve_platform(p)?;
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// Whether a managed JDK is the one `request` selects among `installed`.
fn is_selected(jdk: &ManagedJdk, request: &JdkRequest, installed: &[ManagedJdk]) -> bool {
    match request {
        JdkRequest::System => false,
        JdkRequest::Release(r) => &jdk.release == r,
        JdkRequest::Major(m) => {
            jdk.major == *m && installed.iter().rfind(|j| j.major == *m).is_some_and(|newest| newest.release == jdk.release)
        }
    }
}

/// Renders `kargo toolchain list`.
pub fn list(config: &Config, project: Option<&Manifest>) -> Result<String> {
    let mut out = String::new();
    let installed = jdks::installed(config)?;
    let jdk_request = project.map(|m| m.toolchain.jdk.clone());
    let system = Jdk::find();
    // A project on the system JDK uses a managed one on a machine without Java.
    let fallback = match project {
        Some(m) if m.toolchain.jdk == JdkRequest::System && matches!(system, Ok(None)) => {
            jdks::find_fallback(config, crate::ops::context::jdk_needs(m))?
        }
        _ => None,
    };

    let _ = writeln!(out, "JDKs ({}):", config.jdks_dir().display());
    if installed.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for jdk in &installed {
        let used = jdk_request.as_ref().is_some_and(|r| is_selected(jdk, r, &installed))
            || fallback.as_ref().is_some_and(|f| f.release == jdk.release);
        let _ = writeln!(
            out,
            "    {:<22} {:>3}  {:<8} {:>10}{}",
            jdk.release,
            jdk.major,
            jdk.vendor,
            human_size(dir_size(&jdk.dir)),
            if used { "  (used by this project)" } else { "" }
        );
    }
    match system {
        Ok(Some(system)) => {
            let used = matches!(jdk_request, Some(JdkRequest::System));
            let _ = writeln!(
                out,
                "    system: JDK {} at {}{}",
                system.version,
                system.home.display(),
                if used { "  (used by this project)" } else { "" }
            );
        }
        Ok(None) | Err(_) => {
            let _ = writeln!(out, "    system: none found (JAVA_HOME / PATH)");
        }
    }

    let graalvms = graalvm::installed(config)?;
    let graalvm_request = project.map(|m| m.toolchain.graalvm.clone());
    let _ = writeln!(
        out,
        "
GraalVM ({}):",
        config.graalvm_dir().display()
    );
    if graalvms.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for installed in &graalvms {
        let used = graalvm_request.as_ref().is_some_and(|r| {
            graalvm::find_installed(config, r).ok().flatten().is_some_and(|selected| selected.release() == installed.release())
        });
        let _ = writeln!(
            out,
            "    {:<26} {:>3}  {:>10}{}",
            installed.release(),
            installed.major,
            human_size(dir_size(&installed.dir)),
            if used { "  (used by this project)" } else { "" }
        );
    }
    if let Ok(system) = Graalvm::detect() {
        let used = matches!(graalvm_request, Some(GraalvmRequest::System));
        let _ = writeln!(
            out,
            "    system: GraalVM for JDK {} at {}{}",
            system.version,
            system.home.display(),
            if used { "  (used by this project)" } else { "" }
        );
    }

    let natives = NativeToolchain::installed(config)?;
    let kotlin = project.filter(|m| m.targets.is_enabled(TargetKind::Native)).map(|m| m.kotlin.version.clone());
    let _ = writeln!(out, "\nKotlin/Native ({}):", config.native_dir().display());
    if natives.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for native in &natives {
        let used = kotlin.as_deref() == Some(native.version.as_str());
        let _ = writeln!(
            out,
            "    {:<22} {:>10}{}",
            native.version,
            human_size(dir_size(&native.home)),
            if used { "  (used by this project)" } else { "" }
        );
    }
    let data = config.konan_data_dir();
    if data.is_dir() {
        let _ = writeln!(out, "    dependencies (LLVM, sysroots): {} at {}", human_size(dir_size(&data)), data.display());
    }
    if let (Some(version), Some(m)) = (&kotlin, project)
        && let Some(toolchain) = natives.iter().find(|n| &n.version == version)
    {
        for konan_target in native_platforms(m)? {
            let state = if !buildable_on_host(&konan_target) {
                "builds only on macOS"
            } else if toolchain.has_dependencies(&konan_target) {
                "downloaded"
            } else {
                "not downloaded (`kargo toolchain install` downloads them)"
            };
            let _ = writeln!(out, "    dependencies for {konan_target}: {state}");
        }
    }

    let gradles = GradleDistribution::installed(config)?;
    let gradle = project.and_then(|m| project_gradle(m).ok().flatten());
    let _ = writeln!(out, "\nGradle ({}):", config.gradle_dir().display());
    if gradles.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for dist in &gradles {
        let used = gradle.as_deref() == Some(dist.version.as_str());
        let _ = writeln!(
            out,
            "    {:<22} {:>10}{}",
            dist.version,
            human_size(dir_size(&dist.home)),
            if used { "  (used by this project)" } else { "" }
        );
    }

    let sdk = config.android_sdk_dir();
    let used = project.and_then(project_android_sdk).unwrap_or_default();
    let _ = writeln!(out, "\nAndroid SDK ({}):", sdk.display());
    let packages = android_sdk::installed(&sdk);
    if packages.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for package in &packages {
        let _ = writeln!(
            out,
            "    {:<26} {:>10}{}",
            package.path,
            human_size(dir_size(&package.dir)),
            if used.contains(&package.path) { "  (used by this project)" } else { "" }
        );
    }
    match crate::gradle::android_sdk() {
        Some(system) => {
            let used = project.is_some_and(|m| m.targets.android.is_some() && m.toolchain.android_sdk == AndroidSdkRequest::System);
            let _ = writeln!(out, "    system: {}{}", system.display(), if used { "  (used by this project)" } else { "" });
        }
        None => {
            let _ = writeln!(out, "    system: none found (ANDROID_HOME / ANDROID_SDK_ROOT)");
        }
    }

    let servers = LanguageServer::installed(config)?;
    let _ = writeln!(
        out,
        "
Kotlin language servers and debug adapter ({}):",
        config.toolchains_dir().display()
    );
    if servers.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for server in &servers {
        let pinned = server.version == server.kind.default_version();
        let _ = writeln!(
            out,
            "    {:<20} {:<11} {:>10}{}",
            server.kind.as_str(),
            server.version,
            human_size(dir_size(&server.home)),
            if pinned { format!("  (used by {})", server.kind.used_by()) } else { String::new() }
        );
    }
    Ok(out)
}

/// Installs a toolchain. `version` defaults to what the project needs; `licenses` says how the licenses of
/// Android SDK packages get accepted.
pub fn install(config: &Config, project: Option<&Manifest>, kind: ToolKind, version: Option<&str>, licenses: Licenses) -> Result<String> {
    // Asked for by name, so `KARGO_AUTO_INSTALL=0` doesn't apply.
    let config = &config.clone().with_auto_install(true);
    match kind {
        ToolKind::Jdk => {
            let request: JdkRequest = match (version, project) {
                (Some(v), _) => v.parse()?,
                (None, Some(m)) if m.toolchain.jdk != JdkRequest::System => m.toolchain.jdk.clone(),
                (None, _) => bail!("which JDK? e.g. `kargo toolchain install jdk 21` (or set [toolchain] jdk in kargo.toml)"),
            };
            let jdk = jdks::install(config, &request)?;
            Ok(format!("JDK {} at {}", jdk.release, jdk.home.display()))
        }
        ToolKind::Graalvm => {
            let request: GraalvmRequest = match (version, project) {
                (Some(v), _) => v.parse()?,
                (None, Some(m)) if m.toolchain.graalvm != GraalvmRequest::System => m.toolchain.graalvm.clone(),
                (None, _) => bail!("which GraalVM? e.g. `kargo toolchain install graalvm 25` (or set [toolchain] graalvm in kargo.toml)"),
            };
            let installed = graalvm::install(config, &request)?;
            Ok(format!("{} {} at {}", installed.distribution.title(), installed.version, installed.home.display()))
        }
        ToolKind::Native => {
            let version = match (version, project) {
                (Some(v), _) => v.to_owned(),
                (None, Some(m)) => m.kotlin.version.clone(),
                (None, None) => bail!("which version? e.g. `kargo toolchain install native 2.4.21`"),
            };
            let repos = project.map(|m| m.repositories.clone()).unwrap_or_else(|| vec![kargo_maven::Repository::maven_central()]);
            let client = MavenClient::new(config, repos);
            let toolchain = NativeToolchain::ensure(config, &client, &version)?;
            // The project's platforms, so builds (and the bundled clang) work offline afterwards.
            let mut ready = Vec::new();
            for konan_target in project.map(native_platforms).transpose()?.unwrap_or_default() {
                if buildable_on_host(&konan_target) {
                    toolchain.ensure_dependencies(config, &konan_target)?;
                    ready.push(konan_target);
                } else {
                    config.shell().note(format!("skipping the Kotlin/Native dependencies for {konan_target}, which builds only on macOS"));
                }
            }
            let deps = if ready.is_empty() { String::new() } else { format!(" (dependencies for {})", ready.join(", ")) };
            Ok(format!("Kotlin/Native {} at {}{deps}", toolchain.version, toolchain.home.display()))
        }
        ToolKind::Gradle => {
            let version = match (version, project.map(project_gradle).transpose()?.flatten()) {
                (Some(v), _) => v.to_owned(),
                (None, Some(v)) => v,
                (None, None) => kargo_toolchain::gradle::gradle_for_agp(crate::gradle::AGP_VERSION)?.version().to_owned(),
            };
            let dist = GradleDistribution::ensure(config, &version)?;
            Ok(format!("Gradle {} at {}", dist.version, dist.home.display()))
        }
        ToolKind::AndroidSdk => {
            let android = project.and_then(|m| m.targets.android.as_ref());
            let api = match (version, project) {
                (Some(v), _) => match v.parse::<AndroidSdkRequest>()? {
                    AndroidSdkRequest::Platform(api) => api,
                    AndroidSdkRequest::System => {
                        bail!(
                            "Kargo does not install the system Android SDK; name an API level, e.g. `kargo toolchain install android-sdk 36`"
                        )
                    }
                },
                (None, Some(m)) if m.toolchain.android_sdk != AndroidSdkRequest::System => {
                    android.map_or_else(|| m.toolchain.android_sdk.api_level().expect("not the system SDK"), |a| a.config.compile_sdk)
                }
                (None, _) => {
                    bail!(
                        "which Android platform? e.g. `kargo toolchain install android-sdk 36` (or set [toolchain] android-sdk in kargo.toml)"
                    )
                }
            };
            let agp = android.map_or(crate::gradle::AGP_VERSION, |a| crate::gradle::agp_version(&a.config));
            let build_tools =
                android.and_then(|a| a.config.build_tools.as_deref()).unwrap_or_else(|| android_sdk::build_tools_for_agp(agp));
            let sdk = android_sdk::ensure(
                config,
                &android_sdk::packages_for(api, build_tools),
                licenses,
                &format!("kargo toolchain install android-sdk {api}"),
            )?;
            Ok(format!("Android SDK platform {api}, build-tools {build_tools} and platform-tools at {}", sdk.display()))
        }
        ToolKind::Server(kind) => {
            let server = LanguageServer::ensure(config, kind, version.unwrap_or(kind.default_version()))?;
            Ok(format!("{} {} at {}", kind.title(), server.version, server.home.display()))
        }
    }
}

/// `kargo toolchain install android-sdk --emulator`: the emulator, `avdmanager` and a system image for platform
/// `version` (default: the project's `compile-sdk`) in the Kargo-managed SDK.
pub fn install_android_emulator(config: &Config, project: Option<&Manifest>, version: Option<&str>, licenses: Licenses) -> Result<String> {
    let config = &config.clone().with_auto_install(true);
    let api = match (version, project.and_then(|m| m.targets.android.as_ref())) {
        (Some(v), _) => match v.parse::<AndroidSdkRequest>()? {
            AndroidSdkRequest::Platform(api) => api,
            AndroidSdkRequest::System => bail!(
                "Kargo does not install the system Android SDK; name an API level, e.g. `kargo toolchain install android-sdk 36 --emulator`"
            ),
        },
        (None, Some(android)) => android.config.compile_sdk,
        (None, None) => bail!("which Android platform? e.g. `kargo toolchain install android-sdk 36 --emulator`"),
    };
    let (sdk, image) = android_sdk::ensure_emulator(config, api, licenses)?;
    Ok(format!("the Android emulator, avdmanager and system image {image} at {}", sdk.display()))
}

/// Installs everything the project needs (`kargo toolchain install` without arguments).
pub fn install_for_project(config: &Config, manifest: &Manifest, licenses: Licenses) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if manifest.toolchain.jdk != JdkRequest::System {
        done.push(install(config, Some(manifest), ToolKind::Jdk, None, licenses)?);
    } else {
        // The JDK a build would install on a machine without one.
        let needs = crate::ops::context::jdk_needs(manifest);
        if Jdk::find()?.is_none() && jdks::find_fallback(config, needs)?.is_none() {
            done.push(install(config, Some(manifest), ToolKind::Jdk, Some(&needs.choose().to_string()), licenses)?);
        }
    }
    if manifest.toolchain.graalvm != GraalvmRequest::System {
        done.push(install(config, Some(manifest), ToolKind::Graalvm, None, licenses)?);
    }
    if manifest.targets.is_enabled(TargetKind::Native) {
        done.push(install(config, Some(manifest), ToolKind::Native, None, licenses)?);
    }
    if project_gradle(manifest)?.is_some() {
        done.push(install(config, Some(manifest), ToolKind::Gradle, None, licenses)?);
    }
    if project_android_sdk(manifest).is_some() {
        done.push(install(config, Some(manifest), ToolKind::AndroidSdk, None, licenses)?);
    }
    Ok(done)
}

/// Removes installed toolchains; returns descriptions of what was removed.
pub fn remove(config: &Config, kind: ToolKind, selector: &str) -> Result<Vec<String>> {
    match kind {
        ToolKind::Jdk => Ok(jdks::remove(config, selector)?.into_iter().map(|j| format!("JDK {}", j.release)).collect()),
        ToolKind::Graalvm => {
            Ok(graalvm::remove(config, selector)?.into_iter().map(|g| format!("{} {}", g.distribution.title(), g.version)).collect())
        }
        ToolKind::Native => {
            let removed = NativeToolchain::remove(config, selector)?;
            Ok(vec![format!("Kotlin/Native {}", removed.version)])
        }
        ToolKind::Gradle => {
            let removed = GradleDistribution::remove(config, selector)?;
            Ok(vec![format!("Gradle {}", removed.version)])
        }
        ToolKind::AndroidSdk => Ok(vec![format!("Android SDK package {}", android_sdk::remove(config, selector)?)]),
        ToolKind::Server(kind) => {
            let removed = LanguageServer::remove(config, kind, selector)?;
            Ok(vec![format!("{} {}", kind.title(), removed.version)])
        }
    }
}

/// Renders what can be installed: JDK feature versions, or with `kind` the Android SDK platforms and build-tools.
pub fn available(config: &Config, kind: Option<ToolKind>) -> Result<String> {
    match kind {
        None | Some(ToolKind::Jdk) => {}
        Some(ToolKind::AndroidSdk) => return available_android(config),
        Some(_) => bail!("`kargo toolchain available` lists JDK versions, and Android SDK platforms with `android-sdk`"),
    }
    let releases = adoptium::available(config)?;
    let mut out = String::from("Temurin JDK versions (install with `kargo toolchain install jdk <version>`):\n");
    for v in releases.available_releases.iter().rev() {
        let mut tags = Vec::new();
        if releases.available_lts_releases.contains(v) {
            tags.push("LTS");
        }
        if *v == releases.most_recent_lts {
            tags.push("recommended");
        }
        let _ = writeln!(out, "    {v:>3}  {}", tags.join(", "));
    }
    Ok(out)
}

/// Renders the Android SDK platforms and build-tools of Google's repository.
fn available_android(config: &Config) -> Result<String> {
    let (platforms, build_tools) = android_sdk::Repository::fetch(config)?.available();
    let installed = android_sdk::installed(&config.android_sdk_dir());
    let mut out = String::from("Android SDK platforms (install with `kargo toolchain install android-sdk <api>`):\n");
    for api in platforms {
        let here = installed.iter().any(|p| p.path == android_sdk::platform_package(api));
        let _ = writeln!(out, "    {api:>3}{}", if here { "  installed" } else { "" });
    }
    let agp = crate::gradle::AGP_VERSION;
    let _ = writeln!(
        out,
        "\nBuild-tools (Kargo installs the one of the project's AGP: {} for AGP {agp}):",
        android_sdk::build_tools_for_agp(agp)
    );
    for version in build_tools.iter().take(10) {
        let _ = writeln!(out, "    {version}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(dir: &Path) -> Config {
        Config::new(dir.to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true)
    }

    fn fake_jdk(config: &Config, release: &str, major: u32) {
        let dir = config.jdks_dir().join(release);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("kargo-jdk.json"),
            format!(r#"{{"release": "{release}", "major": {major}, "vendor": "temurin", "home": ""}}"#),
        )
        .unwrap();
    }

    #[test]
    fn lists_installed_toolchains_and_marks_project_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        fake_jdk(&config, "jdk-21.0.1+12", 21);
        fake_jdk(&config, "jdk-21.0.9+10", 21);
        let konanc = config.native_dir().join("2.4.21/bin").join(if cfg!(windows) { "konanc.bat" } else { "konanc" });
        std::fs::create_dir_all(konanc.parent().unwrap()).unwrap();
        std::fs::write(&konanc, "").unwrap();

        let manifest = Manifest::parse(
            "[package]\nname = \"p\"\nversion = \"1\"\n[kotlin]\nversion = \"2.4.21\"\n[toolchain]\njdk = 21\n[target.native]\n[target.jvm]\n",
        )
        .unwrap();
        let text = list(&config, Some(&manifest)).unwrap();
        let used: Vec<&str> = text.lines().filter(|l| l.contains("used by this project")).collect();
        assert_eq!(used.len(), 2, "{text}");
        assert!(used[0].contains("jdk-21.0.9+10"), "newest 21 is used: {text}");
        assert!(used[1].contains("2.4.21"));
        let host = kargo_toolchain::native::Host::current().unwrap().konan_target;
        assert!(text.contains(&format!("dependencies for {host}: not downloaded")), "{text}");

        let text = list(&config, None).unwrap();
        assert!(!text.contains("used by this project"));
        assert!(text.contains("Gradle (") && text.contains("(none installed)"), "{text}");
    }

    #[test]
    fn manages_gradle_distributions() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        let gradle = config.gradle_dir().join("9.6.0/bin").join(if cfg!(windows) { "gradle.bat" } else { "gradle" });
        std::fs::create_dir_all(gradle.parent().unwrap()).unwrap();
        std::fs::write(&gradle, "").unwrap();

        let android = Manifest::parse("[package]\nname = \"p\"\nversion = \"1\"\n[target.android]\nnamespace = \"a.b\"\n").unwrap();
        let text = list(&config, Some(&android)).unwrap();
        let gradle_section = text.split("Gradle (").nth(1).unwrap().split("Android SDK (").next().unwrap();
        let used: Vec<&str> = gradle_section.lines().filter(|l| l.contains("used by this project")).collect();
        assert_eq!(used.len(), 1, "{text}");
        assert!(used[0].contains("9.6.0"), "the default for AGP {}: {text}", crate::gradle::AGP_VERSION);

        // Installed already: no download needed, even offline.
        assert!(install(&config, Some(&android), ToolKind::Gradle, None, Licenses::Ask).unwrap().starts_with("Gradle 9.6.0 at"));
        assert_eq!(install_for_project(&config, &android, Licenses::Ask).unwrap().len(), 1);
        assert!(format!("{:#}", install(&config, None, ToolKind::Gradle, Some("9.5.0"), Licenses::Ask).unwrap_err()).contains("offline"));

        let pinned = Manifest::parse(
            "[package]\nname = \"p\"\nversion = \"1\"\n[target.android]\nnamespace = \"a.b\"\nagp-version = \"9.4.1\"\ngradle-version = \"9.1.0\"\n",
        )
        .unwrap();
        let err = install(&config, Some(&pinned), ToolKind::Gradle, None, Licenses::Ask).unwrap_err().to_string();
        assert!(err.contains("needs Gradle 9.6.0 or newer"), "{err}");

        assert_eq!(remove(&config, ToolKind::Gradle, "9.6.0").unwrap(), ["Gradle 9.6.0"]);
        assert!(remove(&config, ToolKind::Gradle, "9.6.0").is_err());
    }

    #[test]
    fn manages_the_android_sdk() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        let sdk = config.android_sdk_dir();
        for path in ["platforms;android-36", "build-tools;36.0.0", "platform-tools", "platforms;android-35"] {
            let dir = android_sdk::package_dir(&sdk, path);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("package.xml"), "").unwrap();
        }
        let android = Manifest::parse(
            "[package]\nname = \"p\"\nversion = \"1\"\n[toolchain]\nandroid-sdk = \"36\"\n[target.android]\nnamespace = \"a.b\"\n",
        )
        .unwrap();
        let text = list(&config, Some(&android)).unwrap();
        let section = text.split("Android SDK (").nth(1).unwrap();
        let used: Vec<&str> = section.lines().filter(|l| l.contains("used by this project")).collect();
        assert_eq!(used.len(), 3, "the platform, build-tools and platform-tools: {text}");
        assert!(section.lines().any(|l| l.contains("platforms;android-35") && !l.contains("used by")), "{text}");

        // Installed already: no download, even offline.
        let done = install(&config, Some(&android), ToolKind::AndroidSdk, None, Licenses::Ask).unwrap();
        assert!(done.starts_with("Android SDK platform 36, build-tools 36.0.0 and platform-tools at"), "{done}");
        let gradle = config.gradle_dir().join("9.6.0/bin").join(if cfg!(windows) { "gradle.bat" } else { "gradle" });
        std::fs::create_dir_all(gradle.parent().unwrap()).unwrap();
        std::fs::write(&gradle, "").unwrap();
        assert!(install_for_project(&config, &android, Licenses::Ask).unwrap().iter().any(|d| d.starts_with("Android SDK platform 36")));
        let err = format!("{:#}", install(&config, None, ToolKind::AndroidSdk, Some("34"), Licenses::Ask).unwrap_err());
        assert!(err.contains("offline"), "{err}");
        let err = install(&config, None, ToolKind::AndroidSdk, None, Licenses::Ask).unwrap_err().to_string();
        assert!(err.contains("which Android platform"), "{err}");
        assert!(install(&config, None, ToolKind::AndroidSdk, Some("system"), Licenses::Ask).is_err());

        assert_eq!(remove(&config, ToolKind::AndroidSdk, "35").unwrap(), ["Android SDK package platforms;android-35"]);
        assert!(remove(&config, ToolKind::AndroidSdk, "35").is_err());
        assert!(available(&config, Some(ToolKind::Gradle)).is_err());
    }

    #[test]
    fn install_and_remove_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        let err = install(&config, None, ToolKind::Jdk, None, Licenses::Ask).unwrap_err().to_string();
        assert!(err.contains("which JDK"), "{err}");
        assert!(install(&config, None, ToolKind::Native, None, Licenses::Ask).is_err());
        assert!(install(&config, None, ToolKind::Jdk, Some("system"), Licenses::Ask).is_err());
        assert!(format!("{:#}", install(&config, None, ToolKind::Jdk, Some("21"), Licenses::Ask).unwrap_err()).contains("offline"));

        fake_jdk(&config, "jdk-17.0.9+9", 17);
        assert_eq!(remove(&config, ToolKind::Jdk, "17").unwrap(), ["JDK jdk-17.0.9+9"]);
        assert!(remove(&config, ToolKind::Native, "2.0.0").is_err());

        let plain = Manifest::parse("[package]\nname = \"p\"\nversion = \"1\"\n").unwrap();
        assert!(install_for_project(&config, &plain, Licenses::Ask).unwrap().is_empty(), "nothing to install for a system-JDK JVM project");
    }
}
