//! The files that let an Xcode project use an XCFramework Kargo built: a Swift package
//! (`Package.swift`) and a CocoaPods spec (`<name>.podspec`), written next to the
//! `<name>.xcframework` of `kargo build --xcframework --swift-package --cocoapods`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use kargo_manifest::Package;

/// An Apple OS family: its name in Swift, its `PackageDescription` version and its CocoaPods deployment target.
type Family = (&'static str, &'static str, &'static str);

/// The oldest OS each family is declared for. Kotlin/Native's own minimum is lower; these are the versions
/// Compose Multiplatform supports, so one choice suits plain and Compose frameworks.
const IOS: Family = ("iOS", "v14", "14.0");
const MACOS: Family = ("macOS", "v12", "12.0");
const TVOS: Family = ("tvOS", "v14", "14.0");
const WATCHOS: Family = ("watchOS", "v7", "7.0");

/// The OS families of Konan targets such as `ios_arm64` and `macos_arm64`, once each, in a fixed order.
fn families(konan_targets: &[&str]) -> Vec<Family> {
    [("ios_", IOS), ("macos_", MACOS), ("tvos_", TVOS), ("watchos_", WATCHOS)]
        .into_iter()
        .filter(|(prefix, _)| konan_targets.iter().any(|t| t.starts_with(prefix)))
        .map(|(_, family)| family)
        .collect()
}

/// The text of `Package.swift`: a library over the XCFramework at `xcframework`, relative to the package.
pub fn package_swift(name: &str, xcframework: &str, konan_targets: &[&str]) -> String {
    let platforms: String = families(konan_targets).iter().map(|(os, version, _)| format!("        .{os}(.{version}),\n")).collect();
    format!(
        "// swift-tools-version: 5.9\n\
         // Written by `kargo build --xcframework --swift-package`.\n\
         import PackageDescription\n\n\
         let package = Package(\n    \
             name: {name:?},\n    \
             platforms: [\n{platforms}    ],\n    \
             products: [\n        .library(name: {name:?}, targets: [{name:?}]),\n    ],\n    \
             targets: [\n        .binaryTarget(name: {name:?}, path: {xcframework:?}),\n    ]\n\
         )\n",
    )
}

/// A Ruby single-quoted string.
fn ruby(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The text of `<name>.podspec`: a pod that vendors the XCFramework at `xcframework`, relative to the spec. It is
/// meant for a `pod '<name>', :path => '...'` entry, so `source` is empty.
pub fn podspec(name: &str, xcframework: &str, package: &Package, konan_targets: &[&str]) -> String {
    let summary = package.description.clone().unwrap_or_else(|| format!("{} for Apple platforms, built with Kargo", package.name));
    let authors = if package.authors.is_empty() { package.name.clone() } else { package.authors.join(", ") };
    let mut spec = format!(
        "# Written by `kargo build --xcframework --cocoapods`.\n\
         Pod::Spec.new do |spec|\n    \
             spec.name                  = {name}\n    \
             spec.version               = {version}\n    \
             spec.summary               = {summary}\n    \
             spec.homepage              = {homepage}\n    \
             spec.license               = {{ :type => {license} }}\n    \
             spec.authors               = {authors}\n    \
             spec.source                = {{ :http => '' }}\n    \
             spec.vendored_frameworks   = {xcframework}\n    \
             spec.libraries             = 'c++'\n",
        name = ruby(name),
        version = ruby(&package.version),
        summary = ruby(&summary),
        homepage = ruby(package.homepage.as_deref().or(package.repository.as_deref()).unwrap_or_default()),
        license = ruby(package.license.as_deref().unwrap_or("Proprietary")),
        authors = ruby(&authors),
        xcframework = ruby(xcframework),
    );
    for (os, _, target) in families(konan_targets) {
        spec.push_str(&format!("    spec.{}.deployment_target = {}\n", os.to_ascii_lowercase(), ruby(target)));
    }
    spec.push_str("end\n");
    spec
}

/// Which files to write.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppleFiles {
    pub swift_package: bool,
    pub cocoapods: bool,
}

/// Writes the requested files next to `xcframework` (`<dir>/<name>.xcframework`) and returns their paths.
pub fn write(xcframework: &Path, package: &Package, konan_targets: &[&str], files: AppleFiles) -> Result<Vec<PathBuf>> {
    let dir = xcframework.parent().context("an XCFramework has a directory")?;
    let file_name = xcframework.file_name().context("an XCFramework has a name")?.to_string_lossy().into_owned();
    let name = file_name.strip_suffix(".xcframework").unwrap_or(&file_name);
    let mut written = Vec::new();
    let mut put = |file: String, text: String| -> Result<()> {
        let path = dir.join(file);
        std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
        Ok(())
    };
    if files.swift_package {
        put("Package.swift".to_owned(), package_swift(name, &file_name, konan_targets))?;
    }
    if files.cocoapods {
        put(format!("{name}.podspec"), podspec(name, &file_name, package, konan_targets))?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        kargo_manifest::Manifest::parse(
            "[package]\nname = \"shared\"\nversion = \"1.2.0\"\nlicense = \"MIT\"\nauthors = [\"Ada\", \"Bob\"]\n\
             description = \"It's shared\"\nhomepage = \"https://example.com/shared\"\n",
        )
        .unwrap()
        .package
    }

    #[test]
    fn a_swift_package_wraps_the_xcframework() {
        let text = package_swift("shared", "shared.xcframework", &["ios_arm64", "ios_simulator_arm64", "macos_arm64"]);
        assert!(text.starts_with("// swift-tools-version: 5.9\n"), "{text}");
        assert!(text.contains("name: \"shared\",\n"), "{text}");
        assert!(text.contains("        .iOS(.v14),\n        .macOS(.v12),\n    ],"), "{text}");
        assert!(!text.contains("tvOS") && !text.contains("watchOS"), "{text}");
        assert!(text.contains(".library(name: \"shared\", targets: [\"shared\"])"), "{text}");
        assert!(text.contains(".binaryTarget(name: \"shared\", path: \"shared.xcframework\")"), "{text}");
    }

    #[test]
    fn a_podspec_vendors_the_xcframework() {
        let text = podspec("shared", "shared.xcframework", &package(), &["ios_arm64", "tvos_arm64"]);
        assert!(text.contains("spec.name                  = 'shared'\n"), "{text}");
        assert!(text.contains("spec.version               = '1.2.0'\n"), "{text}");
        assert!(text.contains("spec.summary               = 'It\\'s shared'\n"), "{text}");
        assert!(text.contains("spec.homepage              = 'https://example.com/shared'\n"), "{text}");
        assert!(text.contains("{ :type => 'MIT' }") && text.contains("spec.authors               = 'Ada, Bob'\n"), "{text}");
        assert!(text.contains("spec.vendored_frameworks   = 'shared.xcframework'\n"), "{text}");
        assert!(text.contains("spec.ios.deployment_target = '14.0'\n    spec.tvos.deployment_target = '14.0'\nend\n"), "{text}");
    }

    #[test]
    fn the_files_go_next_to_the_xcframework() {
        let dir = tempfile::tempdir().unwrap();
        let xcframework = dir.path().join("shared.xcframework");
        let both = AppleFiles { swift_package: true, cocoapods: true };
        let written = write(&xcframework, &package(), &["ios_arm64"], both).unwrap();
        assert_eq!(written, [dir.path().join("Package.swift"), dir.path().join("shared.podspec")]);
        assert!(std::fs::read_to_string(&written[1]).unwrap().contains("spec.vendored_frameworks   = 'shared.xcframework'"));
        let only_swift = write(&xcframework, &package(), &["ios_arm64"], AppleFiles { swift_package: true, cocoapods: false }).unwrap();
        assert_eq!(only_swift.len(), 1);
    }
}
