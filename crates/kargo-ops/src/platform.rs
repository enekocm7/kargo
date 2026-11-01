//! Cross-compilation platforms (`--platform`): which OS and CPU a build runs on.
//!
//! In the style of Go (`GOOS`/`GOARCH`) and Cargo (`--target`): `kargo build -t jvm
//! --platform linux-arm64` builds the same bytecode on any host, and `kargo package --format
//! jlink --platform linux-arm64,windows-x64` builds a runtime image for each platform from its own
//! Temurin JDK (Kargo downloads it; only its `jmods` are read, the host's `jlink` runs).
//!
//! Wasm and JS stay platform-independent; Android runs on a device (`--target-set device`); a
//! `native-image` or installer builds for the OS it runs on, so a foreign `--platform` for those
//! is a clear error naming what builds where.

use anyhow::{Result, bail};

use kargo_manifest::TargetPlatform;

/// The environment variable `kargo build --platform` reads when no flag is given.
pub const ENV: &str = "KARGO_PLATFORM";

/// Parses `--platform` values: each may hold one `os-arch` platform or several separated by commas
/// (`--platform linux-arm64,windows-x64`). Empty entries are ignored; duplicates are removed with
/// the first occurrence kept.
pub fn parse_values(values: &[String]) -> Result<Vec<TargetPlatform>> {
    let mut out = Vec::new();
    for value in values.iter().flat_map(|v| v.split(',')).map(str::trim).filter(|v| !v.is_empty()) {
        let platform: TargetPlatform = value.parse()?;
        if !out.contains(&platform) {
            out.push(platform);
        }
    }
    Ok(out)
}

/// The platforms a command builds for: `--platform` when given, else `KARGO_PLATFORM`, else
/// `[build] platforms` of `kargo.toml`, else the host. Each source accepts comma-separated lists.
pub fn resolve(flag: &[String], manifest: &[TargetPlatform], config_platform: Option<&str>) -> Result<Vec<TargetPlatform>> {
    if !flag.is_empty() {
        return parse_values(flag);
    }
    if let Some(env) = config_platform.filter(|v| !v.trim().is_empty()) {
        return parse_values(&[env.to_owned()]);
    }
    if !manifest.is_empty() {
        return Ok(manifest.to_vec());
    }
    Ok(Vec::new())
}

/// Reads `KARGO_PLATFORM` (comma-separated, like the flag). `None` when unset or blank.
pub fn from_env() -> Option<String> {
    std::env::var(ENV).ok().filter(|v| !v.trim().is_empty())
}

/// Fails when `platforms` names a combination `target` cannot be built for. Wasm and JS are
/// platform-independent (any `--platform` builds the same module); Android runs on a device, so it
/// takes no `--platform`; `native-image` and installers build for the host OS.
pub fn check_target_platforms(target: kargo_manifest::TargetKind, platforms: &[TargetPlatform], format: Option<&str>) -> Result<()> {
    if platforms.is_empty() {
        return Ok(());
    }
    match target {
        kargo_manifest::TargetKind::Jvm => {
            // Bytecode is portable; `jlink` cross-builds, `native-image`/installers do not.
            if let Some(format) = format {
                check_jvm_format_platforms(platforms, format)?;
            }
            Ok(())
        }
        kargo_manifest::TargetKind::Wasm | kargo_manifest::TargetKind::Js => Ok(()),
        kargo_manifest::TargetKind::Android => {
            bail!(
                "`--platform` selects the OS and CPU of a JVM, Wasm, JS or Native build; Android runs on a device, so it takes no `--platform`"
            )
        }
        kargo_manifest::TargetKind::Native => check_native_platforms(platforms),
    }
}

/// Fails when `platforms` contains a foreign platform for `--format native-image` or installer,
/// which build for the host OS.
fn check_jvm_format_platforms(platforms: &[TargetPlatform], format: &str) -> Result<()> {
    if !matches!(format, "native-image" | "installer") {
        return Ok(());
    }
    let host = TargetPlatform::host()?;
    let foreign = platforms.iter().find(|p| **p != host);
    match (format, foreign) {
        ("native-image", Some(p)) => bail!(
            "`--format native-image` builds a GraalVM executable for the OS it runs on, so it cannot build for {p} on {host}; run `kargo package --format native-image` on {p} instead"
        ),
        ("installer", Some(p)) => bail!(
            "`--format installer` builds a jpackage installer for the OS it runs on ({}), so it cannot build for {p} on {host}; run `kargo package --format installer` on {p} instead",
            installer_os(host)
        ),
        _ => Ok(()),
    }
}

/// The OS name `installer`'s error uses: Windows, macOS or Linux.
fn installer_os(host: TargetPlatform) -> &'static str {
    match host.os() {
        "windows" => "Windows (.msi)",
        "macos" => "macOS (.dmg)",
        _ => "Linux (.deb)",
    }
}

