//! Serde model of `kargo.toml` and its validation into [`super::Manifest`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml::Value;

use super::*;
use kargo_maven::Repository;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RawManifest {
    package: RawPackage,
    #[serde(default)]
    kotlin: RawKotlin,
    #[serde(default)]
    target: Option<RawTargets>,
    #[serde(default)]
    bin: Vec<RawBin>,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default)]
    dev_dependencies: toml::Table,
    #[serde(default)]
    repositories: toml::Table,
    #[serde(default)]
    frameworks: RawFrameworks,
    #[serde(default)]
    plugins: toml::Table,
    #[serde(default)]
    tasks: toml::Table,
    #[serde(default)]
    env: toml::Table,
    #[serde(default)]
    profile: BTreeMap<String, RawProfile>,
    #[serde(default)]
    toolchain: RawToolchain,
    #[serde(default)]
    doc: RawDoc,
    #[serde(default)]
    fmt: RawFmt,
    #[serde(default)]
    lint: RawLint,
    #[serde(default)]
    test: RawTest,
    #[serde(default)]
    bench: RawBench,
    #[serde(default)]
    publish: RawPublish,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawBin {
    name: String,
    main: Option<String>,
    entry_point: Option<String>,
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawToolchain {
    jdk: Option<Value>,
    graalvm: Option<Value>,
    kargo: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawDoc {
    dokka: Option<String>,
    #[serde(default)]
    includes: Vec<PathBuf>,
    source_link: Option<String>,
    #[serde(default)]
    external_links: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawFmt {
    ktlint: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTest {
    fail_fast: Option<bool>,
    timeout: Option<String>,
    test_timeout: Option<String>,
    #[serde(default)]
    jvm_args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawBench {
    warmup: Option<String>,
    iterations: Option<String>,
    forks: Option<u32>,
    mode: Option<String>,
    time_unit: Option<String>,
    #[serde(default)]
    jvm_args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawLint {
    detekt: Option<String>,
    #[serde(default)]
    config: Vec<PathBuf>,
    baseline: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    group: Option<String>,
    description: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    license: Option<String>,
    homepage: Option<String>,
    repository: Option<String>,
    publish: Option<bool>,
    #[serde(default)]
    jlink: RawJlink,
    #[serde(default)]
    native_image: RawNativeImage,
    #[serde(default)]
    installer: RawInstaller,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawJlink {
    #[serde(default)]
    modules: Vec<String>,
    jdeps: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNativeImage {
    #[serde(default)]
    args: Vec<String>,
    #[serde(rename = "static")]
    static_link: Option<bool>,
    #[serde(default)]
    initialize_at_build_time: Vec<String>,
    metadata_repository: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawInstaller {
    name: Option<String>,
    vendor: Option<String>,
    description: Option<String>,
    app_version: Option<String>,
    #[serde(rename = "type")]
    kind: Option<Value>,
    icon: Option<Value>,
    license_file: Option<PathBuf>,
    menu: Option<bool>,
    shortcut: Option<bool>,
    console: Option<bool>,
    upgrade_uuid: Option<String>,
    bundle_id: Option<String>,
    signing_identity: Option<String>,
    category: Option<String>,
    #[serde(default)]
    file_associations: Vec<RawFileAssociation>,
    notarize: Option<RawNotarize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawFileAssociation {
    extension: String,
    mime_type: String,
    description: Option<String>,
    icon: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNotarize {
    apple_id_env: Option<String>,
    password_env: Option<String>,
    team_id: Option<String>,
    keychain_profile: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstallerIcons {
    windows: Option<PathBuf>,
    macos: Option<PathBuf>,
    linux: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPublish {
    repository: Option<String>,
    sign: Option<bool>,
    sources: Option<bool>,
    javadoc: Option<Value>,
    central_publishing: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawKotlin {
    version: Option<String>,
    language_version: Option<String>,
    api_version: Option<String>,
    ksp: Option<String>,
    #[serde(default)]
    opt_in: Vec<String>,
    #[serde(default)]
    compiler_args: Vec<String>,
    #[serde(default)]
    warnings_as_errors: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTargets {
    jvm: Option<RawJvm>,
    wasm: Option<RawWasm>,
    native: Option<RawNative>,
    android: Option<toml::Table>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawJvm {
    jvm_target: Option<String>,
    main_class: Option<String>,
    #[serde(default)]
    jvm_args: Vec<String>,
    #[serde(default)]
    javac_args: Vec<String>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWasm {
    environment: Option<String>,
    kind: Option<String>,
    module_name: Option<String>,
    npm_name: Option<String>,
    package_manager: Option<String>,
    #[serde(default)]
    npm_dependencies: BTreeMap<String, String>,
    wasm_opt: Option<Value>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNative {
    #[serde(default)]
    platforms: Vec<String>,
    kind: Option<String>,
    entry_point: Option<String>,
    #[serde(default)]
    linker_options: Vec<String>,
    #[serde(default)]
    static_libraries: Vec<String>,
    #[serde(default)]
    library_paths: Vec<PathBuf>,
    #[serde(default)]
    frameworks: Vec<String>,
    #[serde(default)]
    platform: BTreeMap<String, RawLinkOptions>,
    #[serde(default)]
    cinterop: BTreeMap<String, RawCinterop>,
    c_sources: Option<RawCSources>,
    bundle_id: Option<String>,
    #[serde(default)]
    export: Vec<String>,
    #[serde(default)]
    dependencies: toml::Table,
}

/// `[target.native.platform.<konan-target>]`: linker options for one platform.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawLinkOptions {
    #[serde(default)]
    linker_options: Vec<String>,
    #[serde(default)]
    static_libraries: Vec<String>,
    #[serde(default)]
    library_paths: Vec<PathBuf>,
    #[serde(default)]
    frameworks: Vec<String>,
}

impl RawLinkOptions {
    fn into_options(self) -> NativeOptions {
        NativeOptions {
            compiler_options: Vec::new(),
            linker_options: self.linker_options,
            static_libraries: self.static_libraries,
            library_paths: self.library_paths,
            frameworks: self.frameworks,
        }
    }
}

/// `[target.native.cinterop.<name>.platform.<konan-target>]`: options of a C library for one platform.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNativeOptions {
    #[serde(default)]
    compiler_options: Vec<String>,
    #[serde(default)]
    linker_options: Vec<String>,
    #[serde(default)]
    static_libraries: Vec<String>,
    #[serde(default)]
    library_paths: Vec<PathBuf>,
    #[serde(default)]
    frameworks: Vec<String>,
}

impl RawNativeOptions {
    fn into_options(self) -> NativeOptions {
        NativeOptions {
            compiler_options: self.compiler_options,
            linker_options: self.linker_options,
            static_libraries: self.static_libraries,
            library_paths: self.library_paths,
            frameworks: self.frameworks,
        }
    }
}

/// A string or a list of strings.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawCinterop {
    def: Option<PathBuf>,
    package: Option<String>,
    #[serde(default)]
    headers: Vec<String>,
    #[serde(default)]
    compiler_options: Vec<String>,
    #[serde(default)]
    linker_options: Vec<String>,
    #[serde(default)]
    static_libraries: Vec<String>,
    #[serde(default)]
    library_paths: Vec<PathBuf>,
    #[serde(default)]
    frameworks: Vec<String>,
    #[serde(default)]
    platform: BTreeMap<String, RawNativeOptions>,
    #[serde(default)]
    header_filter: Vec<String>,
    #[serde(default)]
    excluded_functions: Vec<String>,
    #[serde(default)]
    strict_enums: Vec<String>,
    #[serde(default)]
    non_strict_enums: Vec<String>,
    #[serde(default)]
    modules: Vec<String>,
    language: Option<String>,
    pkg_config: Option<OneOrMany>,
    link: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawCSources {
    sources: Option<Vec<String>>,
    include_dirs: Option<Vec<PathBuf>>,
    #[serde(default)]
    defines: Vec<String>,
    #[serde(default)]
    compiler_options: Vec<String>,
    #[serde(default)]
    platform: BTreeMap<String, RawCFlags>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawCFlags {
    #[serde(default)]
    defines: Vec<String>,
    #[serde(default)]
    compiler_options: Vec<String>,
}

/// The `platform.<konan-target>` tables of `section`, by normalized Konan target name.
fn platform_tables<R, T>(section: &str, raw: BTreeMap<String, R>, convert: impl Fn(R) -> T) -> Result<BTreeMap<String, T>> {
    let mut out = BTreeMap::new();
    for (key, value) in raw {
        if key == "host" {
            bail!("[{section}.platform.host] is not supported; name the Kotlin/Native platform, such as `linux_x64` or `macos_arm64`");
        }
        let target = normalize_konan_target(&key).with_context(|| format!("invalid table [{section}.platform.{key}]"))?;
        if out.insert(target.clone(), convert(value)).is_some() {
            bail!("[{section}.platform] lists `{target}` twice");
        }
    }
    Ok(out)
}

/// Checks the options of `section` and of its platform tables.
fn validate_options(section: &str, common: &NativeOptions, platform: &BTreeMap<String, NativeOptions>) -> Result<()> {
    let tables = std::iter::once((common, section.to_owned())).chain(platform.iter().map(|(t, o)| (o, format!("{section}.platform.{t}"))));
    for (options, section) in tables {
        if let Some(bad) = options.static_libraries.iter().find(|l| l.is_empty() || l.contains(['/', '\\'])) {
            bail!(
                "invalid static library `{bad}` in [{section}]: give the file name (such as `libfoo.a`) and its directory in `library-paths`"
            );
        }
        if let Some(bad) = options.frameworks.iter().find(|f| f.is_empty() || f.contains(['/', '\\']) || f.ends_with(".framework")) {
            bail!("invalid framework `{bad}` in [{section}]: give the framework's name, such as `Foundation`");
        }
    }
    Ok(())
}

fn validate_cinterop(name: String, c: RawCinterop) -> Result<Cinterop> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("invalid cinterop name `{name}` in [target.native.cinterop] (use letters, digits, `_` and `-`)");
    }
    if let Some(pkg) = &c.package
        && (pkg.is_empty() || !pkg.split('.').all(|part| !part.is_empty()))
    {
        bail!("invalid package `{pkg}` in [target.native.cinterop.{name}]");
    }
    let section = format!("target.native.cinterop.{name}");
    let language = match c.language.as_deref() {
        None => None,
        Some("C" | "c") => Some(CLanguage::C),
        Some("Objective-C" | "objective-c" | "objc") => Some(CLanguage::ObjectiveC),
        Some(other) => bail!("unknown language `{other}` in [{section}] (expected \"C\" or \"Objective-C\")"),
    };
    let link = match c.link.as_deref() {
        None | Some("dynamic") => LinkMode::Dynamic,
        Some("static") => LinkMode::Static,
        Some(other) => bail!("unknown link `{other}` in [{section}] (expected \"dynamic\" or \"static\")"),
    };
    let pkg_config = c.pkg_config.map(OneOrMany::into_vec).unwrap_or_default();
    if let Some(bad) = pkg_config.iter().find(|p| p.trim().is_empty() || p.contains(char::is_whitespace)) {
        bail!("invalid pkg-config package `{bad}` in [{section}]");
    }
    for value in c.header_filter.iter().chain(&c.excluded_functions).chain(&c.strict_enums).chain(&c.non_strict_enums).chain(&c.modules) {
        if value.trim().is_empty() || value.contains(char::is_whitespace) {
            bail!("invalid entry `{value}` in [{section}]: `.def` lists are separated by spaces, so entries cannot contain any");
        }
    }
    let platform = platform_tables(&section, c.platform, RawNativeOptions::into_options)?;
    let options = NativeOptions {
        compiler_options: c.compiler_options,
        linker_options: c.linker_options,
        static_libraries: c.static_libraries,
        library_paths: c.library_paths,
        frameworks: c.frameworks,
    };
    validate_options(&section, &options, &platform)?;
    Ok(Cinterop {
        name,
        def: c.def,
        package: c.package,
        headers: c.headers,
        options,
        platform,
        header_filter: c.header_filter,
        excluded_functions: c.excluded_functions,
        strict_enums: c.strict_enums,
        non_strict_enums: c.non_strict_enums,
        modules: c.modules,
        language,
        pkg_config,
        link,
    })
}

fn validate_c_sources(c: RawCSources) -> Result<CSources> {
    let section = "target.native.c-sources";
    let sources = c.sources.unwrap_or_else(|| vec![CSources::DEFAULT_DIR.to_owned()]);
    if sources.is_empty() {
        bail!("[{section}] `sources` is empty; list C files, directories or globs, or remove the table");
    }
    for source in &sources {
        if source.trim().is_empty() || Path::new(source).is_absolute() || Path::new(source).components().any(|c| c.as_os_str() == "..") {
            bail!("invalid source `{source}` in [{section}] (use a path or glob inside the project)");
        }
    }
    for define in c.defines.iter().chain(c.platform.values().flat_map(|p| &p.defines)) {
        if define.is_empty() || define.starts_with('=') || define.starts_with("-D") {
            bail!("invalid define `{define}` in [{section}] (write `NAME` or `NAME=value`)");
        }
    }
    let platform = platform_tables(section, c.platform, |f| CFlags { defines: f.defines, compiler_options: f.compiler_options })?;
    Ok(CSources {
        sources,
        include_dirs: c.include_dirs.unwrap_or_else(|| vec![PathBuf::from(CSources::DEFAULT_DIR)]),
        defines: c.defines,
        compiler_options: c.compiler_options,
        platform,
    })
}

/// Android build settings (rendered into the generated Gradle project).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidConfig {
    pub namespace: String,
    pub application_id: Option<String>,
    #[serde(default = "default_min_sdk")]
    pub min_sdk: u32,
    #[serde(default = "default_sdk")]
    pub target_sdk: u32,
    #[serde(default = "default_compile_sdk")]
    pub compile_sdk: u32,
    #[serde(default = "default_version_code")]
    pub version_code: u32,
    pub version_name: Option<String>,
    /// `application` (default) or `library`.
    #[serde(default = "default_android_kind")]
    pub kind: String,
    /// Android Gradle Plugin version.
    pub agp_version: Option<String>,
    /// Gradle version that runs the Android build (default: the one `agp-version` requires).
    pub gradle_version: Option<String>,
    /// Shrink, obfuscate and optimize release builds with R8.
    #[serde(default)]
    pub minify: bool,
    /// Remove unused resources from release builds (default: `minify`, for applications).
    pub shrink_resources: Option<bool>,
    /// ProGuard/R8 rule files for release builds, relative to the project root.
    #[serde(default)]
    pub proguard_files: Vec<PathBuf>,
    /// ProGuard/R8 rules a library ships to the apps that use it, relative to the project root.
    #[serde(default)]
    pub consumer_proguard_files: Vec<PathBuf>,
    /// `BuildConfig` fields of every variant.
    #[serde(default)]
    pub build_config: BTreeMap<String, BuildConfigValue>,
    /// Signing configurations by build type (`debug` or `release`).
    #[serde(default)]
    pub signing: BTreeMap<String, AndroidSigning>,
    /// The order of the flavor dimensions; required with more than one dimension.
    #[serde(default)]
    pub flavor_dimensions: Vec<String>,
    /// Product flavors by name.
    #[serde(default)]
    pub flavors: BTreeMap<String, AndroidFlavor>,
}

impl AndroidConfig {
    /// The build types Kargo uses: `debug` (dev profile) and `release`.
    pub const BUILD_TYPES: [&str; 2] = ["debug", "release"];

    /// The flavor dimensions in priority order: `flavor-dimensions`, else the single one in use.
    pub fn dimensions(&self) -> Vec<String> {
        if !self.flavor_dimensions.is_empty() {
            return self.flavor_dimensions.clone();
        }
        let mut dims: Vec<String> = self.flavors.values().map(|f| f.dimension.clone()).collect();
        dims.dedup();
        dims
    }

    /// The flavors of `dimension`, by name.
    pub fn flavors_of<'a>(&'a self, dimension: &'a str) -> impl Iterator<Item = (&'a String, &'a AndroidFlavor)> + 'a {
        self.flavors.iter().filter(move |(_, f)| f.dimension == dimension)
    }
}

/// `[target.android.signing.<build-type>]`: the keystore that signs a build type.
/// Passwords come from environment variables, never from `kargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidSigning {
    /// The keystore, relative to the project root.
    pub store_file: PathBuf,
    /// Environment variable holding the keystore password.
    pub store_password_env: String,
    pub key_alias: String,
    /// Environment variable holding the key password (default: `store-password-env`).
    pub key_password_env: Option<String>,
    /// `jks` or `pkcs12` (default: detected by AGP).
    pub store_type: Option<String>,
}

impl AndroidSigning {
    pub fn key_password_env(&self) -> &str {
        self.key_password_env.as_deref().unwrap_or(&self.store_password_env)
    }
}

/// `[target.android.flavors.<name>]`: a product flavor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidFlavor {
    pub dimension: String,
    pub application_id_suffix: Option<String>,
    pub version_name_suffix: Option<String>,
    pub version_code: Option<u32>,
    pub min_sdk: Option<u32>,
    #[serde(default)]
    pub build_config: BTreeMap<String, BuildConfigValue>,
}

/// The value of a `BuildConfig` field; its Java type follows from the TOML type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum BuildConfigValue {
    Bool(bool),
    Int(i64),
    String(String),
}

impl BuildConfigValue {
    /// The Java type and the Java literal of the field.
    pub fn java(&self) -> (&'static str, String) {
        match self {
            BuildConfigValue::Bool(b) => ("boolean", b.to_string()),
            BuildConfigValue::Int(i) if i32::try_from(*i).is_ok() => ("int", i.to_string()),
            BuildConfigValue::Int(i) => ("long", format!("{i}L")),
            BuildConfigValue::String(s) => {
                let mut lit = String::from("\"");
                for c in s.chars() {
                    match c {
                        '"' => lit.push_str("\\\""),
                        '\\' => lit.push_str("\\\\"),
                        '\n' => lit.push_str("\\n"),
                        '\r' => lit.push_str("\\r"),
                        '\t' => lit.push_str("\\t"),
                        c => lit.push(c),
                    }
                }
                lit.push('"');
                ("String", lit)
            }
        }
    }
}

fn is_java_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn validate_build_config(fields: &BTreeMap<String, BuildConfigValue>, section: &str) -> Result<()> {
    for name in fields.keys() {
        if !is_java_identifier(name) {
            bail!("[{section}.build-config] `{name}` is not a valid Java field name");
        }
    }
    Ok(())
}

fn validate_android(config: &AndroidConfig) -> Result<()> {
    if config.namespace.trim().is_empty() {
        bail!("[target.android] `namespace` must not be empty");
    }
    if !matches!(config.kind.as_str(), "application" | "library") {
        bail!("[target.android] `kind` must be `application` or `library`");
    }
    if config.min_sdk > config.target_sdk || config.target_sdk > config.compile_sdk {
        bail!("[target.android] expected min-sdk <= target-sdk <= compile-sdk");
    }
    let is_app = config.kind == "application";
    if config.shrink_resources == Some(true) && (!config.minify || !is_app) {
        bail!("[target.android] `shrink-resources` needs `minify = true` and `kind = \"application\"`");
    }
    if !config.consumer_proguard_files.is_empty() && is_app {
        bail!("[target.android] `consumer-proguard-files` only applies to libraries; use `proguard-files` for an application");
    }
    validate_build_config(&config.build_config, "target.android")?;
    for (build_type, signing) in &config.signing {
        if !AndroidConfig::BUILD_TYPES.contains(&build_type.as_str()) {
            bail!("[target.android.signing.{build_type}] is not a build type; use `debug` or `release`");
        }
        if !is_app {
            bail!("[target.android.signing.{build_type}] only applies to applications; a library is signed by the app that uses it");
        }
        if let Some(t) = &signing.store_type
            && !matches!(t.as_str(), "jks" | "pkcs12")
        {
            bail!("[target.android.signing.{build_type}] `store-type` must be `jks` or `pkcs12`");
        }
        for var in [Some(signing.store_password_env.as_str()), signing.key_password_env.as_deref()].into_iter().flatten() {
            if var.trim().is_empty() {
                bail!("[target.android.signing.{build_type}] the `*-password-env` keys name environment variables and must not be empty");
            }
        }
    }
    for dim in &config.flavor_dimensions {
        if !config.flavors.values().any(|f| &f.dimension == dim) {
            bail!("[target.android] flavor dimension `{dim}` has no flavors");
        }
    }
    let mut dims: Vec<&str> = config.flavors.values().map(|f| f.dimension.as_str()).collect();
    dims.sort();
    dims.dedup();
    if dims.len() > 1 && config.flavor_dimensions.is_empty() {
        bail!("[target.android] the flavors use several dimensions ({}); list their order in `flavor-dimensions`", dims.join(", "));
    }
    for (name, flavor) in &config.flavors {
        if !is_java_identifier(name) || name.contains('$') {
            bail!("invalid flavor name `{name}`: use letters, digits and `_`, starting with a letter");
        }
        if AndroidConfig::BUILD_TYPES.contains(&name.as_str()) || name == "test" || name == "main" || name == "androidTest" {
            bail!("`{name}` cannot be a flavor name: it is a build type or source set");
        }
        if flavor.dimension.trim().is_empty() {
            bail!("[target.android.flavors.{name}] `dimension` must not be empty");
        }
        if !config.flavor_dimensions.is_empty() && !config.flavor_dimensions.contains(&flavor.dimension) {
            bail!("[target.android.flavors.{name}] dimension `{}` is not listed in `flavor-dimensions`", flavor.dimension);
        }
        if !is_app && (flavor.application_id_suffix.is_some() || flavor.version_name_suffix.is_some() || flavor.version_code.is_some()) {
            bail!(
                "[target.android.flavors.{name}] `application-id-suffix`, `version-name-suffix` and `version-code` only apply to applications"
            );
        }
        validate_build_config(&flavor.build_config, &format!("target.android.flavors.{name}"))?;
    }
    Ok(())
}

fn default_min_sdk() -> u32 {
    24
}
fn default_sdk() -> u32 {
    36
}
fn default_compile_sdk() -> u32 {
    37
}
fn default_version_code() -> u32 {
    1
}
fn default_android_kind() -> String {
    "application".to_owned()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawFrameworks {
    compose: Option<Toggle<ComposeConfig>>,
    serialization: Option<Toggle<SerializationConfig>>,
    coroutines: Option<Value>,
    ktor: Option<Toggle<KtorConfig>>,
    lombok: Option<Toggle<LombokConfig>>,
    spring_boot: Option<Toggle<SpringBootConfig>>,
    benchmark: Option<Toggle<BenchmarkConfig>>,
}

/// `feature = true`, `feature = false` or `feature = { ...options }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Toggle<T> {
    Enabled(bool),
    Config(T),
}

impl<T: Default> Toggle<T> {
    fn into_option(self) -> Option<T> {
        match self {
            Toggle::Enabled(true) => Some(T::default()),
            Toggle::Enabled(false) => None,
            Toggle::Config(c) => Some(c),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComposeConfig {
    /// Compose Multiplatform version.
    pub version: Option<String>,
    /// Add Material 3 (default true).
    pub material3: Option<bool>,
    /// The package of the generated `Res` class of `composeResources/`; defaults to
    /// `<group>.<name>.generated.resources`.
    pub resources_package: Option<String>,
    /// Make `Res` and its accessors `public` instead of `internal`, so other modules can use them.
    pub public_resources: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SerializationConfig {
    pub version: Option<String>,
    /// Formats such as `json`, `protobuf`, `cbor`, `properties` (default `json`).
    pub formats: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KtorConfig {
    pub version: Option<String>,
    /// Server engine (`netty`, `cio`, `jetty`, `tomcat`), or omitted for no server.
    pub server: Option<String>,
    /// Client engine (`cio`, `okhttp`, `java`, `apache5`, `js`, `curl`, `winhttp`, `darwin`).
    pub client: Option<String>,
    /// Extra server plugins, e.g. `content-negotiation`, `cors`, `auth`.
    #[serde(default)]
    pub server_plugins: Vec<String>,
    /// Extra client plugins, e.g. `content-negotiation`, `logging`, `auth`.
    #[serde(default)]
    pub client_plugins: Vec<String>,
    /// Add `ktor-serialization-kotlinx-json` (default true when serialization is enabled).
    pub json: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LombokConfig {
    pub version: Option<String>,
    /// Path to `lombok.config`, relative to the project root.
    pub config: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BenchmarkConfig {
    /// The kotlinx-benchmark version.
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SpringBootConfig {
    pub version: Option<String>,
    /// Starters without the `spring-boot-starter-` prefix (default `["web"]`).
    pub starters: Option<Vec<String>>,
    /// Apply the JPA no-arg preset (default: true if `data-jpa` is a starter).
    pub jpa: Option<bool>,
}

/// One `[profile.<name>]` table. Unset keys come from the profile it `inherits` from.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawProfile {
    inherits: Option<String>,
    debug: Option<bool>,
    optimize: Option<bool>,
    compiler_args: Option<Vec<String>>,
    jvm_args: Option<Vec<String>>,
    #[serde(default)]
    env: toml::Table,
    dependencies: Option<toml::Table>,
}

/// The profiles every manifest has, which custom profiles may not replace.
const BUILTIN_PROFILES: [&str; 2] = ["dev", "release"];

/// Names of output directories under `target/` that a profile must not take: `debug` is the
/// output of `dev`, the others belong to `android` and `lint`.
const RESERVED_PROFILE_NAMES: [&str; 3] = ["debug", "android", "lint"];

impl RawProfile {
    /// The profile this one inherits from: `dev` unless `inherits` says otherwise.
    fn parent(&self) -> &str {
        self.inherits.as_deref().unwrap_or("dev")
    }

    /// Applies this table's keys over `base`, the profile it inherits from. `name` is the profile's
    /// name in the manifest, for error messages.
    fn apply(self, mut base: Profile, name: &str, deps: &mut DependencyParser) -> Result<Profile> {
        if let Some(d) = self.debug {
            base.debug = d;
        }
        if let Some(o) = self.optimize {
            base.optimize = o;
        }
        if let Some(args) = self.compiler_args {
            base.compiler_args = args;
        }
        if let Some(args) = self.jvm_args {
            base.jvm_args = args;
        }
        // Variables and `.env` files add to the inherited ones; a variable of the same name replaces its value.
        let own_env = parse_env(&self.env, &format!("[profile.{name}.env]"), false)?;
        base.env.vars.extend(own_env.vars);
        base.env.files.extend(own_env.files);
        if let Some(table) = self.dependencies {
            let section = format!("profile.{name}.dependencies");
            let paths = deps.paths.len();
            let own = deps.parse(&table, &section, false, None)?;
            if deps.paths.len() != paths {
                bail!("[{section}] takes Maven dependencies; declare path dependencies in [dependencies]");
            }
            for dep in own {
                base.dependencies.retain(|d| !d.same_artifact(&dep));
                base.dependencies.push(dep);
            }
        }
        Ok(base)
    }
}

/// Resolves the `[profile.<name>]` tables. `dev` and `release` start from the built-in defaults and
/// may not inherit; a custom profile starts from the profile it inherits from, so it is resolved
/// after that one.
fn resolve_profiles(raw: BTreeMap<String, RawProfile>, deps: &mut DependencyParser) -> Result<BTreeMap<String, Profile>> {
    let mut profiles = BTreeMap::from([("dev".to_owned(), Profile::dev()), ("release".to_owned(), Profile::release())]);
    let mut custom = BTreeMap::new();
    for (name, profile) in raw {
        if BUILTIN_PROFILES.contains(&name.as_str()) {
            if profile.inherits.is_some() {
                bail!("[profile.{name}]: `inherits` is for custom profiles; `dev` and `release` are the built-in profiles");
            }
            let base = profiles.remove(&name).expect("built-in profile");
            let resolved = profile.apply(base, &name, deps)?;
            profiles.insert(name, resolved);
        } else {
            validate_profile_name(&name)?;
            custom.insert(name, profile);
        }
    }
    while !custom.is_empty() {
        let ready: Vec<String> = custom.iter().filter(|(_, p)| profiles.contains_key(p.parent())).map(|(n, _)| n.clone()).collect();
        if ready.is_empty() {
            // Follow the `inherits` of one unresolved profile: the chain ends at an unknown profile or loops back.
            let mut chain = vec![custom.keys().next().expect("custom profiles are left").clone()];
            loop {
                let parent = custom[chain.last().expect("chain is not empty")].parent().to_owned();
                if !custom.contains_key(&parent) {
                    bail!(
                        "[profile.{}] inherits from `{parent}`, which is not a profile; use `dev`, `release` or a profile of this manifest",
                        chain.last().expect("chain is not empty")
                    );
                }
                if let Some(at) = chain.iter().position(|n| *n == parent) {
                    bail!("the `inherits` of [profile.*] loops: {} -> {parent}", chain[at..].join(" -> "));
                }
                chain.push(parent);
            }
        }
        for name in ready {
            let profile = custom.remove(&name).expect("ready profile");
            let base = profiles[profile.parent()].clone();
            let resolved = profile.apply(base, &name, deps)?;
            profiles.insert(name, resolved);
        }
    }
    Ok(profiles)
}

/// Checks a custom profile's name: it becomes an output directory `target/<name>`.
fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name.starts_with(|c: char| c.is_ascii_alphabetic())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("invalid profile name `{name}` (use letters, digits, `-` and `_`, starting with a letter)");
    }
    if RESERVED_PROFILE_NAMES.contains(&name) {
        bail!("profile name `{name}` is reserved; its output would be target/{name}, which Kargo uses for something else");
    }
    Ok(())
}

/// The fields of a table-form variable in `[env]`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawEnvVar {
    value: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    relative: bool,
    #[serde(default)]
    required: bool,
}

/// Parses `[env]` (`base` is true) or `[profile.<name>.env]`. A variable is a string, number or
/// boolean, or a table with `value`, `force`, `relative` and `required`. `env-files` and, in
/// `[env]`, `expose` are arrays of strings; a string named `expose` is still a variable.
fn parse_env(table: &toml::Table, section: &str, base: bool) -> Result<EnvTable> {
    let mut env = EnvTable::default();
    for (name, value) in table {
        match (name.as_str(), value) {
            ("env-files", value) => env.files = parse_string_list(value, &format!("{section}: `env-files`"))?,
            ("expose", Value::Array(_)) if base => {
                let names = parse_string_list(value, &format!("{section}: `expose`"))?;
                for (at, name) in names.iter().enumerate() {
                    if !is_env_name(name) {
                        bail!("{section}: `expose` lists `{name}`, which is not a valid variable name");
                    }
                    if names[..at].contains(name) {
                        bail!("{section}: `expose` lists `{name}` more than once");
                    }
                }
                env.expose = names;
            }
            ("expose", Value::Array(_)) => bail!("{section}: `expose` belongs in [env]; it is the only table that generates BuildConfig"),
            _ => {
                let var = parse_env_var(name, value, section)?;
                env.vars.insert(name.clone(), var);
            }
        }
    }
    Ok(env)
}

/// An array of non-empty strings, such as `env-files`.
fn parse_string_list(value: &Value, what: &str) -> Result<Vec<String>> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                _ => bail!("{what} must be an array of non-empty strings"),
            })
            .collect(),
        _ => bail!("{what} must be an array of non-empty strings"),
    }
}

/// One variable of `[env]` or `[profile.<name>.env]`.
fn parse_env_var(name: &str, value: &Value, section: &str) -> Result<EnvVar> {
    if !is_env_name(name) {
        bail!("{section}: invalid variable name `{name}` (use letters, digits and `_`, not starting with a digit)");
    }
    Ok(match value {
        Value::String(_) | Value::Integer(_) | Value::Boolean(_) => {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            EnvVar { value: Some(value), ..Default::default() }
        }
        Value::Table(_) => {
            let raw: RawEnvVar = value.clone().try_into().with_context(|| format!("{section}: invalid `{name}`"))?;
            match (&raw.value, raw.required) {
                (None, false) => bail!("{section}: `{name}` needs a `value`, or `required = true`"),
                (None, true) if raw.relative => bail!("{section}: `{name}` sets `relative` without a `value`"),
                (Some(_), true) => bail!("{section}: `{name}` sets both `value` and `required`; a required variable has no default"),
                _ => {}
            }
            EnvVar { value: raw.value, force: raw.force, relative: raw.relative, required: raw.required }
        }
        _ => bail!("{section}: `{name}` must be a string or a table"),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct DetailedDependency {
    path: Option<PathBuf>,
    group: Option<String>,
    artifact: Option<String>,
    coords: Option<String>,
    version: Option<String>,
    scope: Option<String>,
    classifier: Option<String>,
    #[serde(alias = "type")]
    extension: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    force: bool,
    transitive: Option<bool>,
    #[serde(default)]
    bom: bool,
    #[serde(default)]
    exported: bool,
    #[serde(default)]
    test_fixtures: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRepository {
    url: String,
    username_env: Option<String>,
    password_env: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPlugin {
    maven: Option<String>,
    id: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, String>,
    command: Option<String>,
    windows_command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    windows_args: Option<Vec<String>>,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTask {
    description: Option<String>,
    run: Option<Value>,
    windows_run: Option<Value>,
    shell: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    #[serde(default)]
    hooks: Vec<String>,
}

/// Parses a task's `run` or `windows-run`: a string or an array of strings.
fn parse_run(task: &str, key: &str, value: Option<Value>) -> Result<Option<Vec<String>>> {
    Ok(match value {
        None => None,
        Some(Value::String(s)) => Some(vec![s]),
        Some(Value::Array(items)) => Some(
            items
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s),
                    _ => bail!("task `{task}`: `{key}` entries must be strings"),
                })
                .collect::<Result<_>>()?,
        ),
        Some(_) => bail!("task `{task}`: `{key}` must be a string or an array of strings"),
    })
}

/// Parses a `[tasks]` table (or `[workspace.tasks]`) and checks every `depends-on` names a task in it.
/// Parses a duration such as `500ms`, `30s`, `5m` or `1h` (a whole number and a unit, optionally separated by a space).
fn parse_duration(key: &str, value: &str) -> Result<Duration> {
    parse_duration_in("[test]", key, value)
}

/// A positive duration such as `500ms`, `30s`, `5m` or `1h`; `section` names where it was read, for the error.
pub(super) fn parse_duration_in(section: &str, key: &str, value: &str) -> Result<Duration> {
    let text = value.trim();
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (amount, unit) = text.split_at(digits);
    let unit_millis = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => 0,
    };
    match amount.parse::<u64>() {
        Ok(n) if n > 0 && unit_millis > 0 => Ok(Duration::from_millis(n.saturating_mul(unit_millis))),
        _ => bail!("{section} `{key}` must be a positive duration such as \"30s\", \"5m\" or \"1h\", not \"{value}\""),
    }
}

fn parse_tasks(table: &toml::Table) -> Result<BTreeMap<String, Task>> {
    let mut tasks = BTreeMap::new();
    for (name, value) in table {
        if name.is_empty() || name.contains(char::is_whitespace) {
            bail!("invalid task name `{name}`");
        }
        let raw: RawTask = match value {
            Value::String(cmd) => RawTask { run: Some(Value::String(cmd.clone())), ..Default::default() },
            Value::Table(_) => value.clone().try_into().with_context(|| format!("invalid task `{name}`"))?,
            _ => bail!("invalid task `{name}`: expected a command string or a table"),
        };
        let run = parse_run(name, "run", raw.run)?.unwrap_or_default();
        let windows_run = parse_run(name, "windows-run", raw.windows_run)?;
        if windows_run.is_some() && run.is_empty() {
            bail!("task `{name}`: `windows-run` needs a `run` to use on other platforms");
        }
        let shell = match raw.shell.as_deref() {
            None => None,
            Some(s) => Some(TaskShell::parse(s).with_context(|| {
                let known: Vec<_> = TaskShell::ALL.iter().map(|s| s.as_str()).collect();
                format!("task `{name}`: unknown shell `{s}`; expected one of {}", known.join(", "))
            })?),
        };
        if run.is_empty() && raw.depends_on.is_empty() {
            bail!("task `{name}` has nothing to do: add `run` or `depends-on`");
        }
        tasks.insert(
            name.clone(),
            Task {
                name: name.clone(),
                description: raw.description,
                run,
                windows_run,
                shell,
                depends_on: raw.depends_on,
                env: raw.env,
                cwd: raw.cwd,
                hooks: parse_hooks(&raw.hooks, &format!("task `{name}`"))?,
            },
        );
    }
    for task in tasks.values() {
        for dep in &task.depends_on {
            if !tasks.contains_key(dep) {
                bail!("task `{}` depends on unknown task `{dep}`", task.name);
            }
        }
    }
    Ok(tasks)
}

/// Whether `value` is an inheritance marker: `{ workspace = true }`.
fn is_inherited(value: &Value) -> bool {
    value.as_table().is_some_and(|t| t.contains_key("workspace"))
}

/// Replaces the `{ workspace = true }` entries of `[package]`, `[frameworks]`, `[plugins]` and
/// `[tasks]` in a member's manifest with the workspace's. Returns `None` when there is nothing to
/// inherit, so the manifest is parsed from its text (with line numbers in errors).
pub(super) fn inherit(mut table: toml::Table, workspace: Option<&WorkspaceManifest>) -> Result<Option<toml::Table>> {
    const SECTIONS: [&str; 4] = ["package", "frameworks", "plugins", "tasks"];
    let marked = SECTIONS.iter().any(|s| table.get(*s).and_then(Value::as_table).is_some_and(|t| t.values().any(is_inherited)));
    if !marked {
        return Ok(None);
    }
    for section in SECTIONS {
        let Some(Value::Table(entries)) = table.get_mut(section) else { continue };
        let keys: Vec<String> = entries.iter().filter(|(_, v)| is_inherited(v)).map(|(k, _)| k.clone()).collect();
        for key in keys {
            let marker = entries[&key].as_table().expect("checked above");
            if marker.get("workspace").and_then(Value::as_bool) != Some(true) || marker.len() > 1 {
                bail!("[{section}] `{key}`: inherit it with `{key} = {{ workspace = true }}` and nothing else");
            }
            let Some(workspace) = workspace else {
                bail!("[{section}] `{key}`: `workspace = true` needs a [workspace] that lists this package in `members`");
            };
            let (shared, from) = match section {
                "package" => (&workspace.package, "workspace.package"),
                "frameworks" => (&workspace.frameworks, "workspace.frameworks"),
                "plugins" => (&workspace.plugins, "workspace.plugins"),
                _ => (&workspace.raw_tasks, "workspace.tasks"),
            };
            let Some(value) = shared.get(&key) else {
                bail!("[{section}] `{key}` inherits from the workspace, but [{from}] has no `{key}`");
            };
            let mut value = value.clone();
            if section == "plugins" {
                // A plugin command next to the workspace's kargo.toml stays there.
                if let Some(Value::String(command)) = value.get("command")
                    && (command.starts_with("./") || command.starts_with("../") || command.contains(['/', '\\']))
                    && !Path::new(command).is_absolute()
                {
                    let absolute = workspace.root.join(command).to_string_lossy().into_owned();
                    value.as_table_mut().expect("a plugin is a table").insert("command".to_owned(), Value::String(absolute));
                }
            }
            entries.insert(key, value);
        }
        // A task inherited from the workspace brings the workspace tasks it depends on.
        if section == "tasks"
            && let Some(workspace) = workspace
        {
            loop {
                let missing: Vec<String> = entries
                    .values()
                    .filter_map(|v| v.get("depends-on").and_then(Value::as_array))
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|dep| !entries.contains_key(*dep) && workspace.raw_tasks.contains_key(*dep))
                    .map(str::to_owned)
                    .collect();
                if missing.is_empty() {
                    break;
                }
                for dep in missing {
                    entries.insert(dep.clone(), workspace.raw_tasks[&dep].clone());
                }
            }
        }
    }
    Ok(Some(table))
}

/// A parsed `[dependencies]` entry: a Maven module or another Kargo package on disk.
enum Entry {
    Maven(Dependency),
    Path(PathDependency),
}

/// Replaces a `{ workspace = true }` entry with the matching `[workspace.dependencies]` entry.
fn inherit_dependency(name: &str, value: &Value, section: &str, workspace: Option<&WorkspaceManifest>) -> Result<Option<Value>> {
    let Value::Table(table) = value else { return Ok(None) };
    let Some(flag) = table.get("workspace") else { return Ok(None) };
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    if flag.as_bool() != Some(true) {
        bail!("{}: `workspace` can only be `true`", ctx());
    }
    if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "workspace" | "scope" | "exported" | "test-fixtures")) {
        bail!("{}: `{other}` cannot be combined with `workspace = true`; set it in [workspace.dependencies] instead", ctx());
    }
    let Some(workspace) = workspace else {
        bail!("{}: `workspace = true` needs a [workspace] that lists this package in `members`", ctx());
    };
    let Some(inherited) = workspace.dependencies.get(name) else {
        bail!("{}: `{name}` is not declared in [workspace.dependencies]", ctx());
    };
    let mut inherited = match inherited {
        Value::String(version) => toml::Table::from_iter([("version".to_owned(), Value::String(version.clone()))]),
        Value::Table(t) => t.clone(),
        _ => bail!("{}: the [workspace.dependencies] entry must be a version string or a table", ctx()),
    };
    // Workspace paths are relative to the workspace root, not to the member.
    if let Some(Value::String(path)) = inherited.get("path") {
        let absolute = workspace.root.join(path);
        inherited.insert("path".to_owned(), Value::String(absolute.to_string_lossy().into_owned()));
    }
    for key in ["scope", "exported", "test-fixtures"] {
        if let Some(value) = table.get(key) {
            inherited.insert(key.to_owned(), value.clone());
        }
    }
    Ok(Some(Value::Table(inherited)))
}

fn parse_dependency(name: &str, value: &Value, section: &str, workspace: Option<&WorkspaceManifest>) -> Result<Entry> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    let inherited = inherit_dependency(name, value, section, workspace)?;
    let value = inherited.as_ref().unwrap_or(value);
    let detailed: DetailedDependency = match value {
        Value::String(version) => DetailedDependency { version: Some(version.clone()), ..Default::default() },
        Value::Table(_) => value.clone().try_into().with_context(ctx)?,
        _ => bail!("{}: expected a version string or a table", ctx()),
    };

    if let Some(path) = detailed.path.clone() {
        return path_dependency(name, path, detailed, section).map(Entry::Path);
    }
    if detailed.test_fixtures {
        bail!("{}: `test-fixtures` only applies to a `path` dependency on another Kargo package", ctx());
    }
    let module: ModuleId = if name.contains(':') {
        if detailed.coords.is_some() || detailed.group.is_some() || detailed.artifact.is_some() {
            bail!("{}: `coords`, `group` and `artifact` are not allowed when the key is already `group:artifact`", ctx());
        }
        name.parse().with_context(ctx)?
    } else if let Some(coords) = &detailed.coords {
        if detailed.group.is_some() || detailed.artifact.is_some() {
            bail!("{}: use either `coords` or `group`/`artifact`, not both", ctx());
        }
        let spec: CoordSpec = coords.parse().with_context(ctx)?;
        if spec.version.is_some() && detailed.version.is_some() {
            bail!("{}: version given both in `coords` and `version`", ctx());
        }
        let version = spec.version.clone().or(detailed.version.clone());
        return finish_dependency(name, spec.module, version, detailed, section).map(Entry::Maven);
    } else if let Some(group) = &detailed.group {
        ModuleId::new(group, detailed.artifact.clone().unwrap_or_else(|| name.to_owned())).to_string().parse().with_context(ctx)?
    } else {
        bail!("{}: the key must be `group:artifact`, or the table must specify `coords`, `group` or `path`", ctx());
    };
    let version = detailed.version.clone();
    finish_dependency(name, module, version, detailed, section).map(Entry::Maven)
}

/// `name = { path = "../name" }`: another Kargo package, built from source.
fn path_dependency(name: &str, path: PathBuf, detailed: DetailedDependency, section: &str) -> Result<PathDependency> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    let maven_only = [
        ("group", detailed.group.is_some()),
        ("artifact", detailed.artifact.is_some()),
        ("coords", detailed.coords.is_some()),
        ("version", detailed.version.is_some()),
        ("classifier", detailed.classifier.is_some()),
        ("extension", detailed.extension.is_some()),
        ("exclude", !detailed.exclude.is_empty()),
        ("force", detailed.force),
        ("transitive", detailed.transitive.is_some()),
        ("bom", detailed.bom),
    ];
    if let Some((key, _)) = maven_only.iter().find(|(_, set)| *set) {
        bail!(
            "{}: `{key}` does not apply to a `path` dependency, which is built from source (only `scope`, `exported` and `test-fixtures` can be set)",
            ctx()
        );
    }
    validate_package_name(name).with_context(|| format!("{}: the key of a `path` dependency is the package name", ctx()))?;
    if path.as_os_str().is_empty() {
        bail!("{}: `path` must not be empty", ctx());
    }
    let scope = match &detailed.scope {
        Some(s) => s.parse().with_context(ctx)?,
        None => Scope::Compile,
    };
    if matches!(scope, Scope::Processor | Scope::Ksp) {
        bail!("{}: a `path` dependency can't be an annotation or KSP processor yet; depend on it as `compile` or `runtime`", ctx());
    }
    Ok(PathDependency {
        name: name.to_owned(),
        path,
        scope,
        dev: false,
        target: None,
        exported: detailed.exported,
        test_fixtures: detailed.test_fixtures,
    })
}

fn finish_dependency(
    name: &str,
    module: ModuleId,
    version: Option<String>,
    detailed: DetailedDependency,
    section: &str,
) -> Result<Dependency> {
    let ctx = || format!("invalid dependency `{name}` in [{section}]");
    if let Some(v) = &version {
        validate_version_string("version", v).with_context(ctx)?;
    }
    if detailed.bom && version.is_none() {
        bail!("{}: a BOM needs a version", ctx());
    }
    let exclusions = detailed
        .exclude
        .iter()
        .map(|e| {
            e.parse::<ModuleId>()
                .or_else(|_| match e.split_once(':') {
                    Some((g, "*")) if !g.is_empty() => Ok(ModuleId::new(g, "*")),
                    _ => Err(anyhow::anyhow!("invalid exclusion `{e}` (expected `group:artifact` or `group:*`)")),
                })
                .with_context(ctx)
        })
        .collect::<Result<Vec<_>>>()?;
    let scope = match &detailed.scope {
        Some(s) => s.parse().with_context(ctx)?,
        None => Scope::Compile,
    };
    Ok(Dependency {
        name: name.to_owned(),
        module,
        version,
        scope,
        classifier: detailed.classifier,
        extension: detailed.extension,
        exclusions,
        force: detailed.force,
        transitive: detailed.transitive.unwrap_or(true),
        bom: detailed.bom,
        exported: detailed.exported,
    })
}

/// Parses the dependency tables of one manifest, collecting `path` dependencies on the side.
struct DependencyParser<'a> {
    workspace: Option<&'a WorkspaceManifest>,
    paths: Vec<PathDependency>,
}

impl DependencyParser<'_> {
    fn parse(&mut self, table: &toml::Table, section: &str, dev: bool, target: Option<TargetKind>) -> Result<Vec<Dependency>> {
        let mut deps: Vec<Dependency> = Vec::new();
        for (name, value) in table {
            match parse_dependency(name, value, section, self.workspace)? {
                Entry::Maven(dep) => {
                    if deps.iter().any(|d| d.same_artifact(&dep)) {
                        bail!("dependency `{}` is declared more than once in [{section}]", dep.module);
                    }
                    deps.push(dep);
                }
                Entry::Path(path) => {
                    if path.test_fixtures && !dev {
                        bail!("invalid dependency `{name}` in [{section}]: `test-fixtures` is for tests; declare it in [dev-dependencies]");
                    }
                    self.paths.push(PathDependency { dev, target, ..path })
                }
            }
        }
        Ok(deps)
    }
}

fn parse_hooks(hooks: &[String], ctx: &str) -> Result<Vec<Hook>> {
    hooks.iter().map(|h| h.parse::<Hook>().with_context(|| ctx.to_owned())).collect()
}

const KONAN_TARGETS: &[&str] = &[
    "host",
    "linux_x64",
    "linux_arm64",
    "mingw_x64",
    "macos_x64",
    "macos_arm64",
    "ios_arm64",
    "ios_x64",
    "ios_simulator_arm64",
    "watchos_arm64",
    "watchos_simulator_arm64",
    "tvos_arm64",
    "tvos_simulator_arm64",
    "android_arm64",
    "android_arm32",
    "android_x64",
    "android_x86",
];

/// Accepts both Konan names (`linux_x64`) and Gradle-style names (`linuxX64`).
pub fn normalize_konan_target(name: &str) -> Result<String> {
    let mut snake = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
    }
    let snake = snake.replace('-', "_");
    let snake = match snake.as_str() {
        "android_native_arm64" => "android_arm64".to_owned(),
        "android_native_arm32" => "android_arm32".to_owned(),
        "android_native_x64" => "android_x64".to_owned(),
        "android_native_x86" => "android_x86".to_owned(),
        _ => snake,
    };
    if !KONAN_TARGETS.contains(&snake.as_str()) {
        bail!("unknown Kotlin/Native platform `{name}` (expected one of: {})", KONAN_TARGETS.join(", "));
    }
    Ok(snake)
}

impl RawManifest {
    pub(super) fn validate(self, workspace: Option<&WorkspaceManifest>) -> Result<Manifest> {
        validate_package_name(&self.package.name)?;
        validate_exact_version("[package] `version`", "0.1.0", &self.package.version)?;
        if let Some(group) = &self.package.group
            && (group.is_empty()
                || group.split('.').any(|seg| seg.is_empty() || !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')))
        {
            bail!("invalid package group `{group}` (expected something like `com.example`)");
        }

        let workspace_kotlin = workspace.and_then(|w| w.kotlin_version.clone());
        let (kotlin_version, kotlin_version_set) = match (self.kotlin.version.clone(), workspace_kotlin) {
            (Some(own), Some(shared)) if own != shared => bail!(
                "[kotlin] version {own} differs from the workspace's [workspace.kotlin] version {shared};                  remove it to use the workspace's, since every member builds with the same Kotlin"
            ),
            (Some(v), _) | (None, Some(v)) => (v, true),
            (None, None) => (DEFAULT_KOTLIN_VERSION.to_owned(), false),
        };
        validate_exact_version("[kotlin] `version`", DEFAULT_KOTLIN_VERSION, &kotlin_version)?;
        // `[workspace.kotlin]` is where every member starts: its lists come first, and a member's
        // own keys take precedence over the others.
        let mut kotlin = self.kotlin;
        if let Some(shared) = workspace.map(|w| &w.kotlin) {
            kotlin.language_version = kotlin.language_version.or_else(|| shared.language_version.clone());
            kotlin.api_version = kotlin.api_version.or_else(|| shared.api_version.clone());
            kotlin.ksp = kotlin.ksp.or_else(|| shared.ksp_version.clone());
            kotlin.warnings_as_errors |= shared.warnings_as_errors;
            let mut opt_in = shared.opt_in.clone();
            opt_in.extend(kotlin.opt_in.into_iter().filter(|o| !shared.opt_in.contains(o)));
            kotlin.opt_in = opt_in;
            let mut compiler_args = shared.compiler_args.clone();
            compiler_args.extend(kotlin.compiler_args);
            kotlin.compiler_args = compiler_args;
        }
        let shared_jvm_target = workspace.and_then(|w| w.kotlin.jvm_target.clone());
        if let Some(ksp) = &kotlin.ksp {
            validate_exact_version("[kotlin] `ksp`", "1.2.3", ksp)?;
        }
        let mut deps = DependencyParser { workspace, paths: Vec::new() };

        let mut targets = match self.target {
            // No [target] section means a plain JVM project.
            None => Targets {
                jvm: Some(JvmTarget {
                    jvm_target: shared_jvm_target.clone().unwrap_or_else(|| DEFAULT_JVM_TARGET.to_owned()),
                    jvm_target_set: shared_jvm_target.is_some(),
                    main_class: None,
                    jvm_args: Vec::new(),
                    javac_args: Vec::new(),
                    dependencies: Vec::new(),
                }),
                ..Default::default()
            },
            Some(raw) => {
                let targets = Targets {
                    jvm: raw
                        .jvm
                        .map(|j| -> Result<JvmTarget> {
                            Ok(JvmTarget {
                                jvm_target_set: j.jvm_target.is_some() || shared_jvm_target.is_some(),
                                jvm_target: match j.jvm_target.or_else(|| shared_jvm_target.clone()) {
                                    Some(target) => normalize_jvm_target(&target)?,
                                    None => DEFAULT_JVM_TARGET.to_owned(),
                                },
                                main_class: j.main_class,
                                jvm_args: j.jvm_args,
                                javac_args: j.javac_args,
                                dependencies: deps.parse(&j.dependencies, "target.jvm.dependencies", false, Some(TargetKind::Jvm))?,
                            })
                        })
                        .transpose()?,
                    wasm: raw
                        .wasm
                        .map(|w| -> Result<WasmTarget> {
                            let environment = match w.environment.as_deref() {
                                None | Some("js") => WasmEnvironment::Js,
                                Some("browser") => WasmEnvironment::Browser,
                                Some("wasi") => WasmEnvironment::Wasi,
                                Some(other) => bail!("unknown wasm environment `{other}` (expected `js`, `browser` or `wasi`)"),
                            };
                            let kind = match w.kind.as_deref() {
                                None | Some("application") | Some("app") => WasmKind::Application,
                                Some("library") | Some("lib") => WasmKind::Library,
                                Some(other) => bail!("unknown wasm kind `{other}` (expected `application` or `library`)"),
                            };
                            if kind == WasmKind::Library && environment == WasmEnvironment::Wasi {
                                bail!(
                                    "[target.wasm] kind = \"library\" builds an npm package, which needs environment = \"js\" or \"browser\", not \"wasi\""
                                );
                            }
                            if let Some(name) = &w.module_name {
                                validate_module_name(name)?;
                            }
                            if let Some(name) = &w.npm_name {
                                if kind != WasmKind::Library {
                                    bail!("[target.wasm] `npm-name` names the package of a library; set kind = \"library\" or remove it");
                                }
                                validate_npm_name(name)?;
                            }
                            for (name, version) in &w.npm_dependencies {
                                if name.trim().is_empty() || version.trim().is_empty() {
                                    bail!("[target.wasm.npm-dependencies] needs a package name and a version for every entry");
                                }
                            }
                            let wasm_opt = match w.wasm_opt {
                                None | Some(Value::Boolean(true)) => WasmOpt::Auto,
                                Some(Value::Boolean(false)) => WasmOpt::Off,
                                Some(Value::String(path)) if !path.trim().is_empty() => WasmOpt::Path(PathBuf::from(path)),
                                Some(_) => bail!("[target.wasm] `wasm-opt` must be true, false or the path of a wasm-opt executable"),
                            };
                            let package_manager = w.package_manager.as_deref().map(str::parse).transpose()?;
                            Ok(WasmTarget {
                                environment,
                                kind,
                                module_name: w.module_name,
                                npm_name: w.npm_name,
                                package_manager,
                                npm_dependencies: w.npm_dependencies,
                                wasm_opt,
                                dependencies: deps.parse(&w.dependencies, "target.wasm.dependencies", false, Some(TargetKind::Wasm))?,
                            })
                        })
                        .transpose()?,
                    native: raw
                        .native
                        .map(|n| -> Result<NativeTarget> {
                            let platforms = if n.platforms.is_empty() {
                                vec!["host".to_owned()]
                            } else {
                                n.platforms.iter().map(|p| normalize_konan_target(p)).collect::<Result<Vec<_>>>()?
                            };
                            let kind = match n.kind.as_deref() {
                                None | Some("executable") | Some("program") => NativeKind::Executable,
                                Some("library") | Some("klib") => NativeKind::Library,
                                Some("shared") | Some("dynamic") => NativeKind::Shared,
                                Some("static") => NativeKind::Static,
                                Some("framework") => NativeKind::Framework,
                                Some(other) => {
                                    bail!("unknown native kind `{other}` (expected executable, library, shared, static or framework)")
                                }
                            };
                            if kind == NativeKind::Framework
                                && let Some(other) = platforms.iter().find(|p| p.as_str() != "host" && !is_apple_target(p))
                            {
                                bail!(
                                    "[target.native] kind = \"framework\" builds Apple frameworks, but `platforms` lists `{other}`; \
                                     list only macOS, iOS, tvOS or watchOS platforms"
                                );
                            }
                            if n.bundle_id.is_some() && kind != NativeKind::Framework {
                                bail!("[target.native] `bundle-id` only applies to kind = \"framework\"");
                            }
                            if !n.export.is_empty() && !matches!(kind, NativeKind::Framework | NativeKind::Shared | NativeKind::Static) {
                                bail!("[target.native] `export` only applies to kind = \"framework\", \"shared\" or \"static\"");
                            }
                            if let Some(bad) = n.export.iter().find(|e| e.split(':').count() != 2 || e.split(':').any(str::is_empty)) {
                                bail!("invalid entry `{bad}` in [target.native] export (expected `group:artifact` of a dependency)");
                            }
                            let cinterop = n
                                .cinterop
                                .into_iter()
                                .map(|(name, c)| validate_cinterop(name, c))
                                .collect::<Result<Vec<_>>>()?;
                            let platform = platform_tables("target.native", n.platform, RawLinkOptions::into_options)?;
                            let options = NativeOptions {
                                compiler_options: Vec::new(),
                                linker_options: n.linker_options,
                                static_libraries: n.static_libraries,
                                library_paths: n.library_paths,
                                frameworks: n.frameworks,
                            };
                            validate_options("target.native", &options, &platform)?;
                            Ok(NativeTarget {
                                platforms,
                                kind,
                                entry_point: n.entry_point,
                                options,
                                platform,
                                cinterop,
                                c_sources: n.c_sources.map(validate_c_sources).transpose()?,
                                bundle_id: n.bundle_id,
                                export: n.export,
                                dependencies: deps.parse(&n.dependencies, "target.native.dependencies", false, Some(TargetKind::Native))?,
                            })
                        })
                        .transpose()?,
                    android: raw
                        .android
                        .map(|mut table| -> Result<AndroidTarget> {
                            let dependencies = match table.remove("dependencies") {
                                None => toml::Table::new(),
                                Some(Value::Table(t)) => t,
                                Some(_) => bail!("[target.android.dependencies] must be a table"),
                            };
                            let config: AndroidConfig = Value::Table(table).try_into().context("invalid [target.android] section")?;
                            validate_android(&config)?;
                            let dependencies =
                                deps.parse(&dependencies, "target.android.dependencies", false, Some(TargetKind::Android))?;
                            Ok(AndroidTarget { config, dependencies })
                        })
                        .transpose()?,
                };
                if targets.enabled().is_empty() {
                    bail!("[target] is present but no target is enabled; add e.g. [target.jvm]");
                }
                targets
            }
        };

        let mut repositories = vec![Repository::maven_central()];
        if targets.android.is_some() {
            repositories.push(Repository::google());
        }
        for (name, value) in &self.repositories {
            let raw: RawRepository = match value {
                Value::String(url) => RawRepository { url: url.clone(), username_env: None, password_env: None },
                Value::Table(_) => value.clone().try_into().with_context(|| format!("invalid repository `{name}`"))?,
                _ => bail!("invalid repository `{name}`: expected a URL or a table"),
            };
            let url = raw.url.trim();
            let mut repo = if url == kargo_maven::repo::MAVEN_LOCAL {
                Repository { name: name.clone(), ..Repository::maven_local()? }
            } else if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("file:") {
                Repository::new(name.clone(), url)
            } else {
                bail!("repository `{name}` must have an http(s):// or file: URL, or be \"maven-local\", got `{url}`");
            };
            repo.credentials = credentials_from_env(name, raw.username_env.as_deref(), raw.password_env.as_deref())?;
            if !repositories.iter().any(|r| r.url == repo.url) {
                repositories.push(repo);
            }
        }

        let frameworks = Frameworks {
            compose: self.frameworks.compose.and_then(Toggle::into_option),
            serialization: self.frameworks.serialization.and_then(Toggle::into_option),
            coroutines: match self.frameworks.coroutines {
                None | Some(Value::Boolean(false)) => None,
                Some(Value::Boolean(true)) => Some(CoroutinesConfig { version: None }),
                Some(Value::String(v)) => {
                    validate_exact_version("[frameworks] `coroutines`", "1.2.3", &v)?;
                    Some(CoroutinesConfig { version: Some(v) })
                }
                Some(_) => bail!("[frameworks] `coroutines` must be true, false or a version string"),
            },
            ktor: self.frameworks.ktor.and_then(Toggle::into_option),
            lombok: self.frameworks.lombok.and_then(Toggle::into_option),
            spring_boot: self.frameworks.spring_boot.and_then(Toggle::into_option),
            benchmark: self.frameworks.benchmark.and_then(Toggle::into_option),
        };
        for (name, version) in [
            ("compose", frameworks.compose.as_ref().and_then(|c| c.version.as_deref())),
            ("serialization", frameworks.serialization.as_ref().and_then(|c| c.version.as_deref())),
            ("ktor", frameworks.ktor.as_ref().and_then(|c| c.version.as_deref())),
            ("lombok", frameworks.lombok.as_ref().and_then(|c| c.version.as_deref())),
            ("spring-boot", frameworks.spring_boot.as_ref().and_then(|c| c.version.as_deref())),
        ] {
            if let Some(v) = version {
                validate_exact_version(&format!("[frameworks.{name}] `version`"), "1.2.3", v)?;
            }
        }
        if let Some(package) = frameworks.compose.as_ref().and_then(|c| c.resources_package.as_deref())
            && (package.is_empty()
                || !package.split('.').all(|part| {
                    part.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                        && part.chars().all(|c| c.is_alphanumeric() || c == '_')
                }))
        {
            bail!("[frameworks.compose] `resources-package` must be a Kotlin package name such as \"com.example.res\", got `{package}`");
        }
        // Compose Multiplatform depends on androidx artifacts hosted by Google.
        if frameworks.compose.is_some() && !repositories.iter().any(|r| r.url == kargo_maven::repo::GOOGLE) {
            repositories.insert(1, Repository::google());
        }
        if let Some(ktor) = &frameworks.ktor
            && ktor.server.is_none()
            && ktor.client.is_none()
        {
            bail!("[frameworks.ktor] enable at least one of `server = \"netty\"` or `client = \"cio\"`");
        }

        let mut compiler_plugins = Vec::new();
        let mut process_plugins = Vec::new();
        for (name, value) in &self.plugins {
            let raw: RawPlugin = value.clone().try_into().with_context(|| format!("invalid plugin `{name}`"))?;
            match (&raw.maven, &raw.command) {
                (Some(maven), None) => {
                    if !raw.hooks.is_empty()
                        || !raw.args.is_empty()
                        || !raw.env.is_empty()
                        || raw.windows_command.is_some()
                        || raw.windows_args.is_some()
                    {
                        bail!(
                            "plugin `{name}`: `hooks`, `args`, `env`, `windows-command` and `windows-args` only apply to `command` plugins"
                        );
                    }
                    let artifact: CoordSpec = maven.parse().with_context(|| format!("invalid plugin `{name}`"))?;
                    if artifact.version.is_none() {
                        bail!("plugin `{name}`: `maven` must include a version (`group:artifact:version`)");
                    }
                    if !raw.options.is_empty() && raw.id.is_none() {
                        bail!("plugin `{name}`: `options` require the compiler plugin `id`");
                    }
                    compiler_plugins.push(CompilerPlugin { name: name.clone(), artifact, id: raw.id, options: raw.options });
                }
                (None, Some(command)) => {
                    if raw.id.is_some() || !raw.options.is_empty() {
                        bail!("plugin `{name}`: `id` and `options` only apply to `maven` compiler plugins");
                    }
                    if raw.hooks.is_empty() {
                        bail!("plugin `{name}`: a `command` plugin needs at least one hook (e.g. hooks = [\"post-build\"])");
                    }
                    process_plugins.push(ProcessPlugin {
                        name: name.clone(),
                        command: command.clone(),
                        windows_command: raw.windows_command,
                        args: raw.args,
                        windows_args: raw.windows_args,
                        hooks: parse_hooks(&raw.hooks, &format!("plugin `{name}`"))?,
                        env: raw.env,
                    });
                }
                (Some(_), Some(_)) => bail!("plugin `{name}`: specify either `maven` or `command`, not both"),
                (None, None) if raw.windows_command.is_some() => {
                    bail!("plugin `{name}`: `windows-command` needs a `command` to use on other platforms")
                }
                (None, None) => bail!("plugin `{name}`: specify `maven = \"group:artifact:version\"` or `command = \"...\"`"),
            }
        }

        let tasks = parse_tasks(&self.tasks)?;

        let jdk = match self.toolchain.jdk {
            None => JdkRequest::System,
            Some(Value::String(s)) => s.parse().context("invalid [toolchain] `jdk`")?,
            Some(Value::Integer(i)) => {
                u32::try_from(i).ok().map(|m| m.to_string()).unwrap_or_default().parse().context("invalid [toolchain] `jdk`")?
            }
            Some(_) => bail!("[toolchain] `jdk` must be a version like \"21\" or \"system\""),
        };
        let graalvm = match self.toolchain.graalvm {
            None => GraalvmRequest::System,
            Some(Value::String(s)) => s.parse().context("invalid [toolchain] `graalvm`")?,
            Some(Value::Integer(i)) => i.to_string().parse().context("invalid [toolchain] `graalvm`")?,
            Some(_) => bail!("[toolchain] `graalvm` must be a version like \"25\", \"oracle-25\" or \"system\""),
        };
        let kargo = match self.toolchain.kargo {
            None => None,
            Some(text) => {
                let req = semver::VersionReq::parse(text.trim()).with_context(|| {
                    format!("invalid [toolchain] `kargo` requirement `{text}`: use a requirement like \">=0.3\" or \"^0.3.1\"")
                })?;
                check_kargo_version(&req, KARGO_VERSION)?;
                Some(req)
            }
        };
        let packaging = PackagingSettings {
            jlink: self.package.jlink.validate()?,
            native_image: self.package.native_image.validate(),
            installer: self.package.installer.validate()?,
        };
        if let (Some(major), Some(jvm)) = (jdk.major(), &mut targets.jvm) {
            // The default target never exceeds the pinned JDK; an explicit one must not.
            jvm.jvm_target = jvm.target_for(major)?;
        }

        let dependencies = deps.parse(&self.dependencies, "dependencies", false, None)?;
        let dev_dependencies = deps.parse(&self.dev_dependencies, "dev-dependencies", true, None)?;
        let profiles = resolve_profiles(self.profile, &mut deps)?;
        let path_dependencies = deps.paths;
        for (i, dep) in path_dependencies.iter().enumerate() {
            if path_dependencies[..i].iter().any(|d| d.name == dep.name && d.dev == dep.dev && d.target == dep.target) {
                bail!("path dependency `{}` is declared more than once", dep.name);
            }
        }

        let bins = validate_bins(self.bin)?;
        let doc = self.doc.validate()?;
        let fmt = self.fmt.validate()?;
        let lint = self.lint.validate()?;

        let publish = self.publish.validate(&repositories)?;
        for (key, url) in [("homepage", &self.package.homepage), ("repository", &self.package.repository)] {
            if let Some(url) = url
                && !(url.starts_with("https://") || url.starts_with("http://"))
            {
                bail!("[package] `{key}` must be an http(s):// URL, got `{url}`");
            }
        }

        Ok(Manifest {
            publish,
            toolchain: ToolchainSettings { jdk, graalvm, kargo },
            packaging,
            doc,
            fmt,
            lint,
            test: TestSettings {
                fail_fast: self.test.fail_fast.unwrap_or(true),
                timeout: self.test.timeout.as_deref().map(|v| parse_duration("timeout", v)).transpose()?,
                test_timeout: self.test.test_timeout.as_deref().map(|v| parse_duration("test-timeout", v)).transpose()?,
                jvm_args: self.test.jvm_args,
            },
            bench: BenchSettings {
                warmup: self.bench.warmup.as_deref().map(|v| BenchRounds::parse("[bench] `warmup`", v)).transpose()?,
                iterations: self.bench.iterations.as_deref().map(|v| BenchRounds::parse("[bench] `iterations`", v)).transpose()?,
                forks: self.bench.forks,
                mode: self.bench.mode.as_deref().map(str::parse).transpose().context("invalid [bench] `mode`")?,
                time_unit: self.bench.time_unit.as_deref().map(|v| validate_time_unit("[bench] `time-unit`", v)).transpose()?,
                jvm_args: self.bench.jvm_args,
            },
            package: Package {
                name: self.package.name,
                version: self.package.version,
                group: self.package.group,
                description: self.package.description,
                authors: self.package.authors,
                license: self.package.license,
                homepage: self.package.homepage,
                repository: self.package.repository,
                publish: self.package.publish.unwrap_or(true),
            },
            kotlin: KotlinSettings {
                version: kotlin_version,
                version_set: kotlin_version_set,
                language_version: kotlin.language_version,
                api_version: kotlin.api_version,
                ksp_version: kotlin.ksp,
                opt_in: kotlin.opt_in,
                compiler_args: kotlin.compiler_args,
                warnings_as_errors: kotlin.warnings_as_errors,
            },
            targets,
            bins,
            dependencies,
            dev_dependencies,
            path_dependencies,
            repositories,
            frameworks,
            compiler_plugins,
            process_plugins,
            tasks,
            env: parse_env(&self.env, "[env]", true)?,
            profiles,
        })
    }
}

/// Validates `[[bin]]`: unique names, and a `main`/`entry-point` or a `path`, not both.
fn validate_bins(raw: Vec<RawBin>) -> Result<Vec<Bin>> {
    let mut bins: Vec<Bin> = Vec::new();
    for b in raw {
        let name = b.name;
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid [[bin]] name `{name}` (start with a letter; use letters, digits, `-` and `_`)");
        }
        if bins.iter().any(|other| other.name == name) {
            bail!("[[bin]] `{name}` is declared more than once");
        }
        let is_class = |s: &str| !s.is_empty() && s.split('.').all(|seg| !seg.is_empty() && !seg.contains(char::is_whitespace));
        for (key, value) in [("main", &b.main), ("entry-point", &b.entry_point)] {
            if let Some(v) = value
                && !is_class(v)
            {
                bail!("[[bin]] `{name}`: invalid `{key}` `{v}` (expected a qualified name such as `com.example.tools.MigrateKt`)");
            }
        }
        match (&b.path, b.main.is_some() || b.entry_point.is_some()) {
            (Some(_), true) => bail!("[[bin]] `{name}`: set either `path` or `main`/`entry-point`, not both"),
            (None, false) => {
                bail!(
                    "[[bin]] `{name}` needs an entry point: set `path` to the file with its `main`, or `main` (JVM) and `entry-point` (Native)"
                )
            }
            _ => {}
        }
        if let Some(path) = &b.path
            && path.extension().is_none_or(|e| e != "kt")
        {
            bail!("[[bin]] `{name}`: `path` must be a Kotlin source file (`.kt`), got `{}`", path.display());
        }
        bins.push(Bin { name, main: b.main, entry_point: b.entry_point, path: b.path });
    }
    Ok(bins)
}

/// Fails unless `version` (a tool version from `[section] key`) is one exact version.
fn validate_tool_version(section: &str, key: &str, tool: &str, example: &str, version: Option<&str>) -> Result<()> {
    if let Some(v) = version
        && (v.is_empty() || v.contains(|c: char| c.is_whitespace() || "[](),".contains(c)))
    {
        bail!("[{section}] `{key}` must be an exact {tool} version such as \"{example}\", got `{v}`");
    }
    Ok(())
}

/// `kargo.toml` of a workspace: only a `[workspace]` table.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RawWorkspaceManifest {
    workspace: RawWorkspace,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    default_members: Vec<String>,
    #[serde(default)]
    package: toml::Table,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default)]
    kotlin: RawWorkspaceKotlin,
    #[serde(default)]
    frameworks: toml::Table,
    #[serde(default)]
    plugins: toml::Table,
    #[serde(default)]
    tasks: toml::Table,
}

/// `[workspace.package]`: the `[package]` keys members can inherit. Deserialized to reject unknown
/// keys; members validate the values they inherit.
#[allow(dead_code)]
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWorkspacePackage {
    version: Option<String>,
    group: Option<String>,
    description: Option<String>,
    authors: Option<Vec<String>>,
    license: Option<String>,
    homepage: Option<String>,
    repository: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawWorkspaceKotlin {
    version: Option<String>,
    language_version: Option<String>,
    api_version: Option<String>,
    ksp: Option<String>,
    #[serde(default)]
    opt_in: Vec<String>,
    #[serde(default)]
    compiler_args: Vec<String>,
    #[serde(default)]
    warnings_as_errors: bool,
    jvm_target: Option<String>,
}

/// A `members`, `exclude` or `default-members` entry: a relative directory, or (in `members`) a glob.
fn workspace_dir(key: &str, entry: &str, globs: bool) -> Result<()> {
    let bad_glob = if globs { entry.contains(['[', ']', '{', '}']) || entry.contains("**") } else { entry.contains(['*', '?', '[']) };
    if entry.trim().is_empty() || bad_glob || entry.starts_with(['/', '\\']) || Path::new(entry).is_absolute() {
        let expected = if globs { "directories such as \"app\" or globs such as \"libs/*\"" } else { "directories such as \"app\"" };
        bail!("[workspace] `{key}` entries must be {expected}, relative to the workspace root; got `{entry}`");
    }
    Ok(())
}

impl RawWorkspaceManifest {
    pub(super) fn validate(self, root: &Path) -> Result<WorkspaceManifest> {
        let raw = self.workspace;
        let mut members: Vec<String> = Vec::new();
        for member in &raw.members {
            workspace_dir("members", member, true)?;
            if members.contains(member) {
                bail!("[workspace] member `{member}` is listed more than once");
            }
            members.push(member.clone());
        }
        for entry in raw.exclude.iter().chain(&raw.default_members) {
            let key = if raw.exclude.contains(entry) { "exclude" } else { "default-members" };
            workspace_dir(key, entry, false)?;
        }
        let kotlin = raw.kotlin;
        for (key, value) in [("version", &kotlin.version), ("ksp", &kotlin.ksp)] {
            if let Some(v) = value {
                validate_exact_version(&format!("[workspace.kotlin] `{key}`"), DEFAULT_KOTLIN_VERSION, v)?;
            }
        }
        let jvm_target =
            kotlin.jvm_target.as_deref().map(normalize_jvm_target).transpose().context("invalid [workspace.kotlin] `jvm-target`")?;

        for (section, table) in
            [("package", &raw.package), ("frameworks", &raw.frameworks), ("plugins", &raw.plugins), ("tasks", &raw.tasks)]
        {
            if let Some(key) = table.iter().find(|(_, v)| is_inherited(v)).map(|(k, _)| k) {
                bail!("[workspace.{section}] `{key}`: `workspace = true` is only valid in a member's kargo.toml");
            }
        }
        let package: RawWorkspacePackage = Value::Table(raw.package.clone()).try_into().context("invalid [workspace.package]")?;
        if let Some(v) = &package.version {
            validate_exact_version("[workspace.package] `version`", "0.1.0", v)?;
        }
        let _: RawFrameworks = Value::Table(raw.frameworks.clone()).try_into().context("invalid [workspace.frameworks]")?;
        for (name, value) in &raw.plugins {
            let _: RawPlugin = value.clone().try_into().with_context(|| format!("invalid plugin `{name}` in [workspace.plugins]"))?;
        }
        let tasks = parse_tasks(&raw.tasks).context("invalid [workspace.tasks]")?;

        let workspace = WorkspaceManifest {
            root: root.to_path_buf(),
            members,
            exclude: raw.exclude.iter().map(PathBuf::from).collect(),
            default_members: raw.default_members.iter().map(PathBuf::from).collect(),
            kotlin_version: kotlin.version,
            kotlin: WorkspaceKotlin {
                language_version: kotlin.language_version,
                api_version: kotlin.api_version,
                ksp_version: kotlin.ksp,
                opt_in: kotlin.opt_in,
                compiler_args: kotlin.compiler_args,
                warnings_as_errors: kotlin.warnings_as_errors,
                jvm_target,
            },
            tasks,
            dependencies: raw.dependencies,
            package: raw.package,
            frameworks: raw.frameworks,
            plugins: raw.plugins,
            raw_tasks: raw.tasks,
        };
        // Validate the shared entries once, so a typo is reported even before a member uses it.
        for (name, value) in &workspace.dependencies {
            if value.get("workspace").is_some() {
                bail!("invalid dependency `{name}` in [workspace.dependencies]: `workspace` is only valid in a member's dependencies");
            }
            parse_dependency(name, value, "workspace.dependencies", None)?;
        }
        Ok(workspace)
    }
}

impl RawPublish {
    fn validate(self, repositories: &[Repository]) -> Result<PublishSettings> {
        if let Some(name) = &self.repository
            && !is_publish_destination(name, repositories)
        {
            let mut names: Vec<&str> = vec![MAVEN_CENTRAL_DESTINATION, kargo_maven::repo::MAVEN_LOCAL];
            names.extend(repositories.iter().map(|r| r.name.as_str()).filter(|n| *n != MAVEN_CENTRAL_DESTINATION));
            bail!(
                "[publish] repository `{name}` is not a repository (expected one of: {}); declare it in [repositories]",
                names.join(", ")
            );
        }
        let javadoc = match self.javadoc {
            None | Some(Value::Boolean(true)) => Javadoc::Dokka,
            Some(Value::Boolean(false)) => Javadoc::Off,
            Some(Value::String(s)) if s == "dokka" => Javadoc::Dokka,
            Some(Value::String(s)) if s == "empty" => Javadoc::Empty,
            Some(other) => bail!("[publish] `javadoc` must be true, false, \"dokka\" or \"empty\", got {other}"),
        };
        let central_publishing = match self.central_publishing.as_deref() {
            None | Some("manual") => CentralPublishing::Manual,
            Some("automatic") => CentralPublishing::Automatic,
            Some(other) => bail!("[publish] `central-publishing` must be \"manual\" or \"automatic\", got `{other}`"),
        };
        Ok(PublishSettings {
            repository: self.repository,
            sign: self.sign,
            sources: self.sources.unwrap_or(true),
            javadoc,
            central_publishing,
        })
    }
}

impl RawJlink {
    fn validate(self) -> Result<JlinkSettings> {
        for module in &self.modules {
            let valid = !module.is_empty()
                && module.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_'));
            if !valid {
                bail!("[package.jlink] `modules` entries must be JDK module names such as \"java.naming\", got `{module}`");
            }
        }
        let jdeps = self.jdeps.unwrap_or(true);
        if !jdeps && self.modules.is_empty() {
            bail!("[package.jlink] `jdeps = false` needs `modules`: the JDK modules to put in the runtime image, e.g. [\"java.base\"]");
        }
        Ok(JlinkSettings { modules: self.modules, jdeps })
    }
}

impl RawNativeImage {
    fn validate(self) -> NativeImageSettings {
        NativeImageSettings {
            args: self.args,
            static_link: self.static_link.unwrap_or(false),
            initialize_at_build_time: self.initialize_at_build_time,
            metadata_repository: self.metadata_repository.unwrap_or(true),
        }
    }
}

impl RawInstaller {
    fn validate(self) -> Result<InstallerSettings> {
        let kinds: Vec<InstallerType> = match self.kind {
            None => Vec::new(),
            Some(Value::String(kind)) => vec![kind.parse().context("invalid [package.installer] `type`")?],
            Some(Value::Array(kinds)) if !kinds.is_empty() => kinds
                .iter()
                .map(|k| match k {
                    Value::String(kind) => kind.parse().context("invalid [package.installer] `type`"),
                    _ => bail!("[package.installer] `type` entries must be strings such as \"msi\""),
                })
                .collect::<Result<_>>()?,
            Some(_) => bail!("[package.installer] `type` must be a type such as \"msi\" or a list such as [\"msi\", \"dmg\", \"deb\"]"),
        };
        let mut file_associations: Vec<FileAssociation> = Vec::new();
        for raw in self.file_associations {
            let extension = raw.extension.strip_prefix('.').unwrap_or(&raw.extension).to_owned();
            if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) {
                bail!(
                    "[package.installer] `file-associations` extensions must be letters and digits such as \"note\", got `{}`",
                    raw.extension
                );
            }
            if !raw.mime_type.contains('/') || raw.mime_type.contains(char::is_whitespace) {
                bail!("[package.installer] `file-associations` needs a MIME type such as \"application/x-note\", got `{}`", raw.mime_type);
            }
            if file_associations.iter().any(|a| a.extension == extension) {
                bail!("[package.installer] `file-associations` lists the extension `{extension}` more than once");
            }
            file_associations.push(FileAssociation { extension, mime_type: raw.mime_type, description: raw.description, icon: raw.icon });
        }
        let notarize = self.notarize.map(RawNotarize::validate).transpose()?;
        if notarize.is_some() && self.signing_identity.is_none() {
            bail!("[package.installer] `notarize` needs `signing-identity`: Apple notarizes only signed applications");
        }
        let icon = match self.icon {
            None => InstallerIcons::default(),
            Some(Value::String(path)) => {
                let path = PathBuf::from(path);
                InstallerIcons { windows: Some(path.clone()), macos: Some(path.clone()), linux: Some(path) }
            }
            Some(table @ Value::Table(_)) => {
                let icons: RawInstallerIcons = table.try_into().context("invalid [package.installer] `icon`")?;
                InstallerIcons { windows: icons.windows, macos: icons.macos, linux: icons.linux }
            }
            Some(_) => bail!(
                "[package.installer] `icon` must be a path or a table such as {{ windows = \"app.ico\", macos = \"app.icns\", linux = \"app.png\" }}"
            ),
        };
        if let Some(version) = &self.app_version
            && !is_numeric_version(version)
        {
            bail!("[package.installer] `app-version` must be one to three numbers separated by dots, such as \"1.2.3\", got `{version}`");
        }
        if let Some(uuid) = &self.upgrade_uuid
            && !is_uuid(uuid)
        {
            bail!("[package.installer] `upgrade-uuid` must be a UUID such as \"0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90\", got `{uuid}`");
        }
        if let Some(name) = &self.name
            && (name.is_empty() || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']))
        {
            bail!("[package.installer] `name` must be usable as a file name, got `{name}`");
        }
        Ok(InstallerSettings {
            name: self.name,
            vendor: self.vendor,
            description: self.description,
            app_version: self.app_version,
            kinds,
            icon,
            license_file: self.license_file,
            menu: self.menu.unwrap_or(false),
            shortcut: self.shortcut.unwrap_or(false),
            console: self.console.unwrap_or(false),
            upgrade_uuid: self.upgrade_uuid,
            bundle_id: self.bundle_id,
            signing_identity: self.signing_identity,
            category: self.category,
            file_associations,
            notarize,
        })
    }
}

impl RawNotarize {
    fn validate(self) -> Result<Notarization> {
        const USAGE: &str = "set either `keychain-profile`, or `apple-id-env`, `password-env` and `team-id`";
        let env_name = |key: &str, name: String| -> Result<String> {
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("[package.installer] notarize `{key}` must name an environment variable such as \"APPLE_ID\", got `{name}`");
            }
            Ok(name)
        };
        match (self.keychain_profile, self.apple_id_env, self.password_env, self.team_id) {
            (Some(profile), None, None, None) if !profile.is_empty() => Ok(Notarization::KeychainProfile(profile)),
            (None, Some(apple_id_env), Some(password_env), Some(team_id)) => {
                if team_id.is_empty() || !team_id.chars().all(|c| c.is_ascii_alphanumeric()) {
                    bail!("[package.installer] notarize `team-id` must be an Apple team ID such as \"ABCDE12345\", got `{team_id}`");
                }
                Ok(Notarization::AppleId {
                    apple_id_env: env_name("apple-id-env", apple_id_env)?,
                    password_env: env_name("password-env", password_env)?,
                    team_id,
                })
            }
            _ => bail!("[package.installer] `notarize`: {USAGE}"),
        }
    }
}

/// `1`, `1.2` or `1.2.3`: the versions `jpackage` accepts on every OS.
pub(crate) fn is_numeric_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() <= 3 && parts.iter().all(|p| !p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit()))
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` in hexadecimal digits.
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5 && groups.iter().zip([8, 4, 4, 4, 12]).all(|(g, len)| g.len() == len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

impl RawFmt {
    fn validate(self) -> Result<FmtSettings> {
        validate_tool_version("fmt", "ktlint", "ktlint", "1.8.0", self.ktlint.as_deref())?;
        for pattern in &self.exclude {
            if pattern.is_empty() || pattern.starts_with('!') || pattern.starts_with('/') || pattern.contains('\\') || pattern.contains(':')
            {
                bail!("[fmt] `exclude` entries must be globs relative to the project root such as \"src/generated/**\", got `{pattern}`");
            }
        }
        Ok(FmtSettings { ktlint_version: self.ktlint, exclude: self.exclude })
    }
}

impl RawLint {
    fn validate(self) -> Result<LintSettings> {
        validate_tool_version("lint", "detekt", "detekt", "1.23.8", self.detekt.as_deref())?;
        Ok(LintSettings { detekt_version: self.detekt, config: self.config, baseline: self.baseline })
    }
}

impl RawDoc {
    fn validate(self) -> Result<DocSettings> {
        validate_tool_version("doc", "dokka", "Dokka", "2.2.0", self.dokka.as_deref())?;
        let is_web = |url: &str| url.starts_with("https://") || url.starts_with("http://");
        if let Some(url) = &self.source_link
            && !is_web(url)
        {
            bail!("[doc] `source-link` must be an http(s):// URL, got `{url}`");
        }
        for url in &self.external_links {
            if !is_web(url) {
                bail!("[doc] `external-links` entries must be http(s):// URLs, got `{url}`");
            }
        }
        Ok(DocSettings {
            dokka_version: self.dokka,
            includes: self.includes,
            source_link: self.source_link.map(|u| u.trim_end_matches('/').to_owned()),
            external_links: self.external_links,
        })
    }
}
