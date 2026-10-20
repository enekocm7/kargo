//! What `kargo publish` uploads: the Maven modules of a package, their POMs and their Gradle
//! module metadata (`.module`), laid out like a Maven repository.
//!
//! A package with only the JVM target is one plain Maven module: `<name>.jar` with a POM.
//! Any other set of targets is published the way the Kotlin Gradle Plugin publishes a
//! multiplatform library, so Gradle and Kargo consumers pick the right artifact per target:
//!
//! - a root module `<name>` whose `.module` points every platform to its own module
//!   (`available-at`), with the common sources;
//! - one module per platform, `<name>-jvm`, `<name>-wasm-js`, `<name>-linuxx64`, ..., holding
//!   the jar or klib, its sources and javadoc, its POM and its `.module`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use kargo_manifest::Package;
use kargo_maven::deploy::escape;
use kargo_maven::pom::{GRADLE_METADATA_MARKER, MAIN_CLASS_PROPERTY};
use kargo_maven::{Artifact, ModuleId};
use kargo_util::hash;

/// A platform a module is published for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformKind {
    /// The JVM, with the bytecode version (`8`, `17`, ...) the classes target.
    Jvm { jvm_version: Option<u32> },
    /// Kotlin/Wasm for the `js` or `wasi` environment.
    Wasm { environment: String },
    /// Kotlin/Native for a Konan target such as `linux_x64`.
    Native { konan_target: String },
}

impl PlatformKind {
    /// The suffix of the platform's module, as the Kotlin Gradle Plugin names it: `jvm`,
    /// `wasm-js`, `wasm-wasi`, `linuxx64`, `iossimulatorarm64`.
    pub fn module_suffix(&self) -> String {
        match self {
            PlatformKind::Jvm { .. } => "jvm".to_owned(),
            PlatformKind::Wasm { environment } => format!("wasm-{environment}"),
            PlatformKind::Native { konan_target } => konan_target.replace('_', "").to_ascii_lowercase(),
        }
    }

    /// The Kotlin target name the variants are named after: `jvm`, `wasmJs`, `linuxX64`.
    fn target_name(&self) -> String {
        match self {
            PlatformKind::Jvm { .. } => "jvm".to_owned(),
            PlatformKind::Wasm { environment } => format!("wasm{}", capitalize(environment)),
            PlatformKind::Native { konan_target } => {
                let mut parts = konan_target.split('_');
                let first = parts.next().unwrap_or_default().to_owned();
                parts.fold(first, |acc, part| acc + &capitalize(part))
            }
        }
    }

    /// The extension of the main artifact.
    pub fn extension(&self) -> &'static str {
        match self {
            PlatformKind::Jvm { .. } => "jar",
            PlatformKind::Wasm { .. } | PlatformKind::Native { .. } => "klib",
        }
    }

    /// The Gradle attributes every variant of this platform carries, besides usage and category.
    fn attributes(&self) -> serde_json::Map<String, Value> {
        let mut attrs = serde_json::Map::new();
        match self {
            PlatformKind::Jvm { jvm_version } => {
                attrs.insert("org.gradle.jvm.environment".into(), json!("standard-jvm"));
                if let Some(version) = jvm_version {
                    attrs.insert("org.gradle.jvm.version".into(), json!(version));
                }
                attrs.insert("org.gradle.libraryelements".into(), json!("jar"));
                attrs.insert("org.jetbrains.kotlin.platform.type".into(), json!("jvm"));
            }
            PlatformKind::Wasm { environment } => {
                attrs.insert("org.gradle.jvm.environment".into(), json!("non-jvm"));
                attrs.insert("org.jetbrains.kotlin.platform.type".into(), json!("wasm"));
                attrs.insert("org.jetbrains.kotlin.wasm.target".into(), json!(environment));
            }
            PlatformKind::Native { konan_target } => {
                attrs.insert("org.gradle.jvm.environment".into(), json!("non-jvm"));
                attrs.insert("org.jetbrains.kotlin.native.target".into(), json!(konan_target));
                attrs.insert("org.jetbrains.kotlin.platform.type".into(), json!("native"));
            }
        }
        attrs
    }

    /// The usages of the platform's variants, `(variant suffix, org.gradle.usage, includes runtime dependencies)`.
    fn usages(&self) -> Vec<(&'static str, &'static str, bool)> {
        match self {
            PlatformKind::Jvm { .. } => vec![("ApiElements", "java-api", false), ("RuntimeElements", "java-runtime", true)],
            PlatformKind::Wasm { .. } => vec![("ApiElements", "kotlin-api", false), ("RuntimeElements", "kotlin-runtime", true)],
            // A klib is linked with everything it uses, so the API variant carries all dependencies.
            PlatformKind::Native { .. } => vec![("ApiElements", "kotlin-api", true)],
        }
    }

    fn sources_usage(&self) -> &'static str {
        match self {
            PlatformKind::Jvm { .. } => "java-runtime",
            _ => "kotlin-runtime",
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Which classpaths of a consumer a published dependency is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyScope {
    /// Compile and runtime (`compile` in the POM, `api` and `runtime` variants).
    Compile,
    /// Runtime only (`runtime` in the POM, `runtime` variants).
    Runtime,
}

/// One dependency of a published module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedDependency {
    /// The module as written in `.module` files: a multiplatform library's root module.
    pub module: ModuleId,
    /// The module as written in the POM, where Maven cannot pick a platform: a multiplatform
    /// library's platform module (`kotlinx-coroutines-core-jvm`).
    pub pom_module: ModuleId,
    pub version: String,
    pub scope: DependencyScope,
    pub classifier: Option<String>,
    pub extension: Option<String>,
    pub exclusions: Vec<ModuleId>,
    /// `false` excludes everything the dependency brings.
    pub transitive: bool,
    /// A BOM (Maven) or platform (Gradle): versions to align, not a library.
    pub bom: bool,
}

