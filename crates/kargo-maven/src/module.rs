//! Gradle Module Metadata (`.module` files) and variant selection, which is
//! how Kotlin Multiplatform libraries publish per-platform artifacts.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::coord::ModuleId;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleMetadata {
    pub format_version: String,
    #[serde(default)]
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Variant {
    pub name: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub dependencies: Vec<ModuleDependency>,
    #[serde(default, rename = "dependencyConstraints")]
    pub dependency_constraints: Vec<ModuleDependency>,
    #[serde(default)]
    pub files: Vec<VariantFile>,
    #[serde(rename = "available-at")]
    pub available_at: Option<AvailableAt>,
    /// Explicit capabilities; empty means the module's own coordinates.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Capability {
    pub group: String,
    pub name: String,
}

impl Variant {
    /// Whether this variant provides the module's default capability. Variants
    /// with other capabilities (e.g. kotlin-test's per-framework variants) are
    /// only selected when explicitly requested.
    fn provides_default_capability(&self, module: &ModuleId) -> bool {
        self.capabilities.is_empty() || self.capabilities.iter().any(|c| c.group == module.group && c.name == module.artifact)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModuleDependency {
    pub group: String,
    pub module: String,
    #[serde(default)]
    pub version: Option<RichVersion>,
    #[serde(default)]
    pub excludes: Vec<Exclude>,
    #[serde(default)]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

impl ModuleDependency {
    pub fn module_id(&self) -> ModuleId {
        ModuleId::new(&self.group, &self.module)
    }

    /// A dependency on a platform (BOM) rather than a library.
    pub fn is_platform(&self) -> bool {
        matches!(self.attributes.get("org.gradle.category").and_then(|v| v.as_str()), Some("platform" | "enforced-platform"))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RichVersion {
    pub requires: Option<String>,
    pub strictly: Option<String>,
    pub prefers: Option<String>,
    #[serde(default)]
    pub rejects: Vec<String>,
}

impl RichVersion {
    /// The most specific version requirement expressed.
    pub fn preferred(&self) -> Option<&str> {
        self.strictly.as_deref().or(self.requires.as_deref()).or(self.prefers.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Exclude {
    pub group: String,
    pub module: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VariantFile {
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AvailableAt {
    pub url: String,
    pub group: String,
    pub module: String,
    pub version: String,
}

impl ModuleMetadata {
    pub fn parse(json: &str) -> Result<Self> {
        let meta: ModuleMetadata = serde_json::from_str(json).context("malformed Gradle module metadata")?;
        if !meta.format_version.starts_with("1.") {
            anyhow::bail!("unsupported Gradle module metadata format {}", meta.format_version);
        }
        Ok(meta)
    }
}

/// The platform we resolve dependencies for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    Jvm,
    /// Kotlin/Wasm with the `js` or `wasi` environment.
    Wasm(String),
    /// Kotlin/JS with the IR compiler.
    Js,
    /// Kotlin/Native with a Konan target name such as `mingw_x64`.
    Native(String),
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Jvm => f.write_str("jvm"),
            Platform::Wasm(env) => write!(f, "wasm-{env}"),
            Platform::Js => f.write_str("js"),
            Platform::Native(target) => write!(f, "native-{target}"),
        }
    }
}

impl Platform {
    /// The file extension of library artifacts for this platform.
    pub fn library_extension(&self) -> &'static str {
        match self {
            Platform::Jvm => "jar",
            Platform::Wasm(_) | Platform::Js | Platform::Native(_) => "klib",
        }
    }
}

fn attr<'a>(variant: &'a Variant, key: &str) -> Option<&'a str> {
    variant.attributes.get(key).and_then(|v| v.as_str())
}

/// Scores a variant for the given platform; `None` means incompatible.
fn score(variant: &Variant, platform: &Platform) -> Option<u32> {
    let category = attr(variant, "org.gradle.category");
    if matches!(category, Some(c) if c != "library") {
        return None;
    }
    if attr(variant, "org.gradle.docstype").is_some() {
        return None;
    }
    let usage = attr(variant, "org.gradle.usage");
    let platform_type = attr(variant, "org.jetbrains.kotlin.platform.type");

    match platform {
        Platform::Jvm => {
            let usage_score = match usage {
                Some("java-runtime") => 3,
                Some("java-api") => 2,
                None => 1,
                Some(_) => return None,
            };
            let platform_score = match platform_type {
                Some("jvm") => 3,
                None if attr(variant, "org.gradle.jvm.environment") != Some("non-jvm") => 2,
                Some("androidJvm") => 1,
                _ => return None,
            };
            // Prefer standard JVM over Android variants.
            let env_score = match attr(variant, "org.gradle.jvm.environment") {
                Some("standard-jvm") | None => 1,
                _ => 0,
            };
            Some(platform_score * 100 + usage_score * 10 + env_score)
        }
        Platform::Wasm(env) => {
            if platform_type != Some("wasm") {
                return None;
            }
            if attr(variant, "org.jetbrains.kotlin.wasm.target").is_some_and(|t| t != env) {
                return None;
            }
            match usage {
                Some("kotlin-runtime") => Some(3),
                Some("kotlin-api") => Some(2),
                _ => None,
            }
        }
        Platform::Js => {
            if platform_type != Some("js") {
                return None;
            }
            // Libraries built for the old (pre-IR) compiler ship `.js` files Kotlin 2 cannot read.
            if attr(variant, "org.jetbrains.kotlin.js.compiler").is_some_and(|c| c != "ir") {
                return None;
            }
            match usage {
                Some("kotlin-runtime") => Some(3),
                Some("kotlin-api") => Some(2),
                _ => None,
            }
        }
        Platform::Native(target) => {
            if platform_type != Some("native") {
                return None;
            }
            // Variants without a target (e.g. the stdlib's, which ships with
            // the compiler) apply to every native target.
            let target_score = match attr(variant, "org.jetbrains.kotlin.native.target") {
                Some(t) if t == target => 2,
                None => 1,
                Some(_) => return None,
            };
            let usage_score = match usage {
                Some("kotlin-api") => 3,
                Some("kotlin-runtime") => 2,
                _ => return None,
            };
            Some(target_score * 10 + usage_score)
        }
    }
}

/// Picks the best variant of `module` for `platform`.
pub fn select_variant<'a>(meta: &'a ModuleMetadata, module: &ModuleId, platform: &Platform) -> Option<&'a Variant> {
    meta.variants
        .iter()
        .filter(|v| v.provides_default_capability(module))
        .filter_map(|v| score(v, platform).map(|s| (s, v)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KMP: &str = r#"{
      "formatVersion": "1.1",
      "component": {"group": "org.x", "module": "lib", "version": "1.0"},
      "variants": [
        {"name": "metadataApiElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "kotlin-metadata", "org.jetbrains.kotlin.platform.type": "common"}},
        {"name": "jvmApiElements-published", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-api", "org.jetbrains.kotlin.platform.type": "jvm"},
         "available-at": {"url": "../../lib-jvm/1.0/lib-jvm-1.0.module", "group": "org.x", "module": "lib-jvm", "version": "1.0"}},
        {"name": "jvmRuntimeElements-published", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "jvm"},
         "available-at": {"url": "../../lib-jvm/1.0/lib-jvm-1.0.module", "group": "org.x", "module": "lib-jvm", "version": "1.0"}},
        {"name": "androidRelease", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "androidJvm"}},
        {"name": "jvmSourcesElements", "attributes": {"org.gradle.category": "documentation", "org.gradle.docstype": "sources"}},
        {"name": "wasmJsApiElements", "attributes": {"org.gradle.usage": "kotlin-api", "org.jetbrains.kotlin.platform.type": "wasm", "org.jetbrains.kotlin.wasm.target": "js"},
         "files": [{"name": "lib-wasm-js.klib", "url": "lib-wasm-js-1.0.klib"}]},
        {"name": "jsLegacyRuntimeElements", "attributes": {"org.gradle.usage": "kotlin-runtime", "org.jetbrains.kotlin.platform.type": "js", "org.jetbrains.kotlin.js.compiler": "legacy"}},
        {"name": "jsIrApiElements", "attributes": {"org.gradle.usage": "kotlin-api", "org.jetbrains.kotlin.platform.type": "js", "org.jetbrains.kotlin.js.compiler": "ir"}},
        {"name": "jsIrRuntimeElements", "attributes": {"org.gradle.usage": "kotlin-runtime", "org.jetbrains.kotlin.platform.type": "js", "org.jetbrains.kotlin.js.compiler": "ir"}},
        {"name": "mingwX64ApiElements", "attributes": {"org.gradle.usage": "kotlin-api", "org.jetbrains.kotlin.platform.type": "native", "org.jetbrains.kotlin.native.target": "mingw_x64"}},
        {"name": "linuxX64ApiElements", "attributes": {"org.gradle.usage": "kotlin-api", "org.jetbrains.kotlin.platform.type": "native", "org.jetbrains.kotlin.native.target": "linux_x64"}}
      ]
    }"#;

    fn id() -> ModuleId {
        ModuleId::new("org.x", "lib")
    }

    #[test]
    fn skips_variants_with_foreign_capabilities() {
        let json = r#"{"formatVersion": "1.1", "variants": [
            {"name": "jvmRuntimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "jvm"},
             "files": [{"name": "lib.jar", "url": "lib.jar"}]},
            {"name": "jvmTestNGRuntimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "jvm"},
             "capabilities": [{"group": "org.x", "name": "lib-framework-testng", "version": "1"}]}
        ]}"#;
        let meta = ModuleMetadata::parse(json).unwrap();
        assert_eq!(select_variant(&meta, &id(), &Platform::Jvm).unwrap().name, "jvmRuntimeElements");
        let own = r#"{"formatVersion": "1.1", "variants": [
            {"name": "v", "attributes": {"org.gradle.usage": "java-runtime"}, "capabilities": [{"group": "org.x", "name": "lib", "version": "1"}]}
        ]}"#;
        assert!(select_variant(&ModuleMetadata::parse(own).unwrap(), &id(), &Platform::Jvm).is_some());
    }

    #[test]
    fn selects_jvm_runtime_variant() {
        let meta = ModuleMetadata::parse(KMP).unwrap();
        let v = select_variant(&meta, &id(), &Platform::Jvm).unwrap();
        assert_eq!(v.name, "jvmRuntimeElements-published");
        assert_eq!(v.available_at.as_ref().unwrap().module, "lib-jvm");
    }

    #[test]
    fn selects_wasm_and_native_variants() {
        let meta = ModuleMetadata::parse(KMP).unwrap();
        assert_eq!(select_variant(&meta, &id(), &Platform::Wasm("js".into())).unwrap().name, "wasmJsApiElements");
        assert!(select_variant(&meta, &id(), &Platform::Wasm("wasi".into())).is_none());
        assert_eq!(select_variant(&meta, &id(), &Platform::Js).unwrap().name, "jsIrRuntimeElements", "never the legacy compiler's");
        assert_eq!(select_variant(&meta, &id(), &Platform::Native("linux_x64".into())).unwrap().name, "linuxX64ApiElements");
        assert!(select_variant(&meta, &id(), &Platform::Native("macos_arm64".into())).is_none());
    }

    #[test]
    fn plain_java_library_variant() {
        let json = r#"{"formatVersion": "1.1", "variants": [
            {"name": "apiElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-api", "org.gradle.jvm.environment": "standard-jvm"}},
            {"name": "runtimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.gradle.jvm.environment": "standard-jvm"},
             "dependencies": [{"group": "a", "module": "b", "version": {"requires": "1.0", "strictly": "1.1"}},
                              {"group": "p", "module": "bom", "version": {"requires": "2"}, "attributes": {"org.gradle.category": "platform"}}],
             "files": [{"name": "x.jar", "url": "x-1.jar", "sha256": "abc"}]},
            {"name": "platform", "attributes": {"org.gradle.category": "platform"}}
        ]}"#;
        let meta = ModuleMetadata::parse(json).unwrap();
        let v = select_variant(&meta, &id(), &Platform::Jvm).unwrap();
        assert_eq!(v.name, "runtimeElements");
        assert_eq!(v.dependencies[0].version.as_ref().unwrap().preferred(), Some("1.1"));
        assert!(!v.dependencies[0].is_platform());
        assert!(v.dependencies[1].is_platform());
        assert_eq!(v.files[0].sha256.as_deref(), Some("abc"));
        assert!(select_variant(&meta, &id(), &Platform::Native("linux_x64".into())).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        assert!(ModuleMetadata::parse(r#"{"formatVersion": "2.0", "variants": []}"#).is_err());
        assert!(ModuleMetadata::parse("{").is_err());
    }
}