/// Fails when `platforms` names an OS/CPU Kotlin/Native has no target for, or one this host cannot
/// build (Apple targets need macOS).
fn check_native_platforms(platforms: &[TargetPlatform]) -> Result<()> {
    for platform in platforms {
        let Some(konan) = platform.konan_target() else {
            bail!(
                "`--platform {platform}` has no Kotlin/Native target (Kotlin/Native builds `linux_x64`, `linux_arm64`, `mingw_x64`, `macos_x64` and `macos_arm64`); list the ones to build in `[target.native] platforms` instead"
            )
        };
        if !kargo_toolchain::native::buildable_on_host(konan) {
            let host = TargetPlatform::host()?;
            bail!(
                "`--platform {platform}` builds `{konan}`, which needs a different host: Apple targets build on macOS with Xcode, so cannot build for {platform} on {host}"
            )
        }
    }
    Ok(())
}

/// The Konan targets of `[target.native] platforms` that `--platform` selects. Empty `--platform`
/// keeps every one; otherwise each platform must map to a Konan target in the manifest.
pub fn filter_native_platforms(manifest_platforms: &[String], platforms: &[TargetPlatform]) -> Result<Vec<String>> {
    if platforms.is_empty() {
        return Ok(manifest_platforms.to_vec());
    }
    let mut out = Vec::new();
    for platform in platforms {
        let Some(konan) = platform.konan_target() else {
            bail!(
                "`--platform {platform}` has no Kotlin/Native target (Kotlin/Native builds `linux_x64`, `linux_arm64`, `mingw_x64`, `macos_x64` and `macos_arm64`); list the ones to build in `[target.native] platforms` instead"
            )
        };
        if !manifest_platforms.iter().any(|p| p == konan) {
            bail!(
                "`--platform {platform}` builds `{konan}`, which is not in `[target.native] platforms` ({})",
                manifest_platforms.join(", ")
            )
        }
        if !out.contains(&konan.to_owned()) {
            out.push(konan.to_owned());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platforms(values: &[&str]) -> Vec<TargetPlatform> {
        parse_values(&values.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parses_comma_separated_platforms() {
        let out = platforms(&["linux-arm64, windows-x64", "linux-arm64"]);
        assert_eq!(out.iter().map(|p| p.as_str()).collect::<Vec<_>>(), ["linux-arm64", "windows-x64"]);
        assert!(parse_values(&["freebsd-x64".to_owned()]).is_err());
        // Aliases Go and Rust use.
        assert_eq!(platforms(&["linux-amd64"])[0].as_str(), "linux-x64");
        assert_eq!(platforms(&["linux-aarch64"])[0].as_str(), "linux-arm64");
    }

    #[test]
    fn resolves_flag_env_and_manifest_in_order() {
        let manifest = platforms(&["windows-x64"]);
        assert_eq!(resolve(&["linux-arm64".to_owned()], &manifest, Some("macos-arm64")).unwrap()[0].as_str(), "linux-arm64");
        assert_eq!(resolve(&[], &manifest, Some("macos-arm64")).unwrap()[0].as_str(), "macos-arm64");
        assert_eq!(resolve(&[], &manifest, None).unwrap(), manifest);
        assert!(resolve(&[], &[], None).unwrap().is_empty());
    }

    #[test]
    fn rejects_platforms_a_target_cannot_build() {
        let host = TargetPlatform::host().unwrap();
        let foreign = TargetPlatform::ALL.iter().copied().find(|p| *p != host).unwrap();
        // JVM bytecode is portable; only native-image/installer refuse a foreign platform.
        assert!(check_target_platforms(kargo_manifest::TargetKind::Jvm, &[foreign], None).is_ok());
        assert!(check_target_platforms(kargo_manifest::TargetKind::Jvm, &[foreign], Some("jlink")).is_ok());
        assert!(check_target_platforms(kargo_manifest::TargetKind::Jvm, &[foreign], Some("native-image")).is_err());
        assert!(check_target_platforms(kargo_manifest::TargetKind::Jvm, &[host], Some("installer")).is_ok());
        // Wasm and JS ignore the platform; Android takes none.
        assert!(check_target_platforms(kargo_manifest::TargetKind::Wasm, &[foreign], None).is_ok());
        assert!(check_target_platforms(kargo_manifest::TargetKind::Android, &[host], None).is_err());
    }

    #[test]
    fn native_platforms_must_exist_and_be_buildable() {
        // Windows/ARM64 has no Kotlin/Native target.
        let no_target = "windows-arm64".parse().unwrap();
        assert!(check_target_platforms(kargo_manifest::TargetKind::Native, &[no_target], None).is_err());
        // A platform the manifest does not list is an error, not a silent no-op.
        let all = ["linux_x64".to_owned(), "macos_arm64".to_owned()];
        assert!(filter_native_platforms(&all, &[]).unwrap() == all);
        let linux: TargetPlatform = "linux-x64".parse().unwrap();
        assert_eq!(filter_native_platforms(&all, &[linux]).unwrap(), ["linux_x64"]);
        let missing: TargetPlatform = "linux-arm64".parse().unwrap();
        assert!(filter_native_platforms(&all, &[missing]).is_err());
    }
}
