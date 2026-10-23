//! Web resources: the files a Kotlin/Wasm library ships to the browser apps that depend on it
//! (`[target.wasm] web-resources`), such as an Emscripten module its bindings call, and the init
//! script (`web-init`) the page Kargo generates imports before the app module.
//!
//! A path dependency's files are copied from its package. `kargo publish` puts them in a zip under
//! the `web-resources` classifier and announces it in the POM ([`WEB_RESOURCES_PROPERTY`], with the
//! init script in [`WEB_INIT_PROPERTY`]), so a Maven dependency's zip is fetched only when its POM
//! says there is one. The zips are pinned in `kargo.lock` like any other artifact.
//!
//! A runtime jar in the resolution, such as Skiko's `skiko-js-wasm-runtime` that Compose for Web
//! draws with, ships its files the same way: everything in the jar but `META-INF/`.
//!
//! Every file lands next to the app module under its own name (a directory keeps its name and
//! contents). The app's own `resources/` wins over a dependency's file; two dependencies shipping
//! the same name is an error.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::ops::build::wasm_resolution;
use crate::ops::context::BuildContext;
use crate::publication::WebResourcesArchive;
use crate::workspace::Workspace;
use kargo_manifest::{Scope, TargetKind};
use kargo_maven::pom::{Pom, WEB_INIT_PROPERTY, WEB_RESOURCES_PROPERTY};
use kargo_maven::resolver::ResolvedNode;
use kargo_maven::{Artifact, ModuleId, Resolution};
use kargo_util::process;

/// The classifier of the zip a library's web resources are published in.
pub const CLASSIFIER: &str = "web-resources";

/// Where a shipped file's bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    File(PathBuf),
    Zip { archive: PathBuf, entry: String },
}

/// One file a library ships, under the name it gets in the app's output (`raylib.mjs`, `assets/logo.png`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFile {
    pub name: String,
    source: Source,
}

/// What one library ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shipment {
    /// The library, for messages: a package name or `group:artifact:version`.
    pub owner: String,
    pub files: Vec<WebFile>,
    /// The name of the init script among `files`.
    pub init: Option<String>,
}

/// The web resources of the libraries `ctx`'s package depends on (with `dev`, also through its
/// dev-dependencies), in dependency order: a library comes after the libraries it depends on, so
/// their init scripts run first.
pub fn dependencies(ctx: &BuildContext, dev: bool) -> Result<Vec<Shipment>> {
    let mut locals = ctx.local_dependencies(TargetKind::Wasm, false)?;
    if dev {
        locals.extend(ctx.local_dependencies(TargetKind::Wasm, true)?);
    }
    let mut shipments = Vec::new();
    // A `provided` library is not there at runtime, so neither are its files.
    for dep in locals.iter().filter(|d| d.scope != Scope::Provided) {
        shipments.extend(own(ctx.project.package(dep.package), true)?);
    }
    let setup = ctx.setup(TargetKind::Wasm)?;
    let resolution = wasm_resolution(ctx, &setup, dev)?;
    shipments.extend(published(ctx, &resolution, dev)?);
    shipments.extend(runtime_jars(&resolution)?);
    // Both lists are breadth-first, nearest first; path dependencies may use Maven libraries, never the reverse.
    shipments.reverse();
    Ok(shipments)
}

/// `kargo fetch`: downloads the zips of the Maven libraries a build and its tests would ship, so an
/// offline build finds them.
pub fn fetch(ctx: &BuildContext) -> Result<()> {
    let setup = ctx.setup(TargetKind::Wasm)?;
    for dev in [false, true] {
        published(ctx, &wasm_resolution(ctx, &setup, dev)?, dev)?;
    }
    Ok(())
}

