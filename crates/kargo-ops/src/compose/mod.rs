//! Compose Desktop: what `[frameworks] compose` adds to running and shipping a JVM application.
//!
//! - Skiko, Compose's renderer, has one native library per OS and CPU, each in its own
//!   `skiko-awt-runtime-<os>-<arch>` jar. The build uses the host's; `kargo package --desktop
//!   --platforms all` merges every platform's into the jar, and Skiko loads the right one at runtime.
//! - Installers unpack the host's library next to the application (`-Dskiko.library.path=$APPDIR`)
//!   instead of leaving it in a jar, so macOS signs and notarizes it with the rest of the app.
//! - `kargo run` passes the application's name and icon to the macOS Dock.
//! - [`resources`]: `composeResources/` and the generated `Res` class.

pub mod resources;

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_manifest::Manifest;

/// The platforms Compose Desktop supports, as the `<os>-<arch>` suffixes of its artifacts.
pub const PLATFORMS: [&str; 6] = ["windows-x64", "windows-arm64", "macos-x64", "macos-arm64", "linux-x64", "linux-arm64"];

pub const SKIKO_GROUP: &str = "org.jetbrains.skiko";
const SKIKO_RUNTIME_PREFIX: &str = "skiko-awt-runtime-";

/// The JDK modules a Compose application needs besides the ones `jdeps` finds: AWT and Swing,
/// logging, and `sun.misc.Unsafe`, which Skiko and coroutines reach through reflection.
pub const RUNTIME_MODULES: [&str; 3] = ["java.desktop", "java.logging", "jdk.unsupported"];

/// The system property that makes `application {}` set up Swing like the Compose Gradle plugin's
/// run and packaged apps do (the system look and feel, the macOS menu bar).
pub const SWING_GLOBALS: &str = "-Dcompose.application.configure.swing.globals=true";

/// Whether the manifest enables Compose.
pub fn enabled(manifest: &Manifest) -> bool {
    manifest.frameworks.compose.is_some()
}

/// The platforms of `--platforms`: `all`, or a list such as `windows-x64,macos-arm64`.
pub fn parse_platforms(values: &[String]) -> Result<Vec<&'static str>> {
    let mut platforms: Vec<&'static str> = Vec::new();
    for value in values.iter().flat_map(|v| v.split(',')).map(str::trim).filter(|v| !v.is_empty()) {
        if value == "all" {
            platforms.extend(PLATFORMS);
            continue;
        }
        match PLATFORMS.iter().find(|p| **p == value) {
            Some(p) => platforms.push(p),
            None => bail!("unknown platform `{value}` for `--platforms` (expected `all` or some of: {})", PLATFORMS.join(", ")),
        }
    }
    platforms.sort_by_key(|p| PLATFORMS.iter().position(|q| q == p));
    platforms.dedup();
    Ok(platforms)
}

/// The Skiko version of the host's runtime jar among the resolved `modules`.
pub fn skiko_version(modules: &[(kargo_maven::ModuleId, String)]) -> Option<&str> {
    modules
        .iter()
        .find(|(m, _)| m.group == SKIKO_GROUP && m.artifact.starts_with(SKIKO_RUNTIME_PREFIX))
        .map(|(_, version)| version.as_str())
}

/// The artifact of Skiko's native library for `platform`.
pub fn skiko_runtime_artifact(platform: &str) -> String {
    format!("{SKIKO_RUNTIME_PREFIX}{platform}")
}

/// Whether `jar` is one of Skiko's native library jars (`skiko-awt-runtime-<os>-<arch>-<version>.jar`).
pub fn is_skiko_runtime_jar(jar: &Path) -> bool {
    jar.file_name().is_some_and(|n| n.to_string_lossy().starts_with(SKIKO_RUNTIME_PREFIX))
}

/// Unpacks the native library of a Skiko runtime jar (and, on Windows, its `icudtl.dat`) into
/// `dest`, where `-Dskiko.library.path` finds them. Returns the file names.
pub fn unpack_skiko_runtime(jar: &Path, dest: &Path) -> Result<Vec<String>> {
    let file = std::fs::File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("failed to read {}", jar.display()))?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        if entry.is_dir() || name.starts_with("META-INF/") || name.ends_with(".sha256") || name.contains('/') {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        std::fs::create_dir_all(dest)?;
        std::fs::write(dest.join(&name), data).with_context(|| format!("failed to unpack {name} from {}", jar.display()))?;
        names.push(name);
    }
    if names.is_empty() {
        bail!("{} has no native library", jar.display());
    }
    Ok(names)
}