/// The publication of one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPublication {
    pub kind: PlatformKind,
    /// The jar or klib.
    pub artifact: PathBuf,
    /// More klibs published with the artifact under a classifier, such as `cinterop-sqlite`.
    pub extra: Vec<(String, PathBuf)>,
    pub sources: Option<PathBuf>,
    pub javadoc: Option<PathBuf>,
    pub dependencies: Vec<PublishedDependency>,
    /// The main class of a JVM application. Its POM names it in the `kargo.main-class` property, so
    /// `kargo install group:artifact` knows what to run.
    pub main_class: Option<String>,
}

/// Everything published for one package.
#[derive(Debug, Clone)]
pub struct Publication<'a> {
    pub group: String,
    pub package: &'a Package,
    pub platforms: Vec<PlatformPublication>,
    /// Sources of `src/` for the root module of a multiplatform library.
    pub common_sources: Option<PathBuf>,
    /// Documentation for the root module of a multiplatform library.
    pub common_javadoc: Option<PathBuf>,
}

/// A module written into the staging repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedModule {
    pub module: ModuleId,
    pub version: String,
    /// The version directory inside the staging repository.
    pub dir: PathBuf,
    /// File names inside `dir`, artifacts first and metadata (`.module`, `.pom`) last.
    pub files: Vec<String>,
}

/// Whether a package with these platforms is published as a multiplatform library
/// (a root module plus one module per platform) rather than one plain JVM module.
pub fn is_multiplatform(platforms: &[PlatformKind]) -> bool {
    !matches!(platforms, [PlatformKind::Jvm { .. }])
}

/// The coordinates under which a package is published for `kind`: its own name for a
/// JVM-only library, `<name>-<suffix>` for a platform of a multiplatform one.
pub fn platform_module(group: &str, name: &str, kind: &PlatformKind, multiplatform: bool) -> ModuleId {
    if multiplatform { ModuleId::new(group, format!("{name}-{}", kind.module_suffix())) } else { ModuleId::new(group, name) }
}

