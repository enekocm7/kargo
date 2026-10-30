//! `kargo doc`: generates HTML API documentation from KDoc with Dokka.
//!
//! Like the Kotlin compiler, Dokka is fetched from Maven Central and run
//! directly (its CLI jar plus the `dokka-base` and K2 analysis plugins), so no
//! Gradle is involved. The one exception is Android, whose classpath (`android.jar`
//! and AAR classes) comes from a task in the generated Gradle project.
//!
//! `src/` and each `src-<target>/` become Dokka source sets: a single target
//! documents both directories as one source set, while several targets get a
//! shared `common` source set for `src/` that every target source set depends on.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::build::{
    default_targets, finished, jvm_classpaths, jvm_compile_classpath, native_cinterop, native_libraries, native_platforms, select_targets,
    wasm_libraries,
};
use super::context::BuildContext;
use kargo_manifest::{DocSettings, GeneratedKind, TargetKind};
use kargo_maven::{DependencyRequest, ModuleId};
use kargo_util::fs::{collect_files, remove_dir_all_if_exists, write_string_atomic};
use kargo_util::process;
use kargo_util::shell::Verbosity;

/// Dokka version used unless `[doc] dokka` overrides it.
pub const DOKKA_VERSION: &str = "2.2.0";

const DOKKA_GROUP: &str = "org.jetbrains.dokka";

#[derive(Debug, Clone, Default)]
pub struct DocOptions {
    /// Targets to document; empty means every enabled target.
    pub targets: Vec<TargetKind>,
    /// Open the generated documentation in a browser.
    pub open: bool,
}

/// One Dokka source set.
#[derive(Debug, Clone, PartialEq)]
struct SourceSet {
    name: String,
    /// Dokka's `analysisPlatform`: `jvm` (also for Android), `wasm`, `native` or `common`.
    platform: &'static str,
    roots: Vec<PathBuf>,
    classpath: Vec<PathBuf>,
    depends_on: Vec<String>,
    /// Documentation sites linked from this source set only, on top of `[doc] external-links`.
    links: Vec<String>,
}

/// Android API reference, so `android.*` and `androidx.*` platform types become links.
const ANDROID_REFERENCE: &str = "https://developer.android.com/reference/kotlin/";

/// Generates the documentation and returns the path of its `index.html`.
pub fn doc(ctx: &BuildContext, opts: &DocOptions) -> Result<PathBuf> {
    let start = Instant::now();
    let targets = select_targets(ctx, &opts.targets)?;
    let index = generate(ctx, &targets)?.join("index.html");
    finished(ctx, start);
    ctx.shell().status("Generated", index.display());
    if opts.open {
        ctx.shell().status("Opening", index.display());
        crate::web::open_in_browser(index.as_os_str())?;
    }
    Ok(index)
}

/// The directory the HTML for `targets` goes to: `target/<profile>/doc` for the default targets, and
/// `target/<profile>/doc-<targets>` for any other set, so documenting some targets keeps the rest.
fn output_dir(ctx: &BuildContext, targets: &[TargetKind]) -> PathBuf {
    let mut names: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let mut default: Vec<&str> = default_targets(ctx).iter().map(|t| t.as_str()).collect();
    default.sort_unstable();
    let dir = if names == default { "doc".to_owned() } else { format!("doc-{}", names.join("-")) };
    ctx.ws.profile_dir(&ctx.profile).join(dir)
}

