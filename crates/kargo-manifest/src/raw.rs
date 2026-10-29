//! Serde model of `kargo.toml` and its validation into [`super::Manifest`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use toml::Value;

use super::*;
use kargo_maven::Repository;
use kargo_maven::repo::Content;

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
    generate: Vec<RawGenerate>,
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
    audit: RawAudit,
    #[serde(default)]
    publish: RawPublish,
    #[serde(default)]
    build: RawBuild,
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
    android_sdk: Option<Value>,
    kargo: Option<String>,
    typescript: Option<String>,
}

/// `[build]` (and `[workspace.build]`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawBuild {
    jobs: Option<Value>,
    default_targets: Option<Vec<String>>,
}

impl RawBuild {
    fn validate(self, section: &str) -> Result<BuildSettings> {
        let jobs = self.jobs.map(|v| kargo_util::jobs::from_toml(&format!("[{section}] `jobs`"), &v)).transpose()?;
        let default_targets = match self.default_targets {
            Some(names) => {
                let mut targets: Vec<TargetKind> = Vec::new();
                for name in &names {
                    let target: TargetKind = name.parse().with_context(|| format!("invalid [{section}] `default-targets`"))?;
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                if targets.is_empty() {
                    bail!("[{section}] `default-targets` is empty; leave it out to build every enabled target");
                }
                Some(targets)
            }
            None => None,
        };
        Ok(BuildSettings { jobs, default_targets })
    }
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
    code_style: Option<String>,
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
struct RawAudit {
    deny: Option<String>,
    #[serde(default)]
    ignore: Vec<RawAuditIgnore>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawAuditIgnore {
    id: String,
    reason: Option<String>,
    until: Option<Value>,
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
    optimize: RawOptimize,
    #[serde(default)]
    native_image: RawNativeImage,
    #[serde(default)]
    installer: RawInstaller,
    shade: Option<RawShade>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawShade {
    #[serde(default)]
    relocate: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    exclude: Vec<String>,
    minimize: Option<bool>,
    #[serde(default)]
    keep: Vec<String>,
    merge_services: Option<bool>,
    relocate_strings: Option<bool>,
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
struct RawOptimize {
    shrink: Option<bool>,
    optimize: Option<bool>,
    obfuscate: Option<bool>,
    rules: Option<PathBuf>,
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
    kind: Option<String>,
    #[serde(default)]
    relocations: Vec<String>,
    npm: Option<RawPublishNpm>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPublishNpm {
    registry: Option<String>,
    scope: Option<String>,
    access: Option<String>,
    tag: Option<String>,
    #[serde(default)]
    provenance: bool,
    #[serde(default)]
    keywords: Vec<String>,
    bugs: Option<String>,
    side_effects: Option<Value>,
    #[serde(default)]
    engines: BTreeMap<String, String>,
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
    js: Option<RawJs>,
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
    #[serde(default)]
    dev_npm_dependencies: BTreeMap<String, String>,
    wasm_opt: Option<Value>,
    opt_level: Option<String>,
    #[serde(default)]
    wasm_opt_flags: Vec<String>,
    debug_info: Option<bool>,
    #[serde(default)]
    compress: Vec<String>,
    content_hash: Option<bool>,
    max_size: Option<String>,
    max_gzip_size: Option<String>,
    size_report: Option<bool>,
    wasm_enable_array_range_checks: Option<bool>,
    wasm_debugger_custom_formatters: Option<bool>,
    #[serde(default)]
    web_resources: Vec<PathBuf>,
    web_init: Option<PathBuf>,
    js_sources: Option<Vec<PathBuf>>,
    port: Option<u16>,
    #[serde(default)]
    dependencies: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawJs {
    environment: Option<String>,
    kind: Option<String>,
    module_kind: Option<String>,
    module_name: Option<String>,
    npm_name: Option<String>,
    package_manager: Option<String>,
    #[serde(default)]
    npm_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dev_npm_dependencies: BTreeMap<String, String>,
    js_sources: Option<Vec<PathBuf>>,
    port: Option<u16>,
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

/// `web-resources` and `web-init` ship files to the browser apps that depend on a Wasm library: only a
/// library has dependents, and only the `js` and `browser` environments have a page. Each lands in the
/// app's output under its file name, so two of them cannot share one.
fn validate_web_resources(kind: WasmKind, environment: WasmEnvironment, resources: &[PathBuf], init: Option<&Path>) -> Result<()> {
    if resources.is_empty() && init.is_none() {
        return Ok(());
    }
    if kind != WasmKind::Library {
        bail!(
            "[target.wasm] `web-resources` and `web-init` ship files to the apps that depend on a library; \
             set kind = \"library\", or put an application's own files in `resources/`"
        );
    }
    if environment == WasmEnvironment::Wasi {
        bail!(
            "[target.wasm] `web-resources` and `web-init` are for the browser; they need environment = \"js\" or \"browser\", not \"wasi\""
        );
    }
    let mut names: BTreeMap<String, &Path> = BTreeMap::new();
    for (key, path) in resources.iter().map(|p| ("web-resources", p.as_path())).chain(init.map(|p| ("web-init", p))) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let inside = !path.as_os_str().is_empty() && !path.is_absolute() && !path.components().any(|c| c.as_os_str() == "..");
        let Some(name) = name.filter(|_| inside) else {
            bail!("[target.wasm] `{key}` lists `{}`; use a path inside the package", path.display());
        };
        if let Some(other) = names.insert(name.clone(), path)
            && other != path
        {
            bail!(
                "[target.wasm] `{}` and `{}` would both be shipped as `{name}`; each file lands next to the app module under its own name, so rename one",
                other.display(),
                path.display()
            );
        }
    }
    Ok(())
}

/// `js-sources` of `[target.<target>]`: directories inside the package, `js/` when not set. An empty
/// list turns the convention off.
fn js_sources(target: &str, dirs: Option<Vec<PathBuf>>) -> Result<Vec<PathBuf>> {
    let Some(dirs) = dirs else { return Ok(vec![PathBuf::from(crate::DEFAULT_JS_SOURCES)]) };
    for dir in &dirs {
        if dir.as_os_str().is_empty() || dir.is_absolute() || dir.components().any(|c| c.as_os_str() == "..") {
            bail!("[target.{target}] `js-sources` lists `{}`; use a directory inside the package", dir.display());
        }
    }
    Ok(dirs)
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
    /// `namespace`, else `<group>.<name>` of [package]: the package of the `R` and `BuildConfig` classes.
    #[serde(skip)]
    pub namespace: String,
    #[serde(rename = "namespace")]
    declared_namespace: Option<String>,
    pub application_id: Option<String>,
    #[serde(default = "default_min_sdk")]
    pub min_sdk: u32,
    /// `target-sdk`, else 36 (or `compile_sdk`, when lower).
    #[serde(skip)]
    pub target_sdk: u32,
    /// `compile-sdk`, else `[toolchain] android-sdk`, else 37.
    #[serde(skip)]
    pub compile_sdk: u32,
    #[serde(rename = "target-sdk")]
    declared_target_sdk: Option<u32>,
    #[serde(rename = "compile-sdk")]
    declared_compile_sdk: Option<u32>,
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
    /// The build-tools version (default: the one the Android Gradle Plugin uses).
    pub build_tools: Option<String>,
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
    /// Values for the `${placeholders}` of `AndroidManifest.xml`, in every variant.
    #[serde(default)]
    pub manifest_placeholders: BTreeMap<String, BuildConfigValue>,
    /// Keep only the resources of these languages (`en`, `es`, `pt-rBR`, or BCP 47 tags such as `b+sr+Latn`).
    #[serde(default)]
    pub locales: Vec<String>,
    /// Native code: the NDK, the ABIs and a CMake or ndk-build project.
    pub ndk: Option<AndroidNdk>,
    /// Android lint.
    #[serde(default)]
    pub lint: AndroidLint,
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
    /// More manifest placeholders for this flavor.
    #[serde(default)]
    pub manifest_placeholders: BTreeMap<String, BuildConfigValue>,
}

/// `[target.android.ndk]`: native code built with the NDK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidNdk {
    /// The NDK version, such as `27.2.12479018`. Kargo installs it with a managed SDK.
    pub version: Option<String>,
    /// The ABIs to package: `armeabi-v7a`, `arm64-v8a`, `x86` and `x86_64` (default: all of them).
    #[serde(default)]
    pub abi_filters: Vec<String>,
    /// The `CMakeLists.txt` that builds the native code, relative to the project root.
    pub cmake: Option<PathBuf>,
    /// The CMake version (`3.22.1`). Kargo installs it with a managed SDK.
    pub cmake_version: Option<String>,
    /// Arguments passed to CMake (`-DANDROID_STL=c++_shared`).
    #[serde(default)]
    pub cmake_arguments: Vec<String>,
    /// Flags passed to the C++ compiler.
    #[serde(default)]
    pub cpp_flags: Vec<String>,
    /// The `Android.mk` that builds the native code with ndk-build, relative to the project root.
    pub ndk_build: Option<PathBuf>,
}

impl AndroidNdk {
    /// The ABIs of Android devices.
    pub const ABIS: [&str; 4] = ["armeabi-v7a", "arm64-v8a", "x86", "x86_64"];
}

/// `[target.android.lint]`: how Android lint (`kargo lint --android`, `kargo android lint`) runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AndroidLint {
    /// The baseline of known issues, relative to the project root (default: `lint-baseline.xml`, when it exists).
    pub baseline: Option<PathBuf>,
    /// A `lint.xml` with per-issue configuration (default: `lint.xml` in the project root, when it exists).
    pub config: Option<PathBuf>,
    /// Fail the build on lint errors (default: true).
    pub abort_on_error: Option<bool>,
    /// Treat warnings as errors.
    #[serde(default)]
    pub warnings_as_errors: bool,
    /// Also check the Android libraries the package depends on.
    #[serde(default)]
    pub check_dependencies: bool,
    /// Issue ids that are not checked.
    #[serde(default)]
    pub disable: Vec<String>,
    /// Issue ids that are checked although they are off by default.
    #[serde(default)]
    pub enable: Vec<String>,
    /// Issue ids reported as fatal.
    #[serde(default)]
    pub fatal: Vec<String>,
    /// Issue ids reported as errors.
    #[serde(default)]
    pub error: Vec<String>,
    /// Issue ids reported as warnings.
    #[serde(default)]
    pub warning: Vec<String>,
    /// Issue ids reported as informational.
    #[serde(default)]
    pub informational: Vec<String>,
    /// Issue ids that are ignored.
    #[serde(default)]
    pub ignore: Vec<String>,
    /// The reports to write: `html`, `xml`, `sarif` and `text` (default: `html`, `xml` and `sarif`).
    pub reports: Option<Vec<String>>,
}

impl AndroidLint {
    /// The report formats lint writes.
    pub const REPORTS: [&str; 4] = ["html", "xml", "sarif", "text"];

    /// The reports lint writes: `reports`, else HTML, XML and SARIF. SARIF is always among them, since
    /// `--message-format json` reads it.
    pub fn reports(&self) -> Vec<&str> {
        let mut reports: Vec<&str> = match &self.reports {
            Some(list) => list.iter().map(String::as_str).collect(),
            None => vec!["html", "xml", "sarif"],
        };
        if !reports.contains(&"sarif") {
            reports.push("sarif");
        }
        reports
    }
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
    /// The value as the text of a manifest placeholder.
    pub fn text(&self) -> String {
        match self {
            BuildConfigValue::Bool(b) => b.to_string(),
            BuildConfigValue::Int(i) => i.to_string(),
            BuildConfigValue::String(s) => s.clone(),
        }
    }

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

/// Fills in `compile-sdk` and `target-sdk`: `compile-sdk` defaults to the platform of `[toolchain] android-sdk`, and
/// must match it when both are set.
fn resolve_sdk_levels(config: &mut AndroidConfig, sdk: AndroidSdkRequest) -> Result<()> {
    config.compile_sdk = match (config.declared_compile_sdk, sdk.api_level()) {
        (Some(compile), Some(api)) if compile != api => bail!(
            "[target.android] `compile-sdk = {compile}` differs from `android-sdk = \"{api}\"` under [toolchain]; \
             remove `compile-sdk`, which defaults to the [toolchain] version, or set both to the same API level"
        ),
        (Some(level), _) | (None, Some(level)) => level,
        (None, None) => default_compile_sdk(),
    };
    config.target_sdk = config.declared_target_sdk.unwrap_or(default_sdk().min(config.compile_sdk));
    Ok(())
}

/// Fills in `namespace`: the declared one, else `<group>.<name>` of [package] made into a Java package name
/// (`com.example.my_lib` for `my-lib`).
fn resolve_namespace(config: &mut AndroidConfig, group: Option<&str>, name: &str) -> Result<()> {
    config.namespace = match (&config.declared_namespace, group) {
        (Some(namespace), _) => namespace.clone(),
        (None, Some(group)) => default_namespace(group, name),
        (None, None) => bail!(
            "[target.android] needs a `namespace` (the package of the `R` class, e.g. \"com.example.{}\"), or a `group` in              [package] to derive it from",
            default_namespace("", name)
        ),
    };
    Ok(())
}

/// `<group>.<name>` as a Java package name: lowercase, with `_` for other characters, and a `_` before a segment that
/// starts with a digit or is a Java keyword.
fn default_namespace(group: &str, name: &str) -> String {
    group
        .split('.')
        .chain([name])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut clean: String =
                segment.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
            if clean.starts_with(|c: char| c.is_ascii_digit()) || JAVA_KEYWORDS.contains(&clean.as_str()) {
                clean.insert(0, '_');
            }
            clean
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The Java keywords and literals, which cannot be segments of a package name.
const JAVA_KEYWORDS: [&str; 53] = [
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "void",
    "volatile",
    "while",
];

fn validate_android(config: &AndroidConfig) -> Result<()> {
    if config.namespace.trim().is_empty() {
        bail!("[target.android] `namespace` must not be empty");
    }
    if config.namespace.split('.').any(|segment| !is_java_identifier(segment) || segment.contains('$')) {
        bail!("[target.android] `namespace = \"{}\"` is not a Java package name such as `com.example.app`", config.namespace);
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
    validate_placeholders(&config.manifest_placeholders, "target.android")?;
    validate_android_resources(config)?;
    validate_android_ndk(config)?;
    validate_android_lint(&config.lint)?;
    if config.build_tools.as_deref().is_some_and(|v| !is_dotted_version(v)) {
        bail!("[target.android] `build-tools` must be a version such as \"36.0.0\"");
    }
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
        validate_placeholders(&flavor.manifest_placeholders, &format!("target.android.flavors.{name}"))?;
    }
    Ok(())
}

/// Whether `v` is dotted numbers (`36.0.0`, `27.2.12479018`), optionally with a suffix such as `-rc1`.
fn is_dotted_version(v: &str) -> bool {
    let core = v.split('-').next().unwrap_or(v);
    !core.is_empty() && core.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn validate_placeholders(placeholders: &BTreeMap<String, BuildConfigValue>, section: &str) -> Result<()> {
    for name in placeholders.keys() {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
            bail!("[{section}.manifest-placeholders] `{name}` is not a placeholder name: use letters, digits, `_`, `.` and `-`");
        }
    }
    Ok(())
}

fn validate_android_resources(config: &AndroidConfig) -> Result<()> {
    for locale in &config.locales {
        let ok = !locale.is_empty() && locale.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '_'));
        if !ok {
            bail!("[target.android] `locales` entry `{locale}` is not a language such as `en`, `pt-rBR` or `b+sr+Latn`");
        }
    }
    Ok(())
}

fn validate_android_ndk(config: &AndroidConfig) -> Result<()> {
    let Some(ndk) = &config.ndk else { return Ok(()) };
    for abi in &ndk.abi_filters {
        if !AndroidNdk::ABIS.contains(&abi.as_str()) {
            bail!("[target.android.ndk] `abi-filters` entry `{abi}` is not an Android ABI; use {}", AndroidNdk::ABIS.join(", "));
        }
    }
    if ndk.cmake.is_some() && ndk.ndk_build.is_some() {
        bail!("[target.android.ndk] set `cmake` or `ndk-build`, not both");
    }
    if ndk.cmake.is_none() && (ndk.cmake_version.is_some() || !ndk.cmake_arguments.is_empty() || !ndk.cpp_flags.is_empty()) {
        bail!("[target.android.ndk] `cmake-version`, `cmake-arguments` and `cpp-flags` need `cmake = \"CMakeLists.txt\"`");
    }
    for (key, version) in [("version", &ndk.version), ("cmake-version", &ndk.cmake_version)] {
        if version.as_deref().is_some_and(|v| !is_dotted_version(v)) {
            bail!("[target.android.ndk] `{key}` must be a version such as \"27.2.12479018\"");
        }
    }
    Ok(())
}

fn validate_android_lint(lint: &AndroidLint) -> Result<()> {
    for (key, ids) in [
        ("disable", &lint.disable),
        ("enable", &lint.enable),
        ("fatal", &lint.fatal),
        ("error", &lint.error),
        ("warning", &lint.warning),
        ("informational", &lint.informational),
        ("ignore", &lint.ignore),
    ] {
        if let Some(id) = ids.iter().find(|id| id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            bail!("[target.android.lint] `{key}` entry `{id}` is not a lint issue id such as `MissingTranslation`");
        }
    }
    if let Some(report) = lint.reports.iter().flatten().find(|r| !AndroidLint::REPORTS.contains(&r.as_str())) {
        bail!("[target.android.lint] `reports` entry `{report}` is not a report format; use {}", AndroidLint::REPORTS.join(", "));
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
#[derive(Debug, Clone)]
pub enum Toggle<T> {
    Enabled(bool),
    Config(T),
}

/// Not `#[serde(untagged)]`, which would answer a bad option with "data did not match any variant":
/// a table is read as `T` so its own error (an unknown key, a wrong type) reaches the user.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Toggle<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Boolean(enabled) => Ok(Toggle::Enabled(enabled)),
            table @ Value::Table(_) => T::deserialize(table).map(Toggle::Config).map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!("expected true, false or a table of options, found {}", other.type_str()))),
        }
    }
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
    /// Stability configuration files of the Compose compiler, relative to the package.
    #[serde(default, deserialize_with = "one_or_many")]
    pub stability_config: Vec<String>,
    /// Strong skipping mode (default true, the Compose compiler's own default).
    pub strong_skipping: Option<bool>,
    /// Compose compiler feature flags such as `OptimizeNonSkippingGroups`; a leading `-` disables one.
    #[serde(default)]
    pub feature_flags: Vec<String>,
    /// Write the Compose compiler's metrics to `target/compose/<target>/metrics`.
    pub metrics: Option<bool>,
    /// Write the Compose compiler's stability reports to `target/compose/<target>/reports`.
    pub reports: Option<bool>,
    /// Compose HTML on the Kotlin/JS target: composables render DOM elements (`org.jetbrains.compose.html`)
    /// instead of drawing on a canvas. The other targets keep their own Compose.
    pub html: Option<bool>,
    /// Add `html-svg`, the SVG elements of Compose HTML (needs `html`).
    pub svg: Option<bool>,
}

/// The feature flags of the Compose compiler that `[frameworks.compose] feature-flags` accepts.
pub const COMPOSE_FEATURE_FLAGS: &[&str] = &["StrongSkipping", "IntrinsicRemember", "OptimizeNonSkippingGroups", "PausableComposition"];

/// A Ktor engine name. A boolean is the likely mistake (`client = true`), so the error names what is accepted.
fn engine_name<'de, D: serde::Deserializer<'de>>(deserializer: D, key: &str, engines: &str) -> Result<Option<String>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(name) => Ok(Some(name)),
        other => Err(serde::de::Error::custom(format!(
            "`{key}` is the name of an engine, a string such as {engines}, not {}; remove the key if you do not need a {key}",
            other.type_str()
        ))),
    }
}