impl Publication<'_> {
    pub fn version(&self) -> &str {
        &self.package.version
    }

    fn root(&self) -> ModuleId {
        ModuleId::new(&self.group, &self.package.name)
    }

    fn multiplatform(&self) -> bool {
        is_multiplatform(&self.platforms.iter().map(|p| p.kind.clone()).collect::<Vec<_>>())
    }

    /// The module that holds `platform`'s artifact.
    pub fn platform_module(&self, platform: &PlatformPublication) -> ModuleId {
        platform_module(&self.group, &self.package.name, &platform.kind, self.multiplatform())
    }

    /// Writes every module into `repo` (a Maven repository layout) and returns them, the root
    /// module last so a repository never lists a root whose platforms are missing.
    pub fn stage(&self, repo: &Path) -> Result<Vec<StagedModule>> {
        let version = self.version();
        let mut staged = Vec::new();
        for platform in &self.platforms {
            let module = self.platform_module(platform);
            let mut writer = ModuleWriter::new(repo, &module, version)?;
            let main = writer.copy(None, platform.kind.extension(), &platform.artifact)?;
            let extra =
                platform.extra.iter().map(|(classifier, path)| writer.copy(Some(classifier), "klib", path)).collect::<Result<Vec<_>>>()?;
            let sources = platform.sources.as_ref().map(|p| writer.copy(Some("sources"), "jar", p)).transpose()?;
            if let Some(javadoc) = &platform.javadoc {
                writer.copy(Some("javadoc"), "jar", javadoc)?;
            }
            if self.multiplatform() {
                let mut files = vec![main];
                files.extend(extra);
                let metadata = self.platform_metadata(platform, &module, files, sources);
                writer.write(None, "module", &metadata)?;
            }
            let packaging = (platform.kind.extension() != "jar").then_some(platform.kind.extension());
            let pom = self.pom(&module, packaging, &platform.dependencies, platform.main_class.as_deref(), self.multiplatform());
            writer.write(None, "pom", &pom)?;
            staged.push(writer.finish());
        }
        if self.multiplatform() {
            let root = self.root();
            let mut writer = ModuleWriter::new(repo, &root, version)?;
            let sources = self.common_sources.as_ref().map(|p| writer.copy(Some("sources"), "jar", p)).transpose()?;
            if let Some(javadoc) = &self.common_javadoc {
                writer.copy(Some("javadoc"), "jar", javadoc)?;
            }
            writer.write(None, "module", &self.root_metadata(sources))?;
            writer.write(None, "pom", &self.pom(&root, Some("pom"), &[], None, true))?;
            staged.push(writer.finish());
        }
        Ok(staged)
    }

    fn component(&self, module: &ModuleId, url: Option<String>) -> Value {
        let mut component = serde_json::Map::new();
        if let Some(url) = url {
            component.insert("url".into(), json!(url));
        }
        component.insert("group".into(), json!(module.group));
        component.insert("module".into(), json!(module.artifact));
        component.insert("version".into(), json!(self.version()));
        component.insert("attributes".into(), json!({ "org.gradle.status": status(self.version()) }));
        Value::Object(component)
    }

    /// The root module's `.module`: each platform's variants, pointing at the platform's module.
    fn root_metadata(&self, sources: Option<FileEntry>) -> String {
        let version = self.version();
        let mut variants = Vec::new();
        for platform in &self.platforms {
            let module = self.platform_module(platform);
            let target = platform.kind.target_name();
            for (suffix, usage, _) in platform.kind.usages() {
                let mut attributes = platform.kind.attributes();
                attributes.insert("org.gradle.category".into(), json!("library"));
                attributes.insert("org.gradle.usage".into(), json!(usage));
                variants.push(json!({
                    "name": format!("{target}{suffix}-published"),
                    "attributes": attributes,
                    "available-at": {
                        "url": format!("../../{}/{version}/{}-{version}.module", module.artifact, module.artifact),
                        "group": module.group,
                        "module": module.artifact,
                        "version": version,
                    },
                }));
            }
        }
        if let Some(sources) = sources {
            variants.push(json!({
                "name": "metadataSourcesElements",
                "attributes": {
                    "org.gradle.category": "documentation",
                    "org.gradle.dependency.bundling": "external",
                    "org.gradle.docstype": "sources",
                    "org.gradle.usage": "kotlin-runtime",
                    "org.jetbrains.kotlin.platform.type": "common",
                },
                "files": [sources.to_json()],
            }));
        }
        module_json(self.component(&self.root(), None), variants)
    }

    /// A platform module's `.module`: its API and runtime variants with their files and dependencies.
    fn platform_metadata(
        &self,
        platform: &PlatformPublication,
        module: &ModuleId,
        files: Vec<FileEntry>,
        sources: Option<FileEntry>,
    ) -> String {
        let version = self.version();
        let root = self.root();
        let target = platform.kind.target_name();
        let mut variants = Vec::new();
        for (suffix, usage, runtime) in platform.kind.usages() {
            let mut attributes = platform.kind.attributes();
            attributes.insert("org.gradle.category".into(), json!("library"));
            attributes.insert("org.gradle.usage".into(), json!(usage));
            let dependencies: Vec<Value> =
                platform.dependencies.iter().filter(|d| runtime || d.scope == DependencyScope::Compile).map(module_dependency).collect();
            let mut variant = serde_json::Map::new();
            variant.insert("name".into(), json!(format!("{target}{suffix}-published")));
            variant.insert("attributes".into(), Value::Object(attributes));
            if !dependencies.is_empty() {
                variant.insert("dependencies".into(), json!(dependencies));
            }
            variant.insert("files".into(), json!(files.iter().map(FileEntry::to_json).collect::<Vec<_>>()));
            variants.push(Value::Object(variant));
        }
        if let Some(sources) = sources {
            let mut attributes = platform.kind.attributes();
            attributes.retain(|k, _| k == "org.jetbrains.kotlin.platform.type");
            attributes.insert("org.gradle.category".into(), json!("documentation"));
            attributes.insert("org.gradle.dependency.bundling".into(), json!("external"));
            attributes.insert("org.gradle.docstype".into(), json!("sources"));
            attributes.insert("org.gradle.usage".into(), json!(platform.kind.sources_usage()));
            variants.push(json!({
                "name": format!("{target}SourcesElements-published"),
                "attributes": attributes,
                "files": [sources.to_json()],
            }));
        }
        let url = format!("../../{}/{version}/{}-{version}.module", root.artifact, root.artifact);
        module_json(self.component(module, Some(url)), variants)
    }

    /// The POM of `module`. With `gradle_metadata`, it carries the marker that tells Gradle (and
    /// Kargo) to read the `.module` next to it instead.
    fn pom(
        &self,
        module: &ModuleId,
        packaging: Option<&str>,
        dependencies: &[PublishedDependency],
        main_class: Option<&str>,
        gradle_metadata: bool,
    ) -> String {
        let pkg = self.package;
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        x.push_str(
            "<project xmlns=\"http://maven.apache.org/POM/4.0.0\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd\">\n",
        );
        if gradle_metadata {
            x.push_str("  <!-- This module is also published with Gradle module metadata, which consumers should prefer. -->\n");
            x.push_str(&format!("  <!-- {GRADLE_METADATA_MARKER} -->\n"));
        }
        x.push_str("  <modelVersion>4.0.0</modelVersion>\n");
        element(&mut x, 1, "groupId", &module.group);
        element(&mut x, 1, "artifactId", &module.artifact);
        element(&mut x, 1, "version", self.version());
        if let Some(packaging) = packaging {
            element(&mut x, 1, "packaging", packaging);
        }
        element(&mut x, 1, "name", &pkg.name);
        if let Some(description) = &pkg.description {
            element(&mut x, 1, "description", description);
        }
        if let Some(url) = pkg.homepage.as_ref().or(pkg.repository.as_ref()) {
            element(&mut x, 1, "url", url);
        }
        if let Some(main_class) = main_class {
            x.push_str("  <properties>\n");
            element(&mut x, 2, MAIN_CLASS_PROPERTY, main_class);
            x.push_str("  </properties>\n");
        }
        let licenses = licenses(pkg.license.as_deref());
        if !licenses.is_empty() {
            x.push_str("  <licenses>\n");
            for id in licenses {
                x.push_str("    <license>\n");
                element(&mut x, 3, "name", &id);
                element(&mut x, 3, "url", &format!("https://spdx.org/licenses/{id}.html"));
                x.push_str("    </license>\n");
            }
            x.push_str("  </licenses>\n");
        }
        if !pkg.authors.is_empty() {
            x.push_str("  <developers>\n");
            for author in &pkg.authors {
                let (name, email) = developer(author);
                x.push_str("    <developer>\n");
                element(&mut x, 3, "name", name);
                if let Some(email) = email {
                    element(&mut x, 3, "email", email);
                }
                x.push_str("    </developer>\n");
            }
            x.push_str("  </developers>\n");
        }
        if let Some(repository) = &pkg.repository {
            x.push_str("  <scm>\n");
            element(&mut x, 2, "connection", &format!("scm:git:{repository}"));
            element(&mut x, 2, "developerConnection", &format!("scm:git:{repository}"));
            element(&mut x, 2, "url", repository.trim_end_matches(".git"));
            x.push_str("  </scm>\n");
        }
        let (boms, libraries): (Vec<_>, Vec<_>) = dependencies.iter().partition(|d| d.bom);
        if !boms.is_empty() {
            x.push_str("  <dependencyManagement>\n    <dependencies>\n");
            for bom in boms {
                x.push_str("      <dependency>\n");
                element(&mut x, 4, "groupId", &bom.pom_module.group);
                element(&mut x, 4, "artifactId", &bom.pom_module.artifact);
                element(&mut x, 4, "version", &bom.version);
                element(&mut x, 4, "type", "pom");
                element(&mut x, 4, "scope", "import");
                x.push_str("      </dependency>\n");
            }
            x.push_str("    </dependencies>\n  </dependencyManagement>\n");
        }
        if !libraries.is_empty() {
            x.push_str("  <dependencies>\n");
            for dep in libraries {
                x.push_str("    <dependency>\n");
                element(&mut x, 3, "groupId", &dep.pom_module.group);
                element(&mut x, 3, "artifactId", &dep.pom_module.artifact);
                element(&mut x, 3, "version", &dep.version);
                if let Some(classifier) = &dep.classifier {
                    element(&mut x, 3, "classifier", classifier);
                }
                if let Some(extension) = dep.extension.as_deref().filter(|e| *e != "jar") {
                    element(&mut x, 3, "type", extension);
                }
                element(&mut x, 3, "scope", if dep.scope == DependencyScope::Compile { "compile" } else { "runtime" });
                let exclusions: Vec<(&str, &str)> = if dep.transitive {
                    dep.exclusions.iter().map(|e| (e.group.as_str(), e.artifact.as_str())).collect()
                } else {
                    vec![("*", "*")]
                };
                if !exclusions.is_empty() {
                    x.push_str("      <exclusions>\n");
                    for (group, artifact) in exclusions {
                        x.push_str("        <exclusion>\n");
                        element(&mut x, 5, "groupId", group);
                        element(&mut x, 5, "artifactId", artifact);
                        x.push_str("        </exclusion>\n");
                    }
                    x.push_str("      </exclusions>\n");
                }
                x.push_str("    </dependency>\n");
            }
            x.push_str("  </dependencies>\n");
        }
        x.push_str("</project>\n");
        x
    }
}

