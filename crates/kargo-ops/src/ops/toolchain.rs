//! `kargo toolchain`: managing JDKs, Kotlin/Native and Gradle distributions, and the Kotlin
//! language servers behind `kargo lsp`.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use kargo_manifest::{JdkRequest, Manifest, TargetKind};
use kargo_maven::MavenClient;
use kargo_toolchain::archive::{dir_size, human_size};
use kargo_toolchain::jdks::{self, ManagedJdk};
use kargo_toolchain::{GradleDistribution, Jdk, LanguageServer, NativeToolchain, ServerKind, adoptium};
use kargo_util::config::Config;

/// Kinds of toolchains Kargo manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Jdk,
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

    let _ = writeln!(out, "JDKs ({}):", config.jdks_dir().display());
    if installed.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for jdk in &installed {
        let used = jdk_request.as_ref().is_some_and(|r| is_selected(jdk, r, &installed));
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
    match Jdk::detect() {
        Ok(system) => {
            let used = matches!(jdk_request, Some(JdkRequest::System));
            let _ = writeln!(
                out,
                "    system: JDK {} at {}{}",
                system.version,
                system.home.display(),
                if used { "  (used by this project)" } else { "" }
            );
        }
        Err(_) => {
            let _ = writeln!(out, "    system: none found (JAVA_HOME / PATH)");
        }
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
Kotlin language servers ({}):",
        config.toolchains_dir().display()
    );
    if servers.is_empty() {
        let _ = writeln!(out, "    (none installed)");
    }
    for server in &servers {
        let pinned = server.version == server.kind.default_version();
        let _ = writeln!(
            out,
            "    {:<10} {:<11} {:>10}{}",
            server.kind.as_str(),
            server.version,
            human_size(dir_size(&server.home)),
            if pinned { "  (used by kargo lsp)" } else { "" }
        );
    }
    Ok(out)
}

/// Installs a toolchain. `version` defaults to what the project needs.
pub fn install(config: &Config, project: Option<&Manifest>, kind: ToolKind, version: Option<&str>) -> Result<String> {
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
        ToolKind::Native => {
            let version = match (version, project) {
                (Some(v), _) => v.to_owned(),
                (None, Some(m)) => m.kotlin.version.clone(),
                (None, None) => bail!("which version? e.g. `kargo toolchain install native 2.4.21`"),
            };
            let repos = project.map(|m| m.repositories.clone()).unwrap_or_else(|| vec![kargo_maven::Repository::maven_central()]);
            let client = MavenClient::new(config, repos);
            let toolchain = NativeToolchain::ensure(config, &client, &version)?;
            Ok(format!("Kotlin/Native {} at {}", toolchain.version, toolchain.home.display()))
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