fn server_engine<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    engine_name(deserializer, "server", "\"netty\", \"cio\", \"jetty\" or \"tomcat\"")
}

fn client_engine<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    engine_name(deserializer, "client", "\"cio\", \"okhttp\", \"java\", \"apache5\", \"js\", \"curl\", \"winhttp\" or \"darwin\"")
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    OneOrMany::deserialize(deserializer).map(OneOrMany::into_vec)
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
    #[serde(default, deserialize_with = "server_engine")]
    pub server: Option<String>,
    /// Client engine (`cio`, `okhttp`, `java`, `apache5`, `js`, `curl`, `winhttp`, `darwin`).
    #[serde(default, deserialize_with = "client_engine")]
    pub client: Option<String>,
    /// The client engine of single targets, instead of `client`: an engine name, or `none` for the
    /// client core alone (the engine of Kotlin/Wasm and Kotlin/JS comes with it).
    #[serde(default)]
    pub client_engines: BTreeMap<String, String>,
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
    url: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
    #[serde(default)]
    include_groups: Vec<String>,
    #[serde(default)]
    exclude_groups: Vec<String>,
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
fn parse_run(what: &str, key: &str, value: Option<Value>) -> Result<Option<Vec<String>>> {
    Ok(match value {
        None => None,
        Some(Value::String(s)) => Some(vec![s]),
        Some(Value::Array(items)) => Some(
            items
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s),
                    _ => bail!("{what}: `{key}` entries must be strings"),
                })
                .collect::<Result<_>>()?,
        ),
        Some(_) => bail!("{what}: `{key}` must be a string or an array of strings"),
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

