//! `kargo toolchain`: managing JDKs, GraalVMs, Kotlin/Native and Gradle distributions, and the
//! Kotlin language servers behind `kargo lsp`.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use kargo_manifest::{GraalvmRequest, JdkRequest, Manifest, TargetKind};
use kargo_maven::MavenClient;
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

/// Installs a toolchain. `version` defaults to what the project needs.
pub fn install(config: &Config, project: Option<&Manifest>, kind: ToolKind, version: Option<&str>) -> Result<String> {
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
        ToolKind::Server(kind) => {
            let server = LanguageServer::ensure(config, kind, version.unwrap_or(kind.default_version()))?;
            Ok(format!("{} {} at {}", kind.title(), server.version, server.home.display()))
        }
    }
}

/// Installs everything the project needs (`kargo toolchain install` without arguments).
pub fn install_for_project(config: &Config, manifest: &Manifest) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if manifest.toolchain.jdk != JdkRequest::System {
        done.push(install(config, Some(manifest), ToolKind::Jdk, None)?);
    } else {
        // The JDK a build would install on a machine without one.
        let needs = crate::ops::context::jdk_needs(manifest);
        if Jdk::find()?.is_none() && jdks::find_fallback(config, needs)?.is_none() {
            done.push(install(config, Some(manifest), ToolKind::Jdk, Some(&needs.choose().to_string()))?);
        }
    }
    if manifest.toolchain.graalvm != GraalvmRequest::System {
        done.push(install(config, Some(manifest), ToolKind::Graalvm, None)?);
    }
    if manifest.targets.is_enabled(TargetKind::Native) {
        done.push(install(config, Some(manifest), ToolKind::Native, None)?);
    }
    if project_gradle(manifest)?.is_some() {
        done.push(install(config, Some(manifest), ToolKind::Gradle, None)?);
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
        ToolKind::Server(kind) => {
            let removed = LanguageServer::remove(config, kind, selector)?;
            Ok(vec![format!("{} {}", kind.title(), removed.version)])
        }
    }
}

/// Renders installable JDK feature versions.
pub fn available(config: &Config) -> Result<String> {
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
        let gradle_section = text.split("Gradle (").nth(1).unwrap();
        let used: Vec<&str> = gradle_section.lines().filter(|l| l.contains("used by this project")).collect();
        assert_eq!(used.len(), 1, "{text}");
        assert!(used[0].contains("9.6.0"), "the default for AGP {}: {text}", crate::gradle::AGP_VERSION);

        // Installed already: no download needed, even offline.
        assert!(install(&config, Some(&android), ToolKind::Gradle, None).unwrap().starts_with("Gradle 9.6.0 at"));
        assert_eq!(install_for_project(&config, &android).unwrap().len(), 1);
        assert!(format!("{:#}", install(&config, None, ToolKind::Gradle, Some("9.5.0")).unwrap_err()).contains("offline"));

        let pinned = Manifest::parse(
            "[package]\nname = \"p\"\nversion = \"1\"\n[target.android]\nnamespace = \"a.b\"\nagp-version = \"9.4.1\"\ngradle-version = \"9.1.0\"\n",
        )
        .unwrap();
        let err = install(&config, Some(&pinned), ToolKind::Gradle, None).unwrap_err().to_string();
        assert!(err.contains("needs Gradle 9.6.0 or newer"), "{err}");

        assert_eq!(remove(&config, ToolKind::Gradle, "9.6.0").unwrap(), ["Gradle 9.6.0"]);
        assert!(remove(&config, ToolKind::Gradle, "9.6.0").is_err());
    }

    #[test]
    fn install_and_remove_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        let err = install(&config, None, ToolKind::Jdk, None).unwrap_err().to_string();
        assert!(err.contains("which JDK"), "{err}");
        assert!(install(&config, None, ToolKind::Native, None).is_err());
        assert!(install(&config, None, ToolKind::Jdk, Some("system")).is_err());
        assert!(format!("{:#}", install(&config, None, ToolKind::Jdk, Some("21")).unwrap_err()).contains("offline"));

        fake_jdk(&config, "jdk-17.0.9+9", 17);
        assert_eq!(remove(&config, ToolKind::Jdk, "17").unwrap(), ["JDK jdk-17.0.9+9"]);
        assert!(remove(&config, ToolKind::Native, "2.0.0").is_err());

        let plain = Manifest::parse("[package]\nname = \"p\"\nversion = \"1\"\n").unwrap();
        assert!(install_for_project(&config, &plain).unwrap().is_empty(), "nothing to install for a system-JDK JVM project");
    }
}