/// Runs Dokka for `targets` and returns the directory holding the HTML (see [`output_dir`]).
pub fn generate(ctx: &BuildContext, targets: &[TargetKind]) -> Result<PathBuf> {
    let ws = ctx.ws;
    let manifest = ws.manifest();

    // What `[[generate]]` steps write is documented with the rest of the code.
    super::build::generate_sources(ctx, targets)?;
    let mut dirs = vec![ws.common_sources()];
    dirs.extend(targets.iter().map(|t| ws.target_sources(*t)));
    dirs.extend(targets.iter().flat_map(|t| ws.manifest().all_groups_for(*t)).map(|g| ws.group_sources(&g.name)));
    dirs.extend(targets.iter().flat_map(|t| crate::generate::dirs(ws, &ctx.profile, *t, GeneratedKind::Sources)));
    if dirs.iter().all(|d| collect_files(d, &["kt", "java"]).is_empty()) {
        bail!("no Kotlin or Java sources to document; add them under `src/` or `src-<target>/`");
    }
    let source_sets = source_sets(ctx, targets)?;

    let jdk = ctx.jdk()?;
    let dokka_version = manifest.doc.dokka_version.as_deref().unwrap_or(DOKKA_VERSION);
    let (cli, plugins) = dokka(ctx, dokka_version)?;

    let pkg = &manifest.package;
    let out = output_dir(ctx, targets);
    let work = ws.profile_dir(&ctx.profile).join("dokka");
    std::fs::create_dir_all(&work).with_context(|| format!("failed to create {}", work.display()))?;
    remove_dir_all_if_exists(&out)?;

    let config = DokkaConfig {
        module_name: &pkg.name,
        module_version: &pkg.version,
        output: &out,
        cache: &work.join("cache"),
        plugins: &plugins,
        source_sets: &source_sets,
        settings: &manifest.doc,
        root: ws.root(),
        jdk_version: jdk_docs_version(manifest, jdk.version),
        offline: ctx.config.offline(),
    };
    let config_path = work.join("dokka.json");
    let text = serde_json::to_string_pretty(&config.to_json()).context("failed to serialize the Dokka configuration")?;
    write_string_atomic(&config_path, &text)?;

    let names: Vec<&str> = source_sets.iter().map(|s| s.name.as_str()).collect();
    ctx.shell().status("Documenting", format!("{} v{} ({})", pkg.name, pkg.version, names.join(", ")));
    let mut cmd = Command::new(jdk.java());
    if jdk.version >= 23 {
        // The bundled IntelliJ code uses sun.misc.Unsafe, which JDK 23+ warns about on every run.
        cmd.arg("--sun-misc-unsafe-memory-access=allow");
    }
    let logging = if ctx.shell().verbosity() == Verbosity::Verbose { "INFO" } else { "WARN" };
    cmd.current_dir(ws.root()).arg("-jar").arg(&cli).args(["-loggingLevel", logging]).arg(&config_path);
    let status = process::status(ctx.shell(), &mut cmd, "Dokka")?;
    if !status.success() {
        bail!("Dokka failed ({}); see its output above", process::describe_status(status));
    }
    let index = out.join("index.html");
    if !index.is_file() {
        bail!("Dokka finished but did not write {}", index.display());
    }
    Ok(out)
}

/// The names a source set depends on: the ones it refines, else `common`.
fn refined(parents: &[&str]) -> Vec<String> {
    if parents.is_empty() { vec!["common".to_owned()] } else { parents.iter().map(|p| (*p).to_owned()).collect() }
}

/// Builds the Dokka source sets for `targets` from the project layout.
fn source_sets(ctx: &BuildContext, targets: &[TargetKind]) -> Result<Vec<SourceSet>> {
    let ws = ctx.ws;
    let common = ws.common_sources();
    let mut sets: Vec<SourceSet> = Vec::new();
    let single = targets.len() == 1;
    if !single {
        // Analyse `src/` once, as common code, so no file belongs to two source sets.
        sets.push(SourceSet {
            name: "common".to_owned(),
            platform: "common",
            roots: existing(vec![common.clone()]),
            classpath: Vec::new(),
            depends_on: Vec::new(),
            links: Vec::new(),
        });
        // The intermediate source sets (`[source-sets.<name>]`) sit between common code and the targets.
        let mut seen: Vec<&str> = Vec::new();
        for (group, parents) in targets.iter().flat_map(|t| ctx.ws.manifest().source_groups_for(*t)) {
            if !seen.contains(&group.name.as_str()) {
                seen.push(&group.name);
                sets.push(SourceSet {
                    name: group.name.clone(),
                    platform: "common",
                    roots: existing(vec![ws.group_sources(&group.name)]),
                    classpath: Vec::new(),
                    depends_on: refined(&parents),
                    links: Vec::new(),
                });
            }
        }
    }
    for &target in targets {
        let mut roots = vec![ws.target_sources(target)];
        let groups = ws.manifest().source_groups_for(target);
        if single {
            roots.extend(groups.iter().map(|(g, _)| ws.group_sources(&g.name)));
        }
        // The sets of some Native platforms are documented with the Native target's own code.
        if target == TargetKind::Native {
            roots.extend(ws.manifest().native_platform_groups(None).iter().map(|(g, _)| ws.group_sources(&g.name)));
        }
        roots.extend(crate::generate::dirs(ws, &ctx.profile, target, GeneratedKind::Sources));
        if single {
            roots.insert(0, common.clone());
        }
        // The target refines the narrowest source sets, those that no other one of its list refines.
        let leaves: Vec<&str> = groups
            .iter()
            .filter(|(g, _)| !groups.iter().any(|(_, p)| p.contains(&g.name.as_str())))
            .map(|(g, _)| g.name.as_str())
            .collect();
        let (platform, classpath) = classpath(ctx, target)?;
        let links = if target == TargetKind::Android { vec![ANDROID_REFERENCE.to_owned()] } else { Vec::new() };
        sets.push(SourceSet {
            name: target.as_str().to_owned(),
            platform,
            roots: existing(roots),
            classpath,
            depends_on: if single { Vec::new() } else { refined(&leaves) },
            links,
        });
    }
    if !single {
        // Common code is analysed against klibs (Wasm or Native), which carry the
        // full Kotlin metadata of multiplatform libraries; JVM and Android jars
        // do not. Without a klib target, common code only sees the standard library.
        if let Some(klib_set) = sets.iter().find(|s| s.platform != "jvm" && s.platform != "common") {
            let classpath = klib_set.classpath.clone();
            for set in sets.iter_mut().filter(|s| s.platform == "common") {
                set.classpath = classpath.clone();
            }
        }
    }
    Ok(sets)
}