fn parse_size(key: &str, value: &str) -> Result<u64> {
    let text = value.trim();
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (amount, unit) = text.split_at(digits);
    let multiplier = match unit.trim() {
        "B" => 1,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        "GiB" => 1024 * 1024 * 1024,
        _ => 0,
    };
    match amount.parse::<u64>() {
        Ok(n) if n > 0 && multiplier > 0 => n.checked_mul(multiplier).context("size value overflow"),
        _ => bail!("{key} must be a positive size such as \"1 MiB\" or \"512 KiB\", not \"{value}\""),
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
        let what = format!("task `{name}`");
        let run = parse_run(&what, "run", raw.run)?.unwrap_or_default();
        let windows_run = parse_run(&what, "windows-run", raw.windows_run)?;
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
        return Err(kargo_util::help::unknown(
            format!("unknown Kotlin/Native platform `{name}` (expected one of: {})", KONAN_TARGETS.join(", ")),
            &snake,
            KONAN_TARGETS,
        ));
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
        // Read before the targets: `compile-sdk` defaults to it.
        let android_sdk = match &self.toolchain.android_sdk {
            None => AndroidSdkRequest::System,
            Some(Value::String(s)) => s.parse().context("invalid [toolchain] `android-sdk`")?,
            Some(Value::Integer(i)) => i.to_string().parse().context("invalid [toolchain] `android-sdk`")?,
            Some(_) => bail!("[toolchain] `android-sdk` must be a platform API level like \"36\" or \"system\""),
        };

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
                                validate_module_name("wasm", name)?;
                            }
                            if let Some(name) = &w.npm_name {
                                if kind != WasmKind::Library {
                                    bail!("[target.wasm] `npm-name` names the package of a library; set kind = \"library\" or remove it");
                                }
                                validate_npm_name("wasm", name)?;
                            }
                            for (table, deps) in [("npm-dependencies", &w.npm_dependencies), ("dev-npm-dependencies", &w.dev_npm_dependencies)] {
                                for (name, version) in deps {
                                    if name.trim().is_empty() || version.trim().is_empty() {
                                        bail!("[target.wasm.{table}] needs a package name and a version for every entry");
                                    }
                                }
                            }
                            let wasm_opt = match w.wasm_opt {
                                None | Some(Value::Boolean(true)) => WasmOpt::Auto,
                                Some(Value::Boolean(false)) => WasmOpt::Off,
                                Some(Value::String(path)) if !path.trim().is_empty() => WasmOpt::Path(PathBuf::from(path)),
                                Some(_) => bail!("[target.wasm] `wasm-opt` must be true, false or the path of a wasm-opt executable"),
                            };
                            let opt_level = match w.opt_level.as_deref().unwrap_or("balanced") {
                                "size" => WasmOptLevel::Size,
                                "speed" => WasmOptLevel::Speed,
                                "balanced" => WasmOptLevel::Balanced,
                                other => bail!("unknown [target.wasm] opt-level `{other}` (expected `size`, `balanced` or `speed`)"),
                            };
                            let compress = w.compress.iter().map(|v| match v.as_str() {
                                "gzip" => Ok(WasmCompression::Gzip),
                                "brotli" => Ok(WasmCompression::Brotli),
                                other => bail!("unknown [target.wasm] compression `{other}` (expected `gzip` or `brotli`)"),
                            }).collect::<Result<Vec<_>>>()?;
                            let max_size = w.max_size.as_deref().map(|s| parse_size("[target.wasm] `max-size`", s)).transpose()?;
                            let max_gzip_size = w.max_gzip_size.as_deref().map(|s| parse_size("[target.wasm] `max-gzip-size`", s)).transpose()?;
                            let package_manager = w.package_manager.as_deref().map(str::parse).transpose()?;
                            validate_web_resources(kind, environment, &w.web_resources, w.web_init.as_deref())?;
                            let js_sources = match (environment, w.js_sources) {
                                // WASI has no JavaScript host.
                                (WasmEnvironment::Wasi, Some(dirs)) if !dirs.is_empty() => bail!(
                                    "[target.wasm] `js-sources` are JavaScript modules, which need environment = \"js\" or \"browser\", not \"wasi\""
                                ),
                                (WasmEnvironment::Wasi, _) => Vec::new(),
                                (_, dirs) => js_sources("wasm", dirs)?,
                            };
                            if w.port.is_some() && environment != WasmEnvironment::Browser {
                                bail!("[target.wasm] `port` is the port `kargo run` serves a browser app on; it needs environment = \"browser\"");
                            }
                            Ok(WasmTarget {
                                environment,
                                kind,
                                module_name: w.module_name,
                                npm_name: w.npm_name,
                                package_manager,
                                npm_dependencies: w.npm_dependencies,
                                dev_npm_dependencies: w.dev_npm_dependencies,
                                wasm_opt,
                                opt_level,
                                wasm_opt_flags: w.wasm_opt_flags,
                                debug_info: w.debug_info.unwrap_or(false),
                                compress,
                                content_hash: w.content_hash.unwrap_or(false),
                                max_size,
                                max_gzip_size,
                                size_report: w.size_report.unwrap_or(false),
                                wasm_enable_array_range_checks: w.wasm_enable_array_range_checks,
                                wasm_debugger_custom_formatters: w.wasm_debugger_custom_formatters.unwrap_or(false),
                                web_resources: w.web_resources,
                                web_init: w.web_init,
                                js_sources,
                                port: w.port,
                                dependencies: deps.parse(&w.dependencies, "target.wasm.dependencies", false, Some(TargetKind::Wasm))?,
                            })
                        })
                        .transpose()?,
                    js: raw
                        .js
                        .map(|j| -> Result<JsTarget> {
                            let environment = match j.environment.as_deref() {
                                None | Some("node") | Some("nodejs") => JsEnvironment::Node,
                                Some("browser") => JsEnvironment::Browser,
                                Some(other) => bail!("unknown js environment `{other}` (expected `node` or `browser`)"),
                            };
                            let kind = match j.kind.as_deref() {
                                None | Some("application") | Some("app") => JsKind::Application,
                                Some("library") | Some("lib") => JsKind::Library,
                                Some(other) => bail!("unknown js kind `{other}` (expected `application` or `library`)"),
                            };
                            let module_kind = match j.module_kind.as_deref() {
                                None | Some("es") | Some("esm") => JsModuleKind::Es,
                                Some("commonjs") | Some("cjs") => JsModuleKind::CommonJs,
                                Some("umd") => JsModuleKind::Umd,
                                Some(other) => bail!("unknown js module-kind `{other}` (expected `es`, `commonjs` or `umd`)"),
                            };
                            if environment == JsEnvironment::Browser && kind == JsKind::Application && module_kind == JsModuleKind::CommonJs
                            {
                                bail!(
                                    "[target.js] a browser cannot load a CommonJS module; set module-kind = \"es\" or \"umd\" for environment = \"browser\""
                                );
                            }
                            if let Some(name) = &j.module_name {
                                validate_module_name("js", name)?;
                            }
                            if let Some(name) = &j.npm_name {
                                if kind != JsKind::Library {
                                    bail!("[target.js] `npm-name` names the package of a library; set kind = \"library\" or remove it");
                                }
                                validate_npm_name("js", name)?;
                            }
                            for (table, deps) in [("npm-dependencies", &j.npm_dependencies), ("dev-npm-dependencies", &j.dev_npm_dependencies)] {
                                for (name, version) in deps {
                                    if name.trim().is_empty() || version.trim().is_empty() {
                                        bail!("[target.js.{table}] needs a package name and a version for every entry");
                                    }
                                }
                            }
                            if j.port.is_some() && environment != JsEnvironment::Browser {
                                bail!("[target.js] `port` is the port `kargo run` serves a browser app on; it needs environment = \"browser\"");
                            }
                            Ok(JsTarget {
                                environment,
                                kind,
                                module_kind,
                                module_name: j.module_name,
                                npm_name: j.npm_name,
                                package_manager: j.package_manager.as_deref().map(str::parse).transpose()?,
                                npm_dependencies: j.npm_dependencies,
                                dev_npm_dependencies: j.dev_npm_dependencies,
                                js_sources: js_sources("js", j.js_sources)?,
                                port: j.port,
                                dependencies: deps.parse(&j.dependencies, "target.js.dependencies", false, Some(TargetKind::Js))?,
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
                            let mut config: AndroidConfig =
                                Value::Table(table).try_into().context("invalid [target.android] section")?;
                            resolve_sdk_levels(&mut config, android_sdk)?;
                            resolve_namespace(&mut config, self.package.group.as_deref(), &self.package.name)?;
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

        // `defaults = false` leaves out Maven Central and Google, for builds that go through a mirror.
        let default_repositories = match self.repositories.get("defaults") {
            None => true,
            Some(Value::Boolean(defaults)) => *defaults,
            Some(_) => bail!("[repositories] `defaults` must be true or false"),
        };
        let mut repositories = Vec::new();
        if default_repositories {
            repositories.push(Repository::maven_central());
            if targets.android.is_some() {
                repositories.push(Repository::google());
            }
        }
        for (name, value) in self.repositories.iter().filter(|(name, _)| *name != "defaults") {
            let raw: RawRepository = match value {
                Value::String(url) => RawRepository {
                    url: Some(url.clone()),
                    username_env: None,
                    password_env: None,
                    include_groups: Vec::new(),
                    exclude_groups: Vec::new(),
                },
                Value::Table(_) => value.clone().try_into().with_context(|| format!("invalid repository `{name}`"))?,
                _ => bail!("invalid repository `{name}`: expected a URL or a table"),
            };
            let mut repo = match raw.url.as_deref().map(str::trim) {
                // The built-in repositories are configured by name.
                None if name == "maven-central" => Repository::maven_central(),
                None if name == "google" => Repository::google(),
                None => bail!("repository `{name}` needs a `url`"),
                Some(kargo_maven::repo::MAVEN_LOCAL) => Repository { name: name.clone(), ..Repository::maven_local()? },
                Some(url) if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("file:") => {
                    Repository::new(name.clone(), url)
                }
                Some(url) => bail!("repository `{name}` must have an http(s):// or file: URL, or be \"maven-local\", got `{url}`"),
            };
            repo.credentials = credentials_from_env(name, raw.username_env.as_deref(), raw.password_env.as_deref())?;
            // GitHub Packages, GitLab and CodeArtifact take the variables their CI sets.
            if raw.username_env.is_none() && raw.password_env.is_none() {
                repo.credentials = kargo_maven::repo::host_preset(&repo.url).and_then(|preset| preset.credentials());
            }
            for pattern in raw.include_groups.iter().chain(&raw.exclude_groups) {
                kargo_maven::repo::validate_group_pattern(pattern).with_context(|| format!("invalid repository `{name}`"))?;
            }
            repo.content = Content { include_groups: raw.include_groups, exclude_groups: raw.exclude_groups };
            // A repository declared again (such as `maven-central`, to exclude groups from it) keeps its place.
            match repositories.iter_mut().find(|r| r.url == repo.url) {
                Some(existing) => {
                    if repo.credentials.is_some() {
                        existing.credentials = repo.credentials;
                    }
                    if !repo.content.is_unrestricted() {
                        existing.content = repo.content;
                    }
                }
                None => repositories.push(repo),
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
        if let Some(compose) = &frameworks.compose {
            for flag in &compose.feature_flags {
                let name = flag.strip_prefix('-').unwrap_or(flag);
                if !COMPOSE_FEATURE_FLAGS.contains(&name) {
                    bail!(
                        "[frameworks.compose] `feature-flags` has unknown flag `{flag}` (expected {}, optionally prefixed with `-` to disable it)",
                        COMPOSE_FEATURE_FLAGS.join(", ")
                    );
                }
                if name == "StrongSkipping" && compose.strong_skipping.is_some() {
                    bail!(
                        "[frameworks.compose] sets both `strong-skipping` and the `StrongSkipping` feature flag; keep only `strong-skipping`"
                    );
                }
            }
            if compose.svg == Some(true) && compose.html != Some(true) {
                bail!("[frameworks.compose] `svg` adds the SVG elements of Compose HTML; set `html = true` too");
            }
            if let Some(empty) = compose.stability_config.iter().find(|p| p.trim().is_empty()) {
                bail!(
                    "[frameworks.compose] `stability-config` has an empty path `{empty}`; list the stability configuration files relative to the package"
                );
            }
        }
        // Compose Multiplatform depends on androidx artifacts hosted by Google.
        if default_repositories && frameworks.compose.is_some() && !repositories.iter().any(|r| r.url == kargo_maven::repo::GOOGLE) {
            repositories.insert(1, Repository::google());
        }
        if let Some(ktor) = &frameworks.ktor
            && ktor.server.is_none()
            && ktor.client.is_none()
        {
            bail!("[frameworks.ktor] enable at least one of `server = \"netty\"` or `client = \"cio\"`");
        }
        if let Some(ktor) = &frameworks.ktor {
            if !ktor.client_engines.is_empty() && ktor.client.is_none() {
                bail!(
                    "[frameworks.ktor] `client-engines` changes the engine of single targets; set `client = \"cio\"` (or another engine) for the others"
                );
            }
            for (target, engine) in &ktor.client_engines {
                if !TargetKind::ALL.iter().any(|t| t.as_str() == target) {
                    let all: Vec<&str> = TargetKind::ALL.iter().map(|t| t.as_str()).collect();
                    bail!("[frameworks.ktor] `client-engines` has unknown target `{target}` (expected one of: {})", all.join(", "));
                }
                if engine.trim().is_empty() {
                    bail!(
                        "[frameworks.ktor] `client-engines.{target}` is empty; name an engine such as \"okhttp\", or \"none\" for no engine"
                    );
                }
            }
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
        let typescript = match self.toolchain.typescript.as_deref().map(str::trim) {
            None => None,
            Some("") => bail!("[toolchain] `typescript` is empty; use a version like \"5.9.3\", a range like \"^6\" or \"latest\""),
            Some(spec) if spec.contains(char::is_whitespace) => {
                bail!(
                    "invalid [toolchain] `typescript` `{spec}`: use a version like \"5.9.3\", a range like \"^6\" or a tag like \"latest\""
                )
            }
            Some(spec) => Some(spec.to_owned()),
        };
        let packaging = PackagingSettings {
            jlink: self.package.jlink.validate()?,
            optimize: self.package.optimize.validate()?,
            native_image: self.package.native_image.validate(),
            installer: self.package.installer.validate()?,
            shade: self.package.shade.map(RawShade::validate).transpose()?,
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
        let generators = validate_generators(self.generate)?;
        let doc = self.doc.validate()?;
        let fmt = self.fmt.validate()?;
        let lint = self.lint.validate()?;
        let build = self.build.validate("build")?;
        if let Some(missing) = build.default_targets.iter().flatten().find(|t| !targets.is_enabled(**t)) {
            bail!("[build] `default-targets` names `{missing}`, which is not enabled; add a [target.{missing}] section");
        }

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
            toolchain: ToolchainSettings { jdk, graalvm, android_sdk, kargo, typescript },
            packaging,
            doc,
            fmt,
            build,
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
            audit: validate_audit(self.audit)?,
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
            generators,
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
fn validate_audit(raw: RawAudit) -> Result<AuditSettings> {
    let deny = raw.deny.as_deref().map(str::parse).transpose().context("invalid [audit] `deny`")?;
    let mut ignore = Vec::new();
    for entry in raw.ignore {
        let id = entry.id.trim().to_owned();
        if id.is_empty() {
            bail!("an [audit] `ignore` entry has an empty `id`; name the advisory, such as \"GHSA-xxxx-xxxx-xxxx\" or \"CVE-2024-1234\"");
        }
        let reason = entry
            .reason
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .with_context(|| format!("[audit] `ignore` entry `{id}` needs a `reason` that says why the advisory does not apply"))?;
        let until = match entry.until {
            None => None,
            Some(Value::Datetime(d)) if d.date.is_some() && d.time.is_none() => Some(d.to_string()),
            Some(Value::String(s)) if is_date(&s) => Some(s),
            Some(other) => bail!("[audit] `ignore` entry `{id}` has `until = {other}`; use a date such as 2026-12-31"),
        };
        ignore.push(AuditIgnore { id, reason, until });
    }
    Ok(AuditSettings { deny, ignore })
}

/// Whether `s` is a date such as `2026-12-31`.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && matches!(s[5..7].parse::<u32>(), Ok(1..=12))
        && matches!(s[8..10].parse::<u32>(), Ok(1..=31))
}

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

/// One `[[generate]]` step as written.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawGenerate {
    name: String,
    #[serde(default)]
    inputs: Vec<String>,
    run: Option<Value>,
    windows_run: Option<Value>,
    shell: Option<String>,
    tool: Option<String>,
    main_class: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    builtin: Option<String>,
    targets: Option<Vec<String>>,
    kind: Option<String>,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    after: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    // builtin = "build-config"
    package: Option<String>,
    object: Option<String>,
    #[serde(default)]
    fields: BTreeMap<String, BuildConfigValue>,
    #[serde(default)]
    expose: Vec<String>,
    build_time: Option<bool>,
    // builtin = "protobuf"
    version: Option<String>,
    proto_path: Option<Vec<String>>,
    java: Option<bool>,
    kotlin: Option<bool>,
    grpc: Option<bool>,
    grpc_java_version: Option<String>,
    grpc_kotlin_version: Option<String>,
}

impl RawGenerate {
    /// The keys that only some `builtin` generators read, with the ones they belong to.
    fn builtin_keys(&self) -> [(&'static str, bool, &'static str); 12] {
        let build_config = "build-config";
        let protobuf = "protobuf";
        [
            ("package", self.package.is_some(), build_config),
            ("object", self.object.is_some(), build_config),
            ("fields", !self.fields.is_empty(), build_config),
            ("expose", !self.expose.is_empty(), build_config),
            ("build-time", self.build_time.is_some(), build_config),
            ("version", self.version.is_some(), protobuf),
            ("proto-path", self.proto_path.is_some(), protobuf),
            ("java", self.java.is_some(), protobuf),
            ("kotlin", self.kotlin.is_some(), protobuf),
            ("grpc", self.grpc.is_some(), protobuf),
            ("grpc-java-version", self.grpc_java_version.is_some(), protobuf),
            ("grpc-kotlin-version", self.grpc_kotlin_version.is_some(), protobuf),
        ]
    }
}

/// Validates `[[generate]]`: unique names, exactly one of `run`, `tool` and `builtin`, known targets,
/// `after` naming other steps without cycles, and keys that fit the way the step works.
fn validate_generators(raw: Vec<RawGenerate>) -> Result<Vec<Generator>> {
    let mut generators: Vec<Generator> = Vec::new();
    for g in raw {
        let name = g.name.clone();
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid [[generate]] name `{name}` (start with a letter; use letters, digits, `-` and `_`)");
        }
        if generators.iter().any(|other| other.name == name) {
            bail!("[[generate]] `{name}` is declared more than once");
        }
        let what = format!("[[generate]] `{name}`");
        let run = parse_run(&what, "run", g.run.clone())?;
        let windows_run = parse_run(&what, "windows-run", g.windows_run.clone())?;
        let ways = [run.is_some(), g.tool.is_some(), g.builtin.is_some()].into_iter().filter(|set| *set).count();
        if ways != 1 {
            bail!(
                "{what}: set exactly one of `run` (a shell command), `tool` (a Maven artifact to run) and `builtin` ({})",
                BUILTIN_GENERATORS.join(", ")
            );
        }
        if run.as_ref().is_some_and(Vec::is_empty) {
            bail!("{what}: `run` has no command");
        }
        if run.is_none() && (windows_run.is_some() || g.shell.is_some()) {
            bail!("{what}: `{}` goes with `run`", if windows_run.is_some() { "windows-run" } else { "shell" });
        }
        if g.tool.is_none() && g.main_class.is_some() {
            bail!("{what}: `main-class` goes with `tool`");
        }
        if let Some(class) = &g.main_class
            && (class.is_empty() || !class.split('.').all(|seg| !seg.is_empty() && !seg.contains(char::is_whitespace)))
        {
            bail!("{what}: invalid `main-class` `{class}` (expected a qualified name such as `org.example.Main`)");
        }
        if !g.args.is_empty() && g.tool.is_none() && g.builtin.as_deref() != Some("protobuf") {
            bail!("{what}: `args` goes with `tool` or `builtin = \"protobuf\"`; put the arguments of a `run` command in the command");
        }
        let shell = match g.shell.as_deref() {
            None => None,
            Some(s) => Some(TaskShell::parse(s).with_context(|| {
                let known: Vec<_> = TaskShell::ALL.iter().map(|s| s.as_str()).collect();
                format!("{what}: unknown shell `{s}`; expected one of {}", known.join(", "))
            })?),
        };
        if let Some(builtin) = &g.builtin {
            if !BUILTIN_GENERATORS.contains(&builtin.as_str()) {
                bail!("{what}: unknown builtin `{builtin}` (expected one of: {})", BUILTIN_GENERATORS.join(", "));
            }
            if let Some((key, _, owner)) = g.builtin_keys().into_iter().find(|(_, set, owner)| *set && owner != builtin) {
                bail!("{what}: `{key}` goes with `builtin = \"{owner}\"`");
            }
        } else if let Some((key, _, owner)) = g.builtin_keys().into_iter().find(|(_, set, _)| *set) {
            bail!("{what}: `{key}` goes with `builtin = \"{owner}\"`");
        }
        let is_class = |s: &str| !s.is_empty() && s.split('.').all(is_java_identifier);
        let action = match (run, &g.tool, g.builtin.as_deref()) {
            (Some(run), _, _) => GeneratorAction::Run { run, windows_run, shell },
            (_, Some(tool), _) => {
                let parts: Vec<&str> = tool.split(':').collect();
                let [group, artifact, version] = parts.as_slice() else {
                    bail!("{what}: `tool` must be `group:artifact:version`, got `{tool}`");
                };
                if [group, artifact, version].iter().any(|p| p.is_empty() || p.contains(char::is_whitespace)) {
                    bail!("{what}: `tool` must be `group:artifact:version`, got `{tool}`");
                }
                GeneratorAction::Tool {
                    module: ModuleId::new(*group, *artifact),
                    version: (*version).to_owned(),
                    main_class: g.main_class.clone(),
                    args: g.args.clone(),
                }
            }
            (_, _, Some("build-config")) => {
                if !g.inputs.is_empty() {
                    bail!("{what}: `inputs` does not apply to `builtin = \"build-config\"`; it is written from kargo.toml and Git");
                }
                if let Some(package) = &g.package
                    && !is_class(package)
                {
                    bail!("{what}: invalid `package` `{package}` (expected a Kotlin package such as `com.example.app`)");
                }
                let object = g.object.clone().unwrap_or_else(|| "BuildConfig".to_owned());
                if !is_java_identifier(&object) || object.contains('$') {
                    bail!("{what}: `object` must be a Kotlin name such as `BuildConfig`, got `{object}`");
                }
                for field in g.fields.keys().chain(&g.expose) {
                    if !is_java_identifier(field) || field.contains('$') {
                        bail!("{what}: `{field}` is not a valid constant name");
                    }
                }
                const FIXED: [&str; 6] = ["VERSION", "GROUP", "NAME", "GIT_SHA", "BUILD_TIME", "PROFILE"];
                if let Some(field) = g.fields.keys().chain(&g.expose).find(|f| FIXED.contains(&f.as_str())) {
                    bail!("{what}: `{field}` is one of the constants Kargo writes; pick another name");
                }
                GeneratorAction::BuildConfig(BuildConfigGenerator {
                    package: g.package.clone(),
                    object,
                    fields: g.fields.clone(),
                    expose: g.expose.clone(),
                    build_time: g.build_time.unwrap_or(false),
                })
            }
            (_, _, Some("protobuf")) => {
                let (java, kotlin, grpc) = (g.java.unwrap_or(true), g.kotlin.unwrap_or(true), g.grpc.unwrap_or(false));
                if !java && !kotlin {
                    bail!("{what}: `java` and `kotlin` are both off, so there is nothing to generate");
                }
                if kotlin && !java {
                    bail!("{what}: the Kotlin DSL wraps the Java classes; turn `java` on or `kotlin` off");
                }
                for (key, version) in
                    [("version", &g.version), ("grpc-java-version", &g.grpc_java_version), ("grpc-kotlin-version", &g.grpc_kotlin_version)]
                {
                    if let Some(v) = version
                        && (v.is_empty() || v.contains(|c: char| c.is_whitespace() || "[](),".contains(c)))
                    {
                        bail!("{what}: `{key}` must be one exact version, got `{v}`");
                    }
                }
                if !grpc && (g.grpc_java_version.is_some() || g.grpc_kotlin_version.is_some()) {
                    bail!("{what}: the gRPC plugin versions go with `grpc = true`");
                }
                if !(grpc && kotlin) && g.grpc_kotlin_version.is_some() {
                    bail!("{what}: `grpc-kotlin-version` goes with `grpc = true` and `kotlin = true`");
                }
                GeneratorAction::Protobuf(ProtobufGenerator {
                    version: g.version.clone(),
                    proto_path: g.proto_path.clone().unwrap_or_else(|| vec!["proto".to_owned()]),
                    java,
                    kotlin,
                    grpc,
                    grpc_java_version: g.grpc_java_version.clone(),
                    grpc_kotlin_version: g.grpc_kotlin_version.clone(),
                    args: g.args.clone(),
                })
            }
            _ => unreachable!("exactly one way was checked above"),
        };

        let is_protobuf = matches!(action, GeneratorAction::Protobuf(_));
        let targets = match &g.targets {
            Some(names) => {
                let mut targets: Vec<TargetKind> = Vec::new();
                for t in names {
                    let target: TargetKind = t.parse().with_context(|| format!("{what}: invalid `targets`"))?;
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                if targets.is_empty() {
                    bail!("{what}: `targets` is empty; leave it out to generate for every target");
                }
                targets
            }
            // Protobuf's Java and Kotlin classes only exist on the JVM.
            None if is_protobuf => vec![TargetKind::Jvm, TargetKind::Android],
            None => TargetKind::ALL.to_vec(),
        };
        let kind: GeneratedKind = match &g.kind {
            Some(k) => k.parse().with_context(|| what.clone())?,
            None => GeneratedKind::default(),
        };
        if kind == GeneratedKind::Resources && !targets.iter().any(|t| matches!(t, TargetKind::Jvm | TargetKind::Android)) {
            bail!("{what}: resources only exist on the jvm and android targets; add one to `targets`");
        }
        let mut inputs = g.inputs.clone();
        if inputs.is_empty() && is_protobuf {
            inputs.push("proto/**/*.proto".to_owned());
        }
        let mut roots = Vec::new();
        for root in &g.roots {
            let slashed = root.replace(r"\", "/");
            let normalized = slashed.trim_end_matches('/');
            if slashed.starts_with('/')
                || normalized.is_empty()
                || normalized.split('/').any(|part| part.is_empty() || part == "." || part == "..")
                || normalized.contains(':')
            {
                bail!("{what}: `roots` entry `{root}` must be a directory inside the output, such as `src/main/kotlin`");
            }
            if !roots.iter().any(|r| r == normalized) {
                roots.push(normalized.to_owned());
            }
        }
        generators.push(Generator { name, inputs, action, targets, kind, roots, after: g.after, env: g.env });
    }
    for g in &generators {
        for dep in &g.after {
            if dep == &g.name {
                bail!("[[generate]] `{}` runs after itself", g.name);
            }
            if !generators.iter().any(|other| &other.name == dep) {
                bail!("[[generate]] `{}` runs after `{dep}`, which no [[generate]] declares", g.name);
            }
        }
    }
    // Follow `after` from every step to find a cycle.
    fn visit<'a>(generators: &'a [Generator], name: &'a str, path: &mut Vec<&'a str>, done: &mut Vec<&'a str>) -> Result<()> {
        if done.contains(&name) {
            return Ok(());
        }
        if let Some(pos) = path.iter().position(|n| *n == name) {
            let mut cycle: Vec<&str> = path[pos..].to_vec();
            cycle.push(name);
            bail!("[[generate]] steps wait for each other: {}", cycle.join(" -> "));
        }
        path.push(name);
        if let Some(g) = generators.iter().find(|g| g.name == name) {
            for dep in &g.after {
                visit(generators, dep, path, done)?;
            }
        }
        path.pop();
        done.push(name);
        Ok(())
    }
    let mut done = Vec::new();
    for g in &generators {
        visit(&generators, &g.name, &mut Vec::new(), &mut done)?;
    }
    Ok(generators)
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
    #[serde(default)]
    build: RawBuild,
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
        let build = raw.build.validate("workspace.build")?;

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
            build,
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
        let kind = match self.kind.as_deref() {
            None | Some("library") => PublishKind::Library,
            Some("bom") => PublishKind::Bom,
            Some(other) => bail!("[publish] `kind` must be \"library\" or \"bom\", got `{other}`"),
        };
        let mut relocations = Vec::new();
        for text in &self.relocations {
            let module: ModuleId =
                text.parse().map_err(|e| anyhow::anyhow!("[publish] `relocations`: {e}; write the old coordinates as `group:artifact`"))?;
            if relocations.contains(&module) {
                bail!("[publish] `relocations` lists `{module}` twice");
            }
            relocations.push(module);
        }
        let npm = self.npm.map(RawPublishNpm::validate).transpose()?;
        Ok(PublishSettings {
            repository: self.repository,
            sign: self.sign,
            sources: self.sources.unwrap_or(true),
            javadoc,
            central_publishing,
            kind,
            relocations,
            npm,
        })
    }
}

impl RawPublishNpm {
    fn validate(self) -> Result<NpmPublishSettings> {
        for (key, url) in [("registry", &self.registry), ("bugs", &self.bugs)] {
            if let Some(url) = url
                && !(url.starts_with("http://") || url.starts_with("https://"))
            {
                bail!("[publish.npm] `{key}` must be an http(s):// URL, got `{url}`");
            }
        }
        let scope = match self.scope {
            None => None,
            Some(scope) => {
                let bare = scope.strip_prefix('@').unwrap_or(&scope);
                let valid = !bare.is_empty()
                    && !bare.starts_with(['.', '_'])
                    && bare.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_' | '~'));
                if !valid {
                    bail!(
                        "[publish.npm] `scope` must be lowercase letters, digits, `-`, `.`, `_` and `~`, such as \"acme\", got `{scope}`"
                    );
                }
                Some(bare.to_owned())
            }
        };
        let access = match self.access.as_deref() {
            None => None,
            Some("public") => Some(NpmAccess::Public),
            Some("restricted") => Some(NpmAccess::Restricted),
            Some(other) => bail!("[publish.npm] `access` must be \"public\" or \"restricted\", got `{other}`"),
        };
        if let Some(tag) = &self.tag
            && (tag.is_empty() || tag.contains(|c: char| c.is_whitespace() || c == '/' || c == '@'))
        {
            bail!("[publish.npm] `tag` must be a dist-tag such as \"next\", got `{tag}`");
        }
        if self.keywords.iter().any(|k| k.trim().is_empty()) {
            bail!("[publish.npm] `keywords` must not contain an empty keyword");
        }
        let side_effects = match self.side_effects {
            None => None,
            Some(Value::Boolean(flag)) => Some(NpmSideEffects::Flag(flag)),
            Some(Value::Array(files)) => {
                let files = files
                    .into_iter()
                    .map(|f| match f {
                        Value::String(s) if !s.trim().is_empty() => Ok(s),
                        _ => bail!("[publish.npm] `side-effects` lists file patterns such as [\"./polyfill.mjs\"]"),
                    })
                    .collect::<Result<Vec<_>>>()?;
                Some(NpmSideEffects::Files(files))
            }
            Some(_) => bail!("[publish.npm] `side-effects` must be true, false or a list of file patterns"),
        };
        if let Some((name, _)) = self.engines.iter().find(|(name, version)| name.trim().is_empty() || version.trim().is_empty()) {
            bail!("[publish.npm] `engines` needs a name and a version range for every entry, such as `node = \">=20\"` (check `{name}`)");
        }
        Ok(NpmPublishSettings {
            registry: self.registry.map(|r| r.trim_end_matches('/').to_owned()),
            scope,
            access,
            tag: self.tag,
            provenance: self.provenance,
            keywords: self.keywords,
            bugs: self.bugs,
            side_effects,
            engines: self.engines,
        })
    }
}

impl RawShade {
    fn validate(self) -> Result<ShadeSettings> {
        fn package_name(name: &str) -> bool {
            !name.is_empty()
                && name.split('.').all(|part| {
                    !part.is_empty()
                        && !part.starts_with(|c: char| c.is_ascii_digit())
                        && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                })
        }
        let mut relocate = Vec::new();
        for (from, to) in self.relocate {
            if !package_name(&from) || !package_name(&to) {
                bail!(
                    "[package.shade] `relocate` maps package names such as \"com.google.common\" = \"com.example.shaded.guava\", got `{from}` = `{to}`"
                );
            }
            if from == to {
                bail!("[package.shade] `relocate` moves `{from}` to itself");
            }
            relocate.push((from, to));
        }
        // The longest prefix wins when a class matches more than one.
        relocate.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        for (key, patterns) in [("exclude", &self.exclude), ("keep", &self.keep)] {
            if let Some(bad) = patterns.iter().find(|p| p.trim().is_empty() || p.contains('/') || p.trim_end_matches(".*").contains('*')) {
                bail!("[package.shade] `{key}` entries are class names or packages such as \"org.slf4j.*\", got `{bad}`");
            }
        }
        if !self.keep.is_empty() && self.minimize != Some(true) {
            bail!("[package.shade] `keep` lists what `minimize` keeps; set `minimize = true`");
        }
        Ok(ShadeSettings {
            relocate,
            exclude: self.exclude,
            minimize: self.minimize.unwrap_or(false),
            keep: self.keep,
            merge_services: self.merge_services.unwrap_or(true),
            relocate_strings: self.relocate_strings.unwrap_or(false),
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

impl RawOptimize {
    fn validate(self) -> Result<OptimizeSettings> {
        Ok(OptimizeSettings {
            shrink: self.shrink.unwrap_or(true),
            optimize: self.optimize.unwrap_or(true),
            obfuscate: self.obfuscate.unwrap_or(true),
            rules: self.rules.unwrap_or_else(|| PathBuf::from("proguard-rules.pro")),
        })
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
        if let Some(style) = &self.code_style
            && !FmtSettings::CODE_STYLES.contains(&style.as_str())
        {
            bail!("[fmt] `code-style` must be one of {}, got `{style}`", FmtSettings::CODE_STYLES.join(", "));
        }
        Ok(FmtSettings { ktlint_version: self.ktlint, code_style: self.code_style, exclude: self.exclude })
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