/// The JVM options `kargo run` adds for a Compose application on `os`: [`SWING_GLOBALS`] and, on
/// macOS, the application's name and icon for the Dock and the menu bar (`[package.installer] name`
/// and `icon`). Windows and Linux take the window's title and `Window(icon = ...)` instead.
pub fn run_jvm_args(manifest: &Manifest, root: &Path, default_name: &str, os: &str) -> Vec<String> {
    let mut args = vec![SWING_GLOBALS.to_owned()];
    if os == "macos" {
        let installer = &manifest.packaging.installer;
        let name = installer.app_name(default_name);
        args.push(format!("-Xdock:name={name}"));
        args.push(format!("-Dapple.awt.application.name={name}"));
        // The Dock reads `.icns` and `.png` files.
        let icon: Option<PathBuf> = [installer.icon.macos.as_deref(), installer.icon.linux.as_deref()]
            .into_iter()
            .flatten()
            .map(|p| root.join(p))
            .find(|p| p.is_file());
        if let Some(icon) = icon {
            args.push(format!("-Xdock:icon={}", icon.display()));
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms() {
        assert_eq!(parse_platforms(&["all".into()]).unwrap(), PLATFORMS);
        assert_eq!(parse_platforms(&["linux-x64,windows-x64".into(), "linux-x64".into()]).unwrap(), ["windows-x64", "linux-x64"]);
        assert!(parse_platforms(&["freebsd-x64".into()]).unwrap_err().to_string().contains("unknown platform `freebsd-x64`"));
        assert!(PLATFORMS.contains(&crate::builtins::compose_desktop_classifier().unwrap_or("windows-x64")));
    }

    #[test]
    fn skiko_jars() {
        let modules = [
            (kargo_maven::ModuleId::new("org.jetbrains.skiko", "skiko-awt"), "0.150.1".to_owned()),
            (kargo_maven::ModuleId::new("org.jetbrains.skiko", "skiko-awt-runtime-windows-x64"), "0.150.1".to_owned()),
        ];
        assert_eq!(skiko_version(&modules), Some("0.150.1"));
        assert_eq!(skiko_version(&modules[..1]), None);
        assert_eq!(skiko_runtime_artifact("macos-arm64"), "skiko-awt-runtime-macos-arm64");
        assert!(is_skiko_runtime_jar(Path::new("/m2/skiko-awt-runtime-linux-x64-0.150.1.jar")));
        assert!(!is_skiko_runtime_jar(Path::new("/m2/skiko-awt-0.150.1.jar")));
    }

    #[test]
    fn unpacks_the_native_library() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let jar = tmp.path().join("skiko-awt-runtime-windows-x64-0.150.1.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        for (name, data) in
            [("META-INF/MANIFEST.MF", "x"), ("skiko-windows-x64.dll", "dll"), ("skiko-windows-x64.dll.sha256", "h"), ("icudtl.dat", "icu")]
        {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let dest = tmp.path().join("app");
        assert_eq!(unpack_skiko_runtime(&jar, &dest).unwrap(), ["skiko-windows-x64.dll", "icudtl.dat"]);
        assert_eq!(std::fs::read_to_string(dest.join("skiko-windows-x64.dll")).unwrap(), "dll");
        assert!(!dest.join("skiko-windows-x64.dll.sha256").exists() && !dest.join("META-INF").exists());
    }

    #[test]
    fn run_options() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("icons")).unwrap();
        std::fs::write(tmp.path().join("icons/app.png"), "").unwrap();
        let m = Manifest::parse(
            "[package]\nname = \"notes\"\nversion = \"1\"\n[package.installer]\nname = \"Notes\"\nicon = { macos = \"icons/missing.icns\", linux = \"icons/app.png\" }\n[frameworks]\ncompose = true\n",
        )
        .unwrap();
        assert_eq!(run_jvm_args(&m, tmp.path(), "notes", "windows"), [SWING_GLOBALS]);
        let mac = run_jvm_args(&m, tmp.path(), "notes", "macos");
        assert_eq!(mac[1..3], ["-Xdock:name=Notes", "-Dapple.awt.application.name=Notes"]);
        assert_eq!(mac[3], format!("-Xdock:icon={}", tmp.path().join("icons/app.png").display()));
    }
}