/// `org.gradle.status`: `integration` for a SNAPSHOT, else `release`.
fn status(version: &str) -> &'static str {
    if kargo_maven::coord::is_snapshot(version) { "integration" } else { "release" }
}

/// A `.module` dependency entry.
fn module_dependency(dep: &PublishedDependency) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("group".into(), json!(dep.module.group));
    out.insert("module".into(), json!(dep.module.artifact));
    out.insert("version".into(), json!({ "requires": dep.version }));
    let excludes: Vec<Value> = if dep.transitive {
        dep.exclusions.iter().map(|e| json!({ "group": e.group, "module": e.artifact })).collect()
    } else {
        vec![json!({ "group": "*", "module": "*" })]
    };
    if !excludes.is_empty() {
        out.insert("excludes".into(), json!(excludes));
    }
    if dep.bom {
        out.insert("attributes".into(), json!({ "org.gradle.category": "platform" }));
    }
    if dep.classifier.is_some() || dep.extension.is_some() {
        let extension = dep.extension.clone().unwrap_or_else(|| "jar".to_owned());
        let mut selector = serde_json::Map::new();
        selector.insert("name".into(), json!(dep.module.artifact));
        selector.insert("type".into(), json!(extension));
        selector.insert("extension".into(), json!(extension));
        if let Some(classifier) = &dep.classifier {
            selector.insert("classifier".into(), json!(classifier));
        }
        out.insert("thirdPartyCompatibility".into(), json!({ "artifactSelector": selector }));
    }
    Value::Object(out)
}