/// The Java version whose API docs JDK types link to: the JVM target, else the running JDK.
fn jdk_docs_version(manifest: &kargo_manifest::Manifest, running: u32) -> u32 {
    let target = manifest.targets.jvm.as_ref().and_then(|j| j.target_for(running).ok());
    target.as_deref().and_then(|t| t.strip_prefix("1.").unwrap_or(t).parse().ok()).unwrap_or(running)
}

fn existing(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

/// Dokka's analysis platform and the libraries the sources of `target` compile against.
fn classpath(ctx: &BuildContext, target: TargetKind) -> Result<(&'static str, Vec<PathBuf>)> {
    let setup = ctx.setup(target)?;
    Ok(match target {
        TargetKind::Jvm => ("jvm", jvm_compile_classpath(ctx, &jvm_classpaths(ctx, &setup)?)?),
        TargetKind::Wasm => ("wasm", wasm_libraries(ctx, &setup, false)?),
        TargetKind::Js => ("js", super::build::js_libraries(ctx, &setup, false)?),
        TargetKind::Native => {
            // The API is the same on every platform, so the first one is enough.
            let platform = native_platforms(ctx)?.into_iter().next().context("[target.native] lists no platforms")?;
            let mut libs = native_libraries(ctx, &setup, &platform, false)?;
            libs.extend(native_cinterop(ctx, &platform)?);
            let home = &ctx.native_toolchain()?.home;
            libs.extend(klibs(&home.join("klib").join("common")));
            libs.extend(klibs(&home.join("klib").join("platform").join(&platform)));
            ("native", libs)
        }
        TargetKind::Android => {
            // Gradle resolves Android dependencies; AGP unpacks the AARs into jars.
            ctx.shell().status("Resolving", "android classpath (gradle)");
            ("jvm", crate::gradle::doc_classpath(ctx)?)
        }
    })
}

/// The klibs of a Kotlin/Native distribution directory, in a stable order.
fn klibs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    out.sort();
    out
}

/// `kargo fetch --tools`: downloads the Dokka CLI and the plugins `kargo doc` runs with.
pub fn fetch_tool(ctx: &BuildContext) -> Result<()> {
    let version = ctx.ws.manifest().doc.dokka_version.as_deref().unwrap_or(DOKKA_VERSION);
    dokka(ctx, version)?;
    Ok(())
}

/// Fetches the Dokka CLI jar and the plugin classpath (`dokka-base` and the K2 analysis).
fn dokka(ctx: &BuildContext, version: &str) -> Result<(PathBuf, Vec<PathBuf>)> {
    let cli = ctx.tool_jar(DOKKA_GROUP, "dokka-cli", version).with_context(|| format!("failed to fetch Dokka {version}"))?;
    let roots: Vec<DependencyRequest> = ["dokka-base", "analysis-kotlin-symbols"]
        .iter()
        .map(|a| DependencyRequest::new(ModuleId::new(DOKKA_GROUP, *a), Some(version.to_owned())))
        .collect();
    let plugins =
        ctx.resolve_tool("dokka-plugins", &roots).with_context(|| format!("failed to obtain the Dokka {version} plugins"))?.files();
    Ok((cli, plugins))
}

/// Everything that goes into Dokka's JSON configuration.
struct DokkaConfig<'a> {
    module_name: &'a str,
    module_version: &'a str,
    output: &'a Path,
    cache: &'a Path,
    plugins: &'a [PathBuf],
    source_sets: &'a [SourceSet],
    settings: &'a DocSettings,
    root: &'a Path,
    jdk_version: u32,
    offline: bool,
}