/// What `ws` itself ships, read from its package: `None` when it declares nothing. `as_dependency`
/// words a missing file for the package that depends on it.
pub fn own(ws: &Workspace, as_dependency: bool) -> Result<Option<Shipment>> {
    let Some(wasm) = ws.manifest().targets.wasm.as_ref() else { return Ok(None) };
    if wasm.web_resources.is_empty() && wasm.web_init.is_none() {
        return Ok(None);
    }
    let mut files: Vec<WebFile> = Vec::new();
    for declared in wasm.web_resources.iter().chain(&wasm.web_init) {
        let path = ws.root().join(declared);
        let name = file_name(declared);
        if path.is_dir() {
            for file in crate::wasm_output::all_files(&path) {
                let relative = file.strip_prefix(&path).unwrap_or(&file);
                files.push(WebFile { name: format!("{name}/{}", process::slash(relative)), source: Source::File(file) });
            }
        } else if path.is_file() {
            // The init script may be listed among the resources too.
            if !files.iter().any(|f| f.name == name) {
                files.push(WebFile { name, source: Source::File(path) });
            }
        } else if as_dependency {
            bail!(
                "`{}` ships the web resource `{}`, but {} does not exist; a path dependency is built without its \
                 `pre-build` hooks, so if one of its tasks makes the file, run it in {} first",
                ws.name(),
                declared.display(),
                path.display(),
                ws.root().display()
            );
        } else {
            bail!("[target.wasm] of `{}` ships `{}`, but {} does not exist", ws.name(), declared.display(), path.display());
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Some(Shipment { owner: ws.name().to_owned(), files, init: wasm.web_init.as_deref().map(file_name) }))
}

/// `kargo publish`: writes what `ws` ships into the zip `dest`, or returns `None` when it ships nothing.
pub fn archive(ws: &Workspace, dest: &Path) -> Result<Option<WebResourcesArchive>> {
    let Some(shipment) = own(ws, false)? else { return Ok(None) };
    let file = std::fs::File::create(dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    // Fixed timestamps, so the same files make the same zip.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    for file in &shipment.files {
        zip.start_file(file.name.as_str(), options)?;
        zip.write_all(&read(&file.source)?)?;
    }
    zip.finish().with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(Some(WebResourcesArchive { zip: dest.to_path_buf(), init: shipment.init }))
}

/// The web resources of the Maven libraries in `resolution` whose POM announces them, with their
/// zips fetched and pinned in `kargo.lock`.
fn published(ctx: &BuildContext, resolution: &Resolution, dev: bool) -> Result<Vec<Shipment>> {
    struct Wanted<'a> {
        node: &'a ResolvedNode,
        rel: String,
        init: Option<String>,
    }
    let mut wanted = Vec::new();
    // A module without files is a POM-only module or the root of a multiplatform library; its platform module has the zip.
    for node in resolution.nodes.iter().filter(|n| !n.files.is_empty()) {
        let Some(classifier) = pom_property(ctx, &node.module, &node.version, WEB_RESOURCES_PROPERTY)? else { continue };
        let artifact = Artifact::new(node.module.clone(), &node.version).with_classifier(Some(classifier)).with_extension("zip");
        let rel = ctx.client.artifact_path(&artifact)?;
        let init = pom_property(ctx, &node.module, &node.version, WEB_INIT_PROPERTY)?;
        wanted.push(Wanted { node, rel, init });
    }
    let inputs: Vec<String> = wanted.iter().map(|w| format!("{}:{} {}", w.node.module, w.node.version, w.rel)).collect();
    let key = if dev { "wasm/web-resources-test" } else { "wasm/web-resources" };
    let locked = ctx.lock_files(key, &inputs, || {
        let nodes = wanted
            .iter()
            .map(|w| -> Result<ResolvedNode> {
                let zip = ctx.client.fetch_verified(&w.rel).with_context(|| {
                    format!("{}:{} says it ships web resources, but its zip could not be fetched", w.node.module, w.node.version)
                })?;
                Ok(ResolvedNode {
                    module: w.node.module.clone(),
                    version: w.node.version.clone(),
                    files: vec![zip],
                    dependencies: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Resolution { roots: Vec::new(), nodes })
    })?;
    let mut shipments = Vec::new();
    for w in &wanted {
        let owner = format!("{}:{}", w.node.module, w.node.version);
        let zip = locked
            .node(&w.node.module)
            .and_then(|n| n.files.first())
            .with_context(|| format!("kargo.lock has no web resources of {owner}; run `kargo update`"))?;
        shipments.push(from_archive(owner, zip, w.init.clone())?);
    }
    Ok(shipments)
}

/// The files of the runtime jars in `resolution`: a Wasm library is a klib, so a jar there holds
/// files for the page, like Skiko's `skiko.mjs` and `skiko.wasm`.
fn runtime_jars(resolution: &Resolution) -> Result<Vec<Shipment>> {
    let mut shipments = Vec::new();
    for node in &resolution.nodes {
        for jar in node.files.iter().filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar"))) {
            let mut shipment = from_archive(format!("{}:{}", node.module, node.version), jar, None)?;
            shipment.files.retain(|f| !f.name.starts_with("META-INF/"));
            shipments.push(shipment);
        }
    }
    Ok(shipments)
}

/// What a directory ships, under the same relative paths: the processed `composeResources/` of a Compose app.
pub fn from_dir(owner: &str, dir: &Path) -> Shipment {
    let mut files: Vec<WebFile> = crate::wasm_output::all_files(dir)
        .into_iter()
        .map(|file| {
            let relative = file.strip_prefix(dir).unwrap_or(&file);
            WebFile { name: process::slash(relative), source: Source::File(file.clone()) }
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Shipment { owner: owner.to_owned(), files, init: None }
}

/// A property of the POM of `module:version`, if the POM is there and sets it.
fn pom_property(ctx: &BuildContext, module: &ModuleId, version: &str, name: &str) -> Result<Option<String>> {
    let rel = ctx.client.artifact_path(&Artifact::new(module.clone(), version).with_extension("pom"))?;
    let Some(text) = ctx.client.fetch_text(&rel)? else { return Ok(None) };
    let pom = Pom::parse(&text).with_context(|| format!("failed to read the POM of {module}:{version}"))?;
    Ok(pom.properties.get(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()))
}

/// What a published zip ships. An entry whose path would leave the output directory is an error.
fn from_archive(owner: String, path: &Path, init: Option<String>) -> Result<Shipment> {
    let file = std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("the web resources of {owner} ({}) are not a zip", path.display()))?;
    let mut files = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name()?.into_owned();
        let Some(name) = entry.enclosed_name().filter(|n| n.components().all(|c| matches!(c, std::path::Component::Normal(_)))) else {
            bail!("the web resources of {owner} contain `{raw}`, which is not a path inside the app's output");
        };
        files.push(WebFile { name: process::slash(&name), source: Source::Zip { archive: path.to_path_buf(), entry: raw } });
    }
    if let Some(init) = &init
        && !files.iter().any(|f| &f.name == init)
    {
        bail!("{owner} names `{init}` as its web init script, but its web resources have no such file");
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Shipment { owner, files, init })
}

/// The files to copy into an app's output: every shipped file except those the app's own resources
/// (`own`) already provide. Fails when two libraries ship the same name, or a library a name the
/// build writes itself (`reserved`: the module's files and the page).
pub fn plan<'a>(shipments: &'a [Shipment], own: &[String], reserved: &[String]) -> Result<Vec<&'a WebFile>> {
    let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
    let mut files = Vec::new();
    for shipment in shipments {
        for file in &shipment.files {
            if reserved.contains(&file.name) {
                bail!("{} ships `{}`, which is a file of the app's own build; the library has to rename it", shipment.owner, file.name);
            }
            if let Some(other) = owners.insert(&file.name, &shipment.owner) {
                bail!(
                    "{other} and {} both ship the web resource `{}`; an app can only have one file of that name next to its module",
                    shipment.owner,
                    file.name
                );
            }
            if !own.contains(&file.name) {
                files.push(file);
            }
        }
    }
    Ok(files)
}