/// A `.module` document. Gradle requires `formatVersion` to be the first key, so the top
/// level is written by hand (`serde_json` sorts object keys).
fn module_json(component: Value, variants: Vec<Value>) -> String {
    let indent = |value: &Value| serde_json::to_string_pretty(value).expect("JSON values serialize").replace('\n', "\n  ");
    format!(
        "{{\n  \"formatVersion\": \"1.1\",\n  \"component\": {},\n  \"variants\": {}\n}}\n",
        indent(&component),
        indent(&json!(variants))
    )
}

/// The SPDX license identifiers of a license expression: `MIT OR Apache-2.0` gives both.
pub fn licenses(expression: Option<&str>) -> Vec<String> {
    let Some(expression) = expression else { return Vec::new() };
    let tokens: Vec<&str> = expression.split(|c: char| c.is_whitespace() || c == '(' || c == ')').filter(|t| !t.is_empty()).collect();
    let mut out: Vec<String> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let operator = matches!(token.to_ascii_uppercase().as_str(), "OR" | "AND" | "WITH");
        let exception = i > 0 && tokens[i - 1].eq_ignore_ascii_case("WITH");
        if !operator && !exception && !out.iter().any(|l| l == token) {
            out.push((*token).to_owned());
        }
    }
    out
}

/// Splits an author such as `Ada Lovelace <ada@example.com>` into name and email.
pub fn developer(author: &str) -> (&str, Option<&str>) {
    match author.split_once('<') {
        Some((name, rest)) => {
            let email = rest.split_once('>').map(|(e, _)| e).unwrap_or(rest).trim();
            (name.trim(), (!email.is_empty()).then_some(email))
        }
        None => (author.trim(), None),
    }
}

fn element(out: &mut String, depth: usize, name: &str, text: &str) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&format!("<{name}>{}</{name}>\n", escape(text)));
}

/// A file listed in a `.module` variant.
#[derive(Debug, Clone)]
struct FileEntry {
    name: String,
    size: u64,
    sums: hash::Checksums,
}

impl FileEntry {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "url": self.name,
            "size": self.size,
            "sha512": self.sums.sha512,
            "sha256": self.sums.sha256,
            "sha1": self.sums.sha1,
            "md5": self.sums.md5,
        })
    }
}

/// Writes the files of one module into its version directory of the staging repository.
struct ModuleWriter {
    module: ModuleId,
    version: String,
    dir: PathBuf,
    files: Vec<String>,
}