impl DokkaConfig<'_> {
    fn to_json(&self) -> Value {
        let paths = |ps: &[PathBuf]| ps.iter().map(|p| path_str(p)).collect::<Vec<_>>();
        let id = |name: &str| json!({ "scopeId": self.module_name, "sourceSetName": name });
        let includes: Vec<String> = self.settings.includes.iter().map(|p| path_str(&self.root.join(p))).collect();
        let source_links: Vec<Value> = match &self.settings.source_link {
            Some(url) => vec![json!({ "localDirectory": path_str(self.root), "remoteUrl": url, "remoteLineSuffix": "#L" })],
            None => Vec::new(),
        };
        let external_link = |url: &String| {
            let base = if url.ends_with('/') { url.clone() } else { format!("{url}/") };
            json!({ "url": base, "packageListUrl": format!("{base}package-list") })
        };
        let source_sets: Vec<Value> = self
            .source_sets
            .iter()
            .map(|s| {
                json!({
                    "displayName": s.name,
                    "sourceSetID": id(&s.name),
                    "analysisPlatform": s.platform,
                    "sourceRoots": paths(&s.roots),
                    "classpath": paths(&s.classpath),
                    "dependentSourceSets": s.depends_on.iter().map(|d| id(d)).collect::<Vec<_>>(),
                    "includes": includes,
                    "sourceLinks": source_links,
                    "externalDocumentationLinks": self.settings.external_links.iter().chain(&s.links).map(external_link).collect::<Vec<_>>(),
                    "documentedVisibilities": ["PUBLIC", "PROTECTED"],
                    "skipEmptyPackages": true,
                    "jdkVersion": self.jdk_version,
                })
            })
            .collect();
        json!({
            "moduleName": self.module_name,
            "moduleVersion": self.module_version,
            "outputDir": path_str(self.output),
            "cacheRoot": path_str(self.cache),
            "offlineMode": self.offline,
            "pluginsClasspath": paths(self.plugins),
            "sourceSets": source_sets,
        })
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(name: &str, platform: &'static str, depends_on: &[&str]) -> SourceSet {
        SourceSet {
            name: name.to_owned(),
            platform,
            roots: vec![PathBuf::from("/p/src")],
            classpath: vec![PathBuf::from("/m2/lib.jar")],
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            links: Vec::new(),
        }
    }

    #[test]
    fn writes_dokka_configuration() {
        let settings = DocSettings {
            dokka_version: None,
            includes: vec![PathBuf::from("docs/module.md")],
            source_link: Some("https://github.com/me/lib/blob/main".to_owned()),
            external_links: vec!["https://kotlinlang.org/api/kotlinx.coroutines".to_owned()],
        };
        let mut android = set("android", "jvm", &["common"]);
        android.links.push(ANDROID_REFERENCE.to_owned());
        let sets = [set("common", "common", &[]), set("jvm", "jvm", &["common"]), android];
        let plugins = [PathBuf::from("/m2/dokka-base.jar")];
        let config = DokkaConfig {
            module_name: "lib",
            module_version: "1.0.0",
            output: Path::new("/p/target/debug/doc"),
            cache: Path::new("/p/target/debug/dokka/cache"),
            plugins: &plugins,
            source_sets: &sets,
            settings: &settings,
            root: Path::new("/p"),
            jdk_version: 21,
            offline: true,
        };
        let json = config.to_json();
        assert_eq!(json["moduleName"], "lib");
        assert_eq!(json["offlineMode"], true);
        assert_eq!(json["pluginsClasspath"][0], path_str(Path::new("/m2/dokka-base.jar")));
        let jvm = &json["sourceSets"][1];
        assert_eq!(jvm["analysisPlatform"], "jvm");
        assert_eq!(jvm["sourceSetID"], json!({ "scopeId": "lib", "sourceSetName": "jvm" }));
        assert_eq!(jvm["dependentSourceSets"], json!([{ "scopeId": "lib", "sourceSetName": "common" }]));
        assert_eq!(jvm["jdkVersion"], 21);
        assert_eq!(jvm["includes"][0], path_str(&Path::new("/p").join("docs/module.md")));
        assert_eq!(jvm["sourceLinks"][0]["remoteUrl"], "https://github.com/me/lib/blob/main");
        let link = &jvm["externalDocumentationLinks"][0];
        assert_eq!(link["url"], "https://kotlinlang.org/api/kotlinx.coroutines/");
        assert_eq!(link["packageListUrl"], "https://kotlinlang.org/api/kotlinx.coroutines/package-list");
        assert_eq!(json["sourceSets"][0]["dependentSourceSets"], json!([]));
        // Android also links to the Android reference; the other source sets don't.
        assert_eq!(jvm["externalDocumentationLinks"].as_array().unwrap().len(), 1);
        let android = &json["sourceSets"][2]["externalDocumentationLinks"];
        assert_eq!(android[1]["packageListUrl"], format!("{ANDROID_REFERENCE}package-list"));
    }
}