/// Copies `files` into `dir` and returns their names.
pub fn copy(files: &[&WebFile], dir: &Path) -> Result<Vec<String>> {
    let mut copied = Vec::new();
    for file in files {
        let dest = dir.join(&file.name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, read(&file.source)?).with_context(|| format!("failed to write {}", dest.display()))?;
        copied.push(file.name.clone());
    }
    Ok(copied)
}

/// The init scripts to import, in order, as the page names them (`./init.mjs`).
pub fn init_scripts(shipments: &[Shipment]) -> Vec<String> {
    shipments.iter().filter_map(|s| s.init.as_ref()).map(|init| format!("./{init}")).collect()
}

fn read(source: &Source) -> Result<Vec<u8>> {
    match source {
        Source::File(path) => std::fs::read(path).with_context(|| format!("failed to read {}", path.display())),
        Source::Zip { archive, entry } => {
            let file = std::fs::File::open(archive).with_context(|| format!("failed to open {}", archive.display()))?;
            let mut zip = zip::ZipArchive::new(file).with_context(|| format!("failed to read {}", archive.display()))?;
            let mut entry = zip.by_name(entry).with_context(|| format!("`{entry}` is missing from {}", archive.display()))?;
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            Ok(bytes)
        }
    }
}

/// The name a declared path ships under: its last component.
fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipment(owner: &str, names: &[&str], init: Option<&str>) -> Shipment {
        Shipment {
            owner: owner.to_owned(),
            files: names.iter().map(|n| WebFile { name: (*n).to_owned(), source: Source::File(PathBuf::from(n)) }).collect(),
            init: init.map(str::to_owned),
        }
    }

    fn names(files: &[&WebFile]) -> Vec<String> {
        files.iter().map(|f| f.name.clone()).collect()
    }

    #[test]
    fn the_apps_own_resources_win_and_clashes_are_errors() {
        let shipments =
            [shipment("raylib-kt", &["raylib.mjs", "raylib.wasm", "init.mjs"], Some("init.mjs")), shipment("ui", &["style.css"], None)];
        let files = plan(&shipments, &["style.css".to_owned()], &["app.mjs".to_owned()]).unwrap();
        assert_eq!(names(&files), ["raylib.mjs", "raylib.wasm", "init.mjs"]);
        assert_eq!(init_scripts(&shipments), ["./init.mjs"]);

        let clash = [shipment("a", &["x.mjs"], None), shipment("org.acme:b:1.0", &["x.mjs"], None)];
        let err = plan(&clash, &[], &[]).unwrap_err().to_string();
        assert!(err.contains("a and org.acme:b:1.0 both ship the web resource `x.mjs`"), "{err}");

        let reserved = [shipment("a", &["index.html"], None)];
        let err = plan(&reserved, &[], &["index.html".to_owned()]).unwrap_err().to_string();
        assert!(err.contains("a ships `index.html`, which is a file of the app's own build"), "{err}");
    }

    #[test]
    fn runtime_jars_ship_everything_but_their_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("skiko-js-wasm-runtime-0.150.1.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        for (name, contents) in [("META-INF/MANIFEST.MF", "Manifest-Version: 1.0"), ("skiko.mjs", "export {}"), ("skiko.wasm", "wasm")] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let klib = dir.path().join("skiko-wasm-js-0.150.1.klib");
        let node = |artifact: &str, file: &Path| ResolvedNode {
            module: ModuleId::new("org.jetbrains.skiko", artifact),
            version: "0.150.1".into(),
            files: vec![file.to_path_buf()],
            dependencies: Vec::new(),
        };
        let resolution = Resolution { roots: Vec::new(), nodes: vec![node("skiko-wasm-js", &klib), node("skiko-js-wasm-runtime", &jar)] };
        let shipments = runtime_jars(&resolution).unwrap();
        assert_eq!(shipments.len(), 1, "a klib ships nothing");
        assert_eq!(shipments[0].owner, "org.jetbrains.skiko:skiko-js-wasm-runtime:0.150.1");
        let files: Vec<&str> = shipments[0].files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(files, ["skiko.mjs", "skiko.wasm"]);

        // The processed `composeResources/` of a Compose app keep their paths.
        let resources = dir.path().join("resources");
        std::fs::create_dir_all(resources.join("composeResources/app.res/values")).unwrap();
        std::fs::write(resources.join("composeResources/app.res/values/strings.cvr"), "string|a|Yg==\n").unwrap();
        let shipment = from_dir("the app's composeResources/", &resources);
        assert_eq!(shipment.files[0].name, "composeResources/app.res/values/strings.cvr");
    }

    #[test]
    fn a_published_zip_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("lib");
        for (path, contents) in [("web/raylib.mjs", "export default 1"), ("web/init.mjs", "init"), ("web/assets/logo.png", "png")] {
            let file = package.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        }
        std::fs::write(
            package.join("kargo.toml"),
            "[package]\nname = \"raylib-kt\"\nversion = \"0.4.0\"\n\n[target.wasm]\nenvironment = \"browser\"\nkind = \"library\"\n\
             web-resources = [\"web/raylib.mjs\", \"web/assets\"]\nweb-init = \"web/init.mjs\"\n",
        )
        .unwrap();
        let ws = Workspace::load(&package.join("kargo.toml")).unwrap();
        let own = own(&ws, false).unwrap().unwrap();
        assert_eq!(own.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["assets/logo.png", "init.mjs", "raylib.mjs"]);

        let zip = dir.path().join("web.zip");
        let archived = archive(&ws, &zip).unwrap().unwrap();
        assert_eq!(archived.init.as_deref(), Some("init.mjs"));
        let first = std::fs::read(&zip).unwrap();
        archive(&ws, &zip).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), first, "the same files make the same zip");

        let published = from_archive("org.acme:raylib-kt:0.4.0".into(), &zip, archived.init).unwrap();
        assert_eq!(published.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["assets/logo.png", "init.mjs", "raylib.mjs"]);
        let out = dir.path().join("out");
        let copied = copy(&plan(std::slice::from_ref(&published), &[], &[]).unwrap(), &out).unwrap();
        assert_eq!(copied.len(), 3);
        assert_eq!(std::fs::read_to_string(out.join("assets/logo.png")).unwrap(), "png");
        assert_eq!(std::fs::read_to_string(out.join("raylib.mjs")).unwrap(), "export default 1");

        let err = from_archive("x".into(), &zip, Some("missing.mjs".into())).unwrap_err().to_string();
        assert!(err.contains("names `missing.mjs` as its web init script"), "{err}");
    }

    #[test]
    fn a_missing_file_of_a_path_dependency_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("kargo.toml"),
            "[package]\nname = \"raylib-kt\"\nversion = \"0.4.0\"\n\n[target.wasm]\nkind = \"library\"\nweb-resources = [\"web/raylib.mjs\"]\n",
        )
        .unwrap();
        let ws = Workspace::load(&dir.path().join("kargo.toml")).unwrap();
        let err = own(&ws, true).unwrap_err().to_string();
        assert!(err.contains("`raylib-kt` ships the web resource `web/raylib.mjs`") && err.contains("`pre-build` hooks"), "{err}");
        assert!(own(&ws, false).unwrap_err().to_string().contains("[target.wasm] of `raylib-kt` ships `web/raylib.mjs`"));
    }

    #[test]
    fn zip_entries_cannot_leave_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("../../escape.js", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        let err = from_archive("org.evil:lib:1".into(), &path, None).unwrap_err().to_string();
        assert!(err.contains("contain `../../escape.js`, which is not a path inside the app's output"), "{err}");
    }
}