impl ModuleWriter {
    fn new(repo: &Path, module: &ModuleId, version: &str) -> Result<Self> {
        let dir = module.repo_dir().split('/').fold(repo.to_path_buf(), |p, seg| p.join(seg)).join(version);
        std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        Ok(Self { module: module.clone(), version: version.to_owned(), dir, files: Vec::new() })
    }

    fn file_name(&self, classifier: Option<&str>, extension: &str) -> String {
        Artifact::new(self.module.clone(), &self.version)
            .with_classifier(classifier.map(str::to_owned))
            .with_extension(extension)
            .file_name()
    }

    fn copy(&mut self, classifier: Option<&str>, extension: &str, from: &Path) -> Result<FileEntry> {
        let name = self.file_name(classifier, extension);
        let dest = self.dir.join(&name);
        std::fs::copy(from, &dest).with_context(|| format!("failed to copy {} to {}", from.display(), dest.display()))?;
        self.entry(name)
    }

    fn write(&mut self, classifier: Option<&str>, extension: &str, text: &str) -> Result<FileEntry> {
        let name = self.file_name(classifier, extension);
        let dest = self.dir.join(&name);
        std::fs::write(&dest, text).with_context(|| format!("failed to write {}", dest.display()))?;
        self.entry(name)
    }

    fn entry(&mut self, name: String) -> Result<FileEntry> {
        let path = self.dir.join(&name);
        let size = std::fs::metadata(&path).with_context(|| format!("failed to read {}", path.display()))?.len();
        let sums = hash::checksums_file(&path).with_context(|| format!("failed to hash {}", path.display()))?;
        self.files.push(name.clone());
        Ok(FileEntry { name, size, sums })
    }

    fn finish(self) -> StagedModule {
        StagedModule { module: self.module, version: self.version, dir: self.dir, files: self.files }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        Package {
            name: "mathlib".into(),
            version: "1.2.0".into(),
            group: Some("com.acme".into()),
            description: Some("Math & more".into()),
            authors: vec!["Ada Lovelace <ada@example.com>".into(), "Grace".into()],
            license: Some("MIT OR Apache-2.0".into()),
            homepage: None,
            repository: Some("https://github.com/acme/mathlib".into()),
            publish: true,
        }
    }

    fn dep(module: &str, pom_module: &str, version: &str, scope: DependencyScope) -> PublishedDependency {
        let (g, a) = module.split_once(':').unwrap();
        let (pg, pa) = pom_module.split_once(':').unwrap();
        PublishedDependency {
            module: ModuleId::new(g, a),
            pom_module: ModuleId::new(pg, pa),
            version: version.into(),
            scope,
            classifier: None,
            extension: None,
            exclusions: Vec::new(),
            transitive: true,
            bom: false,
        }
    }

    fn file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, name).unwrap();
        path
    }

    #[test]
    fn names_platform_modules_like_gradle() {
        let native = PlatformKind::Native { konan_target: "ios_simulator_arm64".into() };
        assert_eq!(native.module_suffix(), "iossimulatorarm64");
        assert_eq!(native.target_name(), "iosSimulatorArm64");
        let wasm = PlatformKind::Wasm { environment: "js".into() };
        assert_eq!((wasm.module_suffix().as_str(), wasm.target_name().as_str()), ("wasm-js", "wasmJs"));
        assert!(!is_multiplatform(&[PlatformKind::Jvm { jvm_version: None }]));
        assert!(is_multiplatform(&[wasm]));
    }

    #[test]
    fn reads_licenses_and_developers() {
        assert_eq!(licenses(Some("MIT OR Apache-2.0")), ["MIT", "Apache-2.0"]);
        assert_eq!(licenses(Some("(GPL-2.0-only WITH Classpath-exception-2.0) AND MIT")), ["GPL-2.0-only", "MIT"]);
        assert!(licenses(None).is_empty());
        assert_eq!(developer("Ada Lovelace <ada@example.com>"), ("Ada Lovelace", Some("ada@example.com")));
        assert_eq!(developer(" Grace "), ("Grace", None));
    }

    #[test]
    fn stages_a_jvm_library_as_one_maven_module() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = package();
        let mut stdlib =
            dep("org.jetbrains.kotlin:kotlin-stdlib", "org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", DependencyScope::Compile);
        stdlib.exclusions.push(ModuleId::new("org.jetbrains", "annotations"));
        let mut logback = dep("ch.qos.logback:logback-classic", "ch.qos.logback:logback-classic", "1.5.0", DependencyScope::Runtime);
        logback.transitive = false;
        let mut bom = dep("org.acme:bom", "org.acme:bom", "3.0", DependencyScope::Compile);
        bom.bom = true;
        let publication = Publication {
            group: "com.acme".into(),
            package: &pkg,
            platforms: vec![PlatformPublication {
                kind: PlatformKind::Jvm { jvm_version: Some(17) },
                artifact: file(dir.path(), "classes.jar"),
                extra: Vec::new(),
                sources: Some(file(dir.path(), "src.jar")),
                javadoc: Some(file(dir.path(), "doc.jar")),
                dependencies: vec![stdlib, logback, bom],
                main_class: Some("com.acme.MainKt".into()),
            }],
            common_sources: None,
            common_javadoc: None,
        };
        let repo = dir.path().join("repo");
        let staged = publication.stage(&repo).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].module, ModuleId::new("com.acme", "mathlib"));
        assert_eq!(staged[0].files, ["mathlib-1.2.0.jar", "mathlib-1.2.0-sources.jar", "mathlib-1.2.0-javadoc.jar", "mathlib-1.2.0.pom"]);
        assert_eq!(staged[0].dir, repo.join("com").join("acme").join("mathlib").join("1.2.0"));
        assert_eq!(std::fs::read_to_string(staged[0].dir.join("mathlib-1.2.0.jar")).unwrap(), "classes.jar");

        let pom = std::fs::read_to_string(staged[0].dir.join("mathlib-1.2.0.pom")).unwrap();
        let parsed = kargo_maven::pom::Pom::parse(&pom).unwrap();
        assert!(!parsed.has_gradle_metadata);
        assert!(!pom.contains("<packaging>"), "{pom}");
        assert!(pom.contains("<description>Math &amp; more</description>"), "{pom}");
        assert!(pom.contains("<url>https://github.com/acme/mathlib</url>"), "{pom}");
        assert!(pom.contains("<name>Apache-2.0</name>") && pom.contains("<url>https://spdx.org/licenses/MIT.html</url>"), "{pom}");
        assert!(pom.contains("<email>ada@example.com</email>"), "{pom}");
        assert!(pom.contains("<connection>scm:git:https://github.com/acme/mathlib</connection>"), "{pom}");
        assert_eq!(parsed.dependencies.len(), 2);
        assert_eq!(parsed.dependencies[0].scope.as_deref(), Some("compile"));
        assert_eq!(parsed.dependencies[0].exclusions, [("org.jetbrains".to_owned(), "annotations".to_owned())]);
        assert_eq!(parsed.dependencies[1].scope.as_deref(), Some("runtime"));
        assert_eq!(parsed.dependencies[1].exclusions, [("*".to_owned(), "*".to_owned())]);
        assert_eq!(parsed.dependency_management[0].scope.as_deref(), Some("import"));
        assert_eq!(parsed.properties.get(MAIN_CLASS_PROPERTY).map(String::as_str), Some("com.acme.MainKt"));
    }

    #[test]
    fn stages_a_multiplatform_library_with_gradle_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = package();
        let coroutines = dep(
            "org.jetbrains.kotlinx:kotlinx-coroutines-core",
            "org.jetbrains.kotlinx:kotlinx-coroutines-core-jvm",
            "1.11.0",
            DependencyScope::Compile,
        );
        let runtime = dep("org.slf4j:slf4j-simple", "org.slf4j:slf4j-simple", "2.0.0", DependencyScope::Runtime);
        let publication = Publication {
            group: "com.acme".into(),
            package: &pkg,
            platforms: vec![
                PlatformPublication {
                    kind: PlatformKind::Jvm { jvm_version: Some(17) },
                    artifact: file(dir.path(), "classes.jar"),
                    extra: Vec::new(),
                    sources: Some(file(dir.path(), "jvm-src.jar")),
                    javadoc: None,
                    dependencies: vec![coroutines, runtime],
                    main_class: None,
                },
                PlatformPublication {
                    kind: PlatformKind::Native { konan_target: "linux_x64".into() },
                    artifact: file(dir.path(), "lib.klib"),
                    extra: vec![("cinterop-sqlite".into(), file(dir.path(), "sqlite.klib"))],
                    sources: None,
                    javadoc: None,
                    dependencies: Vec::new(),
                    main_class: None,
                },
            ],
            common_sources: Some(file(dir.path(), "common-src.jar")),
            common_javadoc: None,
        };
        let repo = dir.path().join("repo");
        let staged = publication.stage(&repo).unwrap();
        let names: Vec<&str> = staged.iter().map(|s| s.module.artifact.as_str()).collect();
        assert_eq!(names, ["mathlib-jvm", "mathlib-linuxx64", "mathlib"], "the root comes last");
        assert_eq!(
            staged[1].files,
            [
                "mathlib-linuxx64-1.2.0.klib",
                "mathlib-linuxx64-1.2.0-cinterop-sqlite.klib",
                "mathlib-linuxx64-1.2.0.module",
                "mathlib-linuxx64-1.2.0.pom"
            ]
        );
        assert_eq!(staged[2].files, ["mathlib-1.2.0-sources.jar", "mathlib-1.2.0.module", "mathlib-1.2.0.pom"]);

        // Kargo's own resolver reads what Kargo publishes.
        let root_text = std::fs::read_to_string(staged[2].dir.join("mathlib-1.2.0.module")).unwrap();
        assert!(root_text.starts_with("{\n  \"formatVersion\": \"1.1\""), "Gradle wants formatVersion first:\n{root_text}");
        let root = kargo_maven::module::ModuleMetadata::parse(&root_text).unwrap();
        let root_id = ModuleId::new("com.acme", "mathlib");
        let jvm = kargo_maven::module::select_variant(&root, &root_id, &kargo_maven::Platform::Jvm).unwrap();
        assert_eq!(jvm.available_at.as_ref().unwrap().module, "mathlib-jvm");
        assert_eq!(jvm.available_at.as_ref().unwrap().url, "../../mathlib-jvm/1.2.0/mathlib-jvm-1.2.0.module");
        let native = kargo_maven::module::select_variant(&root, &root_id, &kargo_maven::Platform::Native("linux_x64".into())).unwrap();
        assert_eq!(native.available_at.as_ref().unwrap().module, "mathlib-linuxx64");
        assert!(kargo_maven::module::select_variant(&root, &root_id, &kargo_maven::Platform::Wasm("js".into())).is_none());
        let root_pom = std::fs::read_to_string(staged[2].dir.join("mathlib-1.2.0.pom")).unwrap();
        assert!(kargo_maven::pom::Pom::parse(&root_pom).unwrap().has_gradle_metadata);
        assert!(root_pom.contains("<packaging>pom</packaging>"));

        let jvm_text = std::fs::read_to_string(staged[0].dir.join("mathlib-jvm-1.2.0.module")).unwrap();
        let jvm_meta = kargo_maven::module::ModuleMetadata::parse(&jvm_text).unwrap();
        let jvm_id = ModuleId::new("com.acme", "mathlib-jvm");
        let runtime = kargo_maven::module::select_variant(&jvm_meta, &jvm_id, &kargo_maven::Platform::Jvm).unwrap();
        assert_eq!(runtime.name, "jvmRuntimeElements-published");
        assert_eq!(runtime.files[0].url, "mathlib-jvm-1.2.0.jar");
        assert_eq!(runtime.files[0].sha1.as_deref(), Some(hash::sha1_file(&staged[0].dir.join("mathlib-jvm-1.2.0.jar")).unwrap().as_str()));
        let deps: Vec<&str> = runtime.dependencies.iter().map(|d| d.module.as_str()).collect();
        assert_eq!(deps, ["kotlinx-coroutines-core", "slf4j-simple"], "variants name root modules");
        let api = jvm_meta.variants.iter().find(|v| v.name == "jvmApiElements-published").unwrap();
        assert_eq!(api.dependencies.len(), 1, "runtime-only dependencies stay off the API variant");
        assert_eq!(api.attributes["org.gradle.jvm.version"], 17);
        assert!(jvm_text.contains("\"url\": \"../../mathlib/1.2.0/mathlib-1.2.0.module\""), "{jvm_text}");
        let jvm_pom = std::fs::read_to_string(staged[0].dir.join("mathlib-jvm-1.2.0.pom")).unwrap();
        assert!(jvm_pom.contains("<artifactId>kotlinx-coroutines-core-jvm</artifactId>"), "POMs name platform modules:\n{jvm_pom}");

        let native_text = std::fs::read_to_string(staged[1].dir.join("mathlib-linuxx64-1.2.0.module")).unwrap();
        let native_meta = kargo_maven::module::ModuleMetadata::parse(&native_text).unwrap();
        let klibs = kargo_maven::module::select_variant(
            &native_meta,
            &ModuleId::new("com.acme", "mathlib-linuxx64"),
            &kargo_maven::Platform::Native("linux_x64".into()),
        )
        .unwrap();
        let urls: Vec<&str> = klibs.files.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(urls, ["mathlib-linuxx64-1.2.0.klib", "mathlib-linuxx64-1.2.0-cinterop-sqlite.klib"]);
        assert!(std::fs::read_to_string(staged[1].dir.join("mathlib-linuxx64-1.2.0.pom")).unwrap().contains("<packaging>klib</packaging>"));
    }

    #[test]
    fn snapshots_have_integration_status() {
        assert_eq!(status("1.0-SNAPSHOT"), "integration");
        assert_eq!(status("1.0"), "release");
    }
}
