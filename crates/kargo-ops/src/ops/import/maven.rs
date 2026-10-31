//! Reading a Maven project: `pom.xml`, its parents and its modules.
//!
//! Dependencies, properties, BOMs and parents come from Kargo's own POM support (`kargo_maven::pom`),
//! so they are the ones the build resolves. This module adds what that model leaves out: the
//! `<build>` plugins, the repositories, the modules and the project's metadata.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_manifest::{DEFAULT_KOTLIN_VERSION, validate_package_name};
use kargo_maven::pom::{
    EffectivePom, Pom, PomDependency, PomSource, effective_pom_without_imports, effective_properties, interpolate, pom_chain,
};
use kargo_maven::{Artifact, MavenClient, ModuleId, Repository as MavenRepository};
use kargo_util::config::Config;
use kargo_util::fs::dunce_canonicalize;

use super::frameworks::{self, Hints};
use super::model::{CompilerPlugin, Dependency, DirMove, KotlinSettings, LocalPackage, Package, Repository, Scope};
use super::{Imported, relative_dir};

/// The documentation page about moving from Maven.
const MIGRATION: &str = "migration/from-maven/";

/// A parsed XML element with the parts of a POM the importer reads.
#[derive(Debug, Clone, Default)]
struct Xml {
    name: String,
    text: String,
    children: Vec<Xml>,
}

/// Top-level elements the importer never reads (they can be huge, as in a BOM).
const SKIPPED: &[&str] = &["dependencyManagement", "dependencies", "properties", "profiles", "reporting"];

impl Xml {
    fn from_node(node: roxmltree::Node<'_, '_>, depth: usize) -> Self {
        let name = node.tag_name().name().to_owned();
        let text: String = node.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect();
        let children = if depth == 1 && SKIPPED.contains(&name.as_str()) {
            Vec::new()
        } else {
            node.children().filter(|c| c.is_element()).map(|c| Xml::from_node(c, depth + 1)).collect()
        };
        Self { name, text: text.trim().to_owned(), children }
    }

    fn child(&self, name: &str) -> Option<&Xml> {
        self.children.iter().find(|c| c.name == name)
    }

    fn at(&self, path: &[&str]) -> Option<&Xml> {
        path.iter().try_fold(self, |node, name| node.child(name))
    }

    fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Xml> {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn text_at(&self, path: &[&str]) -> Option<String> {
        self.at(path).map(|n| n.text.clone()).filter(|t| !t.is_empty())
    }
}

/// A `<plugin>` of `<build>`.
#[derive(Debug, Clone)]
struct Plugin {
    artifact: String,
    version: Option<String>,
    /// The plugin's `<configuration>` and the ones of its executions.
    configs: Vec<Xml>,
    goals: Vec<String>,
}

impl Plugin {
    fn from_xml(node: &Xml) -> Option<Self> {
        let artifact = node.text_at(&["artifactId"])?;
        let mut configs: Vec<Xml> = node.child("configuration").cloned().into_iter().collect();
        let mut goals = Vec::new();
        for execution in node.child("executions").into_iter().flat_map(|e| e.named("execution")) {
            configs.extend(execution.child("configuration").cloned());
            goals.extend(execution.child("goals").into_iter().flat_map(|g| g.named("goal")).map(|g| g.text.clone()));
        }
        Some(Self { artifact, version: node.text_at(&["version"]), configs, goals })
    }
}

/// The facts of one POM file the importer reads besides what [`Pom`] holds.
#[derive(Debug, Clone, Default)]
struct Extras {
    plugins: Vec<Plugin>,
    managed_plugins: Vec<Plugin>,
    repositories: Vec<(String, String)>,
    modules: Vec<String>,
    description: Option<String>,
    url: Option<String>,
    licenses: Vec<String>,
    scm_url: Option<String>,
    developers: Vec<(String, Option<String>)>,
    source_directory: Option<String>,
    test_source_directory: Option<String>,
    resources: Vec<String>,
    test_resources: Vec<String>,
    relative_path: Option<String>,
}

impl Extras {
    fn parse(xml: &str) -> Result<Self> {
        let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(xml, options).context("malformed POM XML")?;
        let project = Xml::from_node(doc.root_element(), 0);
        let plugin_list = |node: Option<&Xml>| -> Vec<Plugin> {
            node.map(|n| n.named("plugin").filter_map(Plugin::from_xml).collect()).unwrap_or_default()
        };
        let build = project.child("build");
        let directories = |tag: &str, wrapper: &str| -> Vec<String> {
            build.and_then(|b| b.child(wrapper)).into_iter().flat_map(|r| r.named(tag)).filter_map(|r| r.text_at(&["directory"])).collect()
        };
        Ok(Self {
            plugins: plugin_list(build.and_then(|b| b.child("plugins"))),
            managed_plugins: plugin_list(build.and_then(|b| b.at(&["pluginManagement", "plugins"]))),
            repositories: project
                .child("repositories")
                .into_iter()
                .flat_map(|r| r.named("repository"))
                .filter_map(|r| Some((r.text_at(&["id"])?, r.text_at(&["url"])?)))
                .collect(),
            modules: project
                .child("modules")
                .into_iter()
                .flat_map(|m| m.named("module"))
                .map(|m| m.text.clone())
                .filter(|m| !m.is_empty())
                .collect(),
            description: project.text_at(&["description"]),
            url: project.text_at(&["url"]),
            licenses: project
                .child("licenses")
                .into_iter()
                .flat_map(|l| l.named("license"))
                .map(|l| format!("{} {}", l.text_at(&["name"]).unwrap_or_default(), l.text_at(&["url"]).unwrap_or_default()))
                .collect(),
            scm_url: project.text_at(&["scm", "url"]),
            developers: project
                .child("developers")
                .into_iter()
                .flat_map(|d| d.named("developer"))
                .filter_map(|d| Some((d.text_at(&["name"]).or_else(|| d.text_at(&["id"]))?, d.text_at(&["email"]))))
                .collect(),
            source_directory: build.and_then(|b| b.text_at(&["sourceDirectory"])),
            test_source_directory: build.and_then(|b| b.text_at(&["testSourceDirectory"])),
            resources: directories("resource", "resources"),
            test_resources: directories("testResource", "testResources"),
            relative_path: project.child("parent").map(|p| p.text_at(&["relativePath"]).unwrap_or_else(|| "../pom.xml".to_owned())),
        })
    }
}

/// A plugin as the chain of POMs configures it: the nearest declaration first.
struct PluginView<'a> {
    plugins: Vec<&'a Plugin>,
}

impl PluginView<'_> {
    fn version(&self) -> Option<&str> {
        self.plugins.iter().find_map(|p| p.version.as_deref())
    }

    fn has_goal(&self, goal: &str) -> bool {
        self.plugins.iter().any(|p| p.goals.iter().any(|g| g == goal))
    }

    fn configs(&self) -> impl Iterator<Item = &Xml> {
        self.plugins.iter().flat_map(|p| p.configs.iter())
    }

    /// The text at `path` of the nearest configuration that has it.
    fn value(&self, path: &[&str]) -> Option<String> {
        self.configs().find_map(|c| c.text_at(path))
    }

    /// The texts of the `item` elements under `path` in the nearest configuration that has any.
    fn values(&self, path: &[&str], item: &str) -> Vec<String> {
        self.configs()
            .map(|c| {
                c.at(path).map(|n| n.named(item).map(|i| i.text.clone()).filter(|t| !t.is_empty()).collect::<Vec<_>>()).unwrap_or_default()
            })
            .find(|values| !values.is_empty())
            .unwrap_or_default()
    }
}

fn find_plugin<'a>(chain: &'a [Extras], artifact: &str) -> Option<PluginView<'a>> {
    let plugins: Vec<&Plugin> =
        chain.iter().flat_map(|e| e.plugins.iter().chain(e.managed_plugins.iter())).filter(|p| p.artifact == artifact).collect();
    (!plugins.is_empty()).then_some(PluginView { plugins })
}

fn coordinates(group: &str, artifact: &str, version: &str) -> String {
    format!("{group}:{artifact}:{version}")
}

/// The POMs of the project and of its parents: the project's own files first, then the repositories.
struct Poms<'a> {
    config: &'a Config,
    texts: RefCell<HashMap<String, String>>,
    /// The coordinates of the POMs that are files of the project.
    local: RefCell<HashSet<String>>,
    repositories: Vec<MavenRepository>,
    client: OnceCell<MavenClient>,
}

impl<'a> Poms<'a> {
    fn new(config: &'a Config, repositories: Vec<MavenRepository>) -> Self {
        Self { config, texts: RefCell::new(HashMap::new()), local: RefCell::new(HashSet::new()), repositories, client: OnceCell::new() }
    }

    /// Makes a POM file of the project available under its coordinates.
    fn register(&self, xml: String) -> Result<Pom> {
        let pom = Pom::parse(&xml)?;
        if let (Some(group), Some(version)) = (pom.effective_group(), pom.effective_version()) {
            let key = coordinates(group, &pom.artifact, version);
            self.local.borrow_mut().insert(key.clone());
            self.texts.borrow_mut().insert(key, xml);
        }
        Ok(pom)
    }

    fn registered(&self, module: &ModuleId, version: &str) -> bool {
        self.texts.borrow().contains_key(&coordinates(&module.group, &module.artifact, version))
    }

    fn xml(&self, module: &ModuleId, version: &str) -> Result<String> {
        let key = coordinates(&module.group, &module.artifact, version);
        if let Some(xml) = self.texts.borrow().get(&key) {
            return Ok(xml.clone());
        }
        let client = self.client.get_or_init(|| MavenClient::new(self.config, self.repositories.clone()));
        let artifact = Artifact::new(module.clone(), version).with_extension("pom");
        let path = client.artifact_path(&artifact)?;
        let Some(xml) = client.fetch_text(&path)? else { bail!("{module}:{version} is not in the Maven repositories") };
        self.texts.borrow_mut().insert(key, xml.clone());
        Ok(xml)
    }
}

impl PomSource for Poms<'_> {
    fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom> {
        Pom::parse(&self.xml(module, version)?)
    }
}

/// The POMs as they are when the parent of one project cannot be read: that project, without its parent.
struct Detached<'a, 'b> {
    poms: &'b Poms<'a>,
    module: ModuleId,
    version: String,
}

impl PomSource for Detached<'_, '_> {
    fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom> {
        let mut pom = self.poms.pom(module, version)?;
        if *module == self.module && version == self.version {
            pom.parent = None;
        }
        Ok(pom)
    }
}

/// A module of the project.
struct Module {
    /// Relative to the project root.
    dir: PathBuf,
    pom: Pom,
    extras: Extras,
}

/// Resolves `.` and `..` in a relative path without touching the disk.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Resolves `.` and `..` in a path relative to the import root, keeping leading
/// `..` that escape it (e.g. `../core` for a sibling of a child-module import).
fn clean_relative(path: &Path) -> PathBuf {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(last) if last != "..") {
                    parts.pop();
                } else {
                    parts.push("..".to_owned());
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => return path.to_path_buf(),
        }
    }
    parts.iter().collect()
}

/// Sibling modules listed in a parent POM on disk that is not part of the
/// imported project: when `kargo import` runs in a child module, a dependency
/// on a sibling looks like a Maven coordinate, but it is really a path
/// dependency on a directory outside the import root (e.g. `../core`).
fn external_siblings(root: &Path, modules: &[Module]) -> HashMap<String, LocalPackage> {
    let mut out: HashMap<String, LocalPackage> = HashMap::new();
    let known_poms: Vec<PathBuf> = modules.iter().map(|m| dunce_canonicalize(&root.join(&m.dir).join("pom.xml"))).collect();
    let known_dirs: Vec<PathBuf> = modules.iter().map(|m| dunce_canonicalize(&root.join(&m.dir))).collect();
    for module in modules {
        let relative = module.extras.relative_path.as_deref().unwrap_or("../pom.xml");
        let mut parent_file = root.join(&module.dir).join(relative);
        if parent_file.is_dir() {
            parent_file.push("pom.xml");
        }
        if !parent_file.is_file() {
            continue;
        }
        // The parent is already imported (a workspace import); its modules are known.
        if known_poms.iter().any(|p| *p == dunce_canonicalize(&parent_file)) {
            continue;
        }
        let Ok(parent_xml) = std::fs::read_to_string(&parent_file) else { continue };
        let Ok(parent_extras) = Extras::parse(&parent_xml) else { continue };
        if parent_extras.modules.is_empty() {
            continue;
        }
        let parent_file_rel = clean_relative(&module.dir.join(relative));
        let Some(parent_dir_rel) = parent_file_rel.parent().map(Path::to_path_buf) else { continue };
        let Some(parent_dir_abs) = parent_file.parent().map(Path::to_path_buf) else { continue };
        for child in &parent_extras.modules {
            let child_path = Path::new(child);
            let child_path =
                if child_path.extension().is_some_and(|e| e == "xml") { child_path.parent().unwrap_or(Path::new("")) } else { child_path };
            let sibling_pom_abs = parent_dir_abs.join(child_path).join("pom.xml");
            if !sibling_pom_abs.is_file() {
                continue;
            }
            // The sibling is already imported; it is not external.
            if known_dirs.iter().any(|d| *d == dunce_canonicalize(&parent_dir_abs.join(child_path))) {
                continue;
            }
            let Ok(sibling_xml) = std::fs::read_to_string(&sibling_pom_abs) else { continue };
            let Ok(sibling_pom) = Pom::parse(&sibling_xml) else { continue };
            let Some(group) = sibling_pom.effective_group() else { continue };
            if group.contains("${") {
                continue;
            }
            let sibling_dir_rel = clean_relative(&parent_dir_rel.join(child_path));
            let (name, _) = package_name(&sibling_pom.artifact);
            out.entry(format!("{group}:{}", sibling_pom.artifact)).or_insert(LocalPackage { name, dir: sibling_dir_rel });
        }
    }
    out
}

/// Reads `<dir>/pom.xml` and, from its `<modules>`, every module below it.
fn load_modules(root: &Path, dir: &Path, modules: &mut Vec<Module>, seen: &mut HashSet<PathBuf>, imported: &mut Imported) -> Result<()> {
    if !seen.insert(dir.to_path_buf()) {
        return Ok(());
    }
    let file = root.join(dir).join("pom.xml");
    let xml = std::fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
    let pom = Pom::parse(&xml).with_context(|| format!("failed to parse {}", file.display()))?;
    let extras = Extras::parse(&xml).with_context(|| format!("failed to parse {}", file.display()))?;
    let children = extras.modules.clone();
    let location = if dir.as_os_str().is_empty() { "pom.xml".to_owned() } else { format!("{}/pom.xml", dir.display()).replace('\\', "/") };
    if pom.effective_group().is_none() {
        bail!("{location} has no <groupId>, and neither has its parent");
    }
    if pom.effective_version().is_none() {
        bail!("{location} has no <version>, and neither has its parent");
    }
    modules.push(Module { dir: dir.to_path_buf(), pom, extras });
    for child in children {
        let path = Path::new(&child);
        let path = if path.extension().is_some_and(|e| e == "xml") { path.parent().unwrap_or(Path::new("")) } else { path };
        match normalize(&dir.join(path)) {
            Some(sub) if root.join(&sub).join("pom.xml").is_file() => load_modules(root, &sub, modules, seen, imported)?,
            Some(sub) => imported.manual(format!("the module `{child}` has no pom.xml in {}; it was left out", sub.display()), MIGRATION),
            None => imported.manual(format!("the module `{child}` is outside the project; it was left out"), MIGRATION),
        }
    }
    Ok(())
}

pub(super) fn read(config: &Config, root: &Path) -> Result<Imported> {
    let mut imported = Imported::default();
    let mut modules = Vec::new();
    load_modules(root, Path::new(""), &mut modules, &mut HashSet::new(), &mut imported)?;

    let mut repositories = vec![MavenRepository::maven_central()];
    for module in &modules {
        for (id, url) in &module.extras.repositories {
            if !repositories.iter().any(|r| r.url == url.trim_end_matches('/')) {
                repositories.push(MavenRepository::new(id.clone(), url.clone()));
            }
        }
    }
    let poms = Poms::new(config, repositories);
    for module in &modules {
        let xml = std::fs::read_to_string(root.join(&module.dir).join("pom.xml"))?;
        poms.register(xml)?;
    }
    // A parent in the file system that no `<modules>` lists.
    for module in &modules {
        let Some(parent) = &module.pom.parent else { continue };
        if poms.registered(&ModuleId::new(&parent.group, &parent.artifact), &parent.version) {
            continue;
        }
        let relative = module.extras.relative_path.as_deref().unwrap_or("../pom.xml");
        let mut file = root.join(&module.dir).join(relative);
        if file.is_dir() {
            file.push("pom.xml");
        }
        if let Ok(xml) = std::fs::read_to_string(&file) {
            let _ = poms.register(xml);
        }
    }

    let root_module = &modules[0];
    let root_packaging = root_module.pom.packaging.as_deref().unwrap_or("jar");
    let workspace = !root_module.extras.modules.is_empty();
    if root_packaging == "pom" && !workspace {
        bail!("pom.xml has packaging `pom` and no <modules>, so there is no code to build; Kargo imports projects that build a jar");
    }
    if workspace && root_packaging != "pom" {
        imported.manual("the root pom.xml has modules and builds a jar itself; Kargo's workspace root has no package, so the sources of the root were not imported", MIGRATION);
    }

    // The buildable modules, and the names the modules that depend on each other use.
    let buildable: Vec<&Module> = modules
        .iter()
        .filter(|m| m.pom.packaging.as_deref().unwrap_or("jar") != "pom")
        .filter(|m| !(workspace && m.dir.as_os_str().is_empty()))
        .collect();
    if buildable.is_empty() {
        bail!("none of the modules builds a jar; Kargo imports projects that build a jar");
    }
    let mut siblings: HashMap<String, LocalPackage> = HashMap::new();
    for module in &buildable {
        let group = module.pom.effective_group().unwrap_or_default();
        let (name, _) = package_name(&module.pom.artifact);
        siblings.insert(format!("{group}:{}", module.pom.artifact), LocalPackage { name, dir: module.dir.clone() });
    }
    // A child-module import: dependencies on siblings listed in a parent POM
    // outside the project are path dependencies, not Maven coordinates.
    let mut outside: HashSet<String> = HashSet::new();
    for (coords, local) in external_siblings(root, &modules) {
        if let std::collections::hash_map::Entry::Vacant(entry) = siblings.entry(coords.clone()) {
            outside.insert(coords);
            entry.insert(local);
        }
    }

    imported.workspace = workspace;
    for module in buildable {
        let package = package(&poms, module, &siblings, &outside, &mut imported)?;
        imported.packages.push(package);
    }
    let names: Vec<_> = imported.packages.iter().map(|p| p.name.clone()).collect();
    if workspace {
        imported.translated(format!("<modules> -> workspace members ({})", names.join(", ")));
    }
    Ok(imported)
}

/// A Kargo package name for a Maven artifact id.
fn package_name(artifact: &str) -> (String, bool) {
    if validate_package_name(artifact).is_ok() {
        return (artifact.to_owned(), false);
    }
    let mut name: String = artifact.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        name.insert_str(0, "pkg-");
    }
    (name, true)
}

/// The nearest-first extras of the POMs in `chain`, which starts with the POM of `module:version`.
fn chain_extras(poms: &Poms, module: &Module, chain: &[Pom]) -> Vec<Extras> {
    let mut out = vec![module.extras.clone()];
    for pom in &chain[..chain.len().saturating_sub(1)] {
        let Some(parent) = &pom.parent else { break };
        let extras = poms.xml(&ModuleId::new(&parent.group, &parent.artifact), &parent.version).and_then(|xml| Extras::parse(&xml));
        out.push(extras.unwrap_or_default());
    }
    out
}

struct Resolved {
    chain: Vec<Pom>,
    /// How many POMs at the start of `chain` are files of the project (the rest come from repositories).
    local: usize,
    extras: Vec<Extras>,
    props: BTreeMap<String, String>,
    effective: Option<EffectivePom>,
}

fn resolve(poms: &Poms, module: &Module, imported: &mut Imported, name: &str) -> Result<Resolved> {
    let group = module.pom.effective_group().unwrap_or_default().to_owned();
    let version = module.pom.effective_version().unwrap_or_default().to_owned();
    let id = ModuleId::new(&group, &module.pom.artifact);

    let load = |source: &dyn PomSource| {
        pom_chain(source, &id, &version).and_then(|chain| effective_properties(source, &id, &version).map(|props| (chain, props)))
    };
    let detached = Detached { poms, module: id.clone(), version: version.clone() };
    let (source, chain, props): (&dyn PomSource, Vec<Pom>, BTreeMap<String, String>) = match load(poms) {
        Ok((chain, props)) => (poms, chain, props),
        Err(e) => {
            imported.manual(
                format!(
                    "the parent POM of `{name}` could not be read ({e:#}), so the properties, versions and plugins it defines are missing; make it reachable (check your connection or `[repositories]`) and import again"
                ),
                MIGRATION,
            );
            let (chain, props) = load(&detached)?;
            (&detached, chain, props)
        }
    };
    let extras = chain_extras(poms, module, &chain);
    let effective = match effective_pom_without_imports(source, &id, &version) {
        Ok(effective) => Some(effective),
        Err(e) => {
            imported.manual(
                format!("the managed versions of `{name}` could not be resolved ({e:#}); dependencies without a version keep none"),
                MIGRATION,
            );
            None
        }
    };
    let mut local = 0;
    let mut key = coordinates(&group, &module.pom.artifact, &version);
    for pom in &chain {
        if !poms.local.borrow().contains(&key) {
            break;
        }
        local += 1;
        match &pom.parent {
            Some(parent) => key = coordinates(&parent.group, &parent.artifact, &parent.version),
            None => break,
        }
    }
    Ok(Resolved { chain, local, extras, props, effective })
}

fn package(
    poms: &Poms,
    module: &Module,
    siblings: &HashMap<String, LocalPackage>,
    outside: &HashSet<String>,
    imported: &mut Imported,
) -> Result<Package> {
    let (name, renamed) = package_name(&module.pom.artifact);
    if renamed {
        imported.manual(
            format!("the artifact id `{}` is not a valid package name; the package is called `{name}`", module.pom.artifact),
            MIGRATION,
        );
    }
    let resolved = resolve(poms, module, imported, &name)?;
    let interp = |s: &str| interpolate(s, &resolved.props);
    let property = |key: &str| resolved.props.get(key).map(|v| interp(v)).filter(|v| !v.contains("${") && !v.is_empty());
    let extras = &resolved.extras;
    let own = &extras[0];

    let mut package = Package { dir: module.dir.clone(), name: name.clone(), ..Package::default() };
    let packaging = module.pom.packaging.as_deref().unwrap_or("jar");
    if !matches!(packaging, "jar" | "bundle") {
        imported.manual(format!("`{name}` has packaging `{packaging}`; Kargo builds it as a jar"), MIGRATION);
    }

    // Coordinates and metadata.
    package.version = module.pom.effective_version().map(&interp).filter(|v| !v.contains("${")).unwrap_or_else(|| {
        imported.manual(format!("the version of `{name}` could not be resolved; it is 0.1.0"), MIGRATION);
        "0.1.0".to_owned()
    });
    package.group = module.pom.effective_group().map(&interp).filter(|g| !g.contains("${"));
    package.description = own.description.as_deref().map(&interp);
    package.homepage = own.url.as_deref().map(&interp).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    package.repository = own.scm_url.as_deref().map(&interp).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    package.license = own.licenses.iter().find_map(|l| spdx(l));
    if !own.licenses.is_empty() && package.license.is_none() {
        imported.manual(
            format!("the license of `{name}` is not one Kargo knows by its SPDX id; set `license` in kargo.toml"),
            "reference/manifest/",
        );
    }
    package.authors = own.developers.iter().map(|(n, e)| e.as_ref().map(|e| format!("{n} <{e}>")).unwrap_or_else(|| n.clone())).collect();

    // The Kotlin compiler plugin of Maven carries the Kotlin settings.
    let kotlin_plugin = find_plugin(extras, "kotlin-maven-plugin");
    let raw_dependencies = &module.pom.dependencies;
    let mut kotlin = KotlinSettings::default();
    if let Some(plugin) = &kotlin_plugin {
        kotlin.version = plugin.version().map(&interp).filter(|v| !v.contains("${"));
        kotlin.language_version = plugin.value(&["languageVersion"]).map(|v| interp(&v));
        kotlin.api_version = plugin.value(&["apiVersion"]).map(|v| interp(&v));
        for arg in plugin.values(&["args"], "arg").iter().map(|a| interp(a)) {
            if let Some(opt_in) = arg.strip_prefix("-opt-in=") {
                kotlin.opt_in.push(opt_in.to_owned());
            } else if arg == "-Werror" {
                kotlin.warnings_as_errors = true;
            } else {
                kotlin.compiler_args.push(arg);
            }
        }
        if plugin.has_goal("kapt") {
            imported.manual(
                format!("`{name}` runs kapt; Kargo supports KSP processors (`scope = \"ksp\"`) and Java annotation processors, not kapt"),
                "guides/dependencies/#kotlin-symbol-processing-ksp",
            );
        }
    } else {
        imported.manual(format!("`{name}` does not use kotlin-maven-plugin; check that the Kotlin version is right"), MIGRATION);
    }
    kotlin.version = kotlin.version.or_else(|| {
        resolved
            .effective
            .iter()
            .flat_map(|e| e.dependencies.iter())
            .chain(raw_dependencies)
            .find(|d| d.group == "org.jetbrains.kotlin" && d.artifact.starts_with("kotlin-stdlib"))
            .and_then(|d| d.version.as_deref().map(&interp))
            .filter(|v| !v.contains("${"))
            .or_else(|| property("kotlin.version"))
    });
    if kotlin.version.is_none() {
        imported
            .manual(format!("the Kotlin version of `{name}` was not found; Kargo uses {DEFAULT_KOTLIN_VERSION}"), "reference/manifest/");
    }

    // The JVM target.
    let compiler = find_plugin(extras, "maven-compiler-plugin");
    let jvm_target = plugin_value(&kotlin_plugin, &["jvmTarget"])
        .map(|v| interp(&v))
        .or_else(|| property("kotlin.compiler.jvmTarget"))
        .or_else(|| property("maven.compiler.release"))
        .or_else(|| plugin_value(&compiler, &["release"]).map(|v| interp(&v)))
        .or_else(|| property("java.version"))
        .or_else(|| property("maven.compiler.target"))
        .or_else(|| plugin_value(&compiler, &["target"]).map(|v| interp(&v)))
        .or_else(|| property("maven.compiler.source"));
    package.jvm_target = jvm_target.and_then(|t| match java_target(&t) {
        Some(v) => Some(v),
        None => {
            imported.manual(
                format!("the Java version `{t}` of `{name}` is not one Kargo knows; set `jvm-target` in kargo.toml"),
                "reference/manifest/",
            );
            None
        }
    });
    if let Some(compiler) = &compiler {
        package.javac_args = compiler.values(&["compilerArgs"], "arg").iter().map(|a| interp(a)).collect();
        if compiler.value(&["parameters"]).is_some_and(|p| p == "true") && !package.javac_args.iter().any(|a| a == "-parameters") {
            package.javac_args.push("-parameters".to_owned());
        }
    }

    // Dependencies.
    let (mut dependencies, mut dev_dependencies, bom_imports) = dependencies(&resolved, &interp, siblings, outside, &package.dir, imported);
    dependencies.extend(bom_imports);
    if let Some(compiler) = &compiler {
        for path in compiler.configs().flat_map(|c| c.at(&["annotationProcessorPaths"]).into_iter().flat_map(|p| p.named("path"))) {
            let (Some(group), Some(artifact)) = (path.text_at(&["groupId"]), path.text_at(&["artifactId"])) else { continue };
            let mut dep = Dependency::new(interp(&group), interp(&artifact), path.text_at(&["version"]).map(|v| interp(&v)));
            dep.scope = Scope::Processor;
            dependencies.push(dep);
        }
    }

    // Frameworks.
    let spring_parent = resolved
        .chain
        .iter()
        .find(|p| p.effective_group() == Some("org.springframework.boot") && p.artifact == "spring-boot-starter-parent");
    let spring_version = spring_parent.and_then(|p| p.effective_version().map(str::to_owned));
    let plugin_names: Vec<String> = kotlin_plugin.iter().flat_map(|p| p.values(&["compilerPlugins"], "plugin")).collect();
    let hints = Hints {
        spring_boot: (spring_parent.is_some() || find_plugin(extras, "spring-boot-maven-plugin").is_some()).then_some(spring_version),
        jpa: plugin_names.iter().any(|p| p == "jpa"),
        serialization_plugin: plugin_names.iter().any(|p| p == "kotlinx-serialization"),
        lombok: plugin_names.iter().any(|p| p == "lombok"),
        ..Hints::default()
    };
    let detected = frameworks::detect(&mut dependencies, &mut dev_dependencies, &hints);
    for line in detected.translated {
        imported.translated(format!("{name}: {line}"));
    }
    let spring = detected.frameworks.spring_boot.is_some();
    package.frameworks = detected.frameworks;
    dependencies.retain(|d| !frameworks::is_built_in(d));
    dev_dependencies.retain(|d| !frameworks::is_built_in(d));
    if spring {
        kotlin.compiler_args.retain(|a| a != "-Xjsr305=strict" && a != "-java-parameters");
        package.javac_args.retain(|a| a != "-parameters");
    }

    // Kotlin compiler plugins.
    if let Some(plugin) = &kotlin_plugin {
        let options = plugin.values(&["pluginOptions"], "option");
        let kotlin_version = kotlin.version.clone().unwrap_or_else(|| DEFAULT_KOTLIN_VERSION.to_owned());
        for plugin_name in &plugin_names {
            match compiler_plugins(plugin_name, &options, &kotlin_version, spring) {
                Some(found) => package.plugins.extend(found),
                None => imported.manual(
                    format!("`{name}` uses the Kotlin compiler plugin `{plugin_name}`, which Kargo has no setting for; add it as a `[plugins.<name>]` entry"),
                    "guides/tasks-and-plugins/#compiler-plugins",
                ),
            }
        }
        dedupe_plugin_names(&mut package.plugins);
    }

    // The main class.
    package.main_class = ["exec-maven-plugin", "spring-boot-maven-plugin"]
        .iter()
        .find_map(|p| plugin_value(&find_plugin(extras, p), &["mainClass"]))
        .or_else(|| plugin_value(&find_plugin(extras, "maven-jar-plugin"), &["archive", "manifest", "mainClass"]))
        .or_else(|| plugin_value(&find_plugin(extras, "maven-shade-plugin"), &["transformers", "transformer", "mainClass"]))
        .map(|c| interp(&c))
        .or_else(|| property("start-class"))
        .or_else(|| property("exec.mainClass"))
        .filter(|c| !c.contains("${") && !c.is_empty());

    // Repositories.
    let mut seen = HashSet::new();
    for (id, url) in extras.iter().flat_map(|e| e.repositories.iter()) {
        let url = interp(url);
        if is_central(&url) || !seen.insert(url.clone()) {
            continue;
        }
        let repo_name: String =
            interp(id).chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        package.repositories.push(Repository { name: repo_name, url });
    }
    if !package.repositories.is_empty() {
        imported.translated(format!(
            "{name}: <repositories> -> [repositories] ({})",
            package.repositories.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
        ));
        imported.manual(
            format!("credentials for the repositories of `{name}` live in ~/.m2/settings.xml; name environment variables in `username-env` and `password-env`"),
            "guides/dependencies/#private-repositories",
        );
    }

    package.dependencies = dependencies;
    package.dev_dependencies = dev_dependencies;
    package.kotlin = kotlin;
    package.moves = source_moves(&package.dir, extras, &kotlin_plugin, &interp);
    let count = package.dependencies.len() + package.dev_dependencies.len();
    imported.translated(format!("{name}: {count} dependencies, Kotlin {}", package.kotlin.version.as_deref().unwrap_or("(default)")));
    Ok(package)
}

fn plugin_value(plugin: &Option<PluginView<'_>>, path: &[&str]) -> Option<String> {
    plugin.as_ref().and_then(|p| p.value(path))
}

fn is_central(url: &str) -> bool {
    let url = url.trim_end_matches('/');
    url.contains("repo.maven.apache.org/maven2") || url.contains("repo1.maven.org/maven2")
}

/// A Java version in `jvm-target`'s spelling: `1.8` and `8` stay, `11` stays, anything else is not one.
pub(super) fn java_target(text: &str) -> Option<String> {
    let text = text.trim();
    let number = text.strip_prefix("1.").filter(|n| *n == "8").unwrap_or(text);
    let value: u32 = number.parse().ok()?;
    (value >= 8).then(|| number.to_owned())
}

/// The SPDX id of a license from its POM name and URL, for the common ones.
fn spdx(license: &str) -> Option<String> {
    let lower = license.to_ascii_lowercase();
    let id = if lower.contains("apache") && (lower.contains("2.0") || lower.contains("license-2")) {
        "Apache-2.0"
    } else if lower.contains("mit license") || lower.contains("opensource.org/licenses/mit") || lower.trim_start().starts_with("mit ") {
        "MIT"
    } else if lower.contains("bsd") && lower.contains('3') {
        "BSD-3-Clause"
    } else if lower.contains("bsd") && lower.contains('2') {
        "BSD-2-Clause"
    } else if lower.contains("eclipse public license") && lower.contains("2.0") {
        "EPL-2.0"
    } else if lower.contains("eclipse public license") {
        "EPL-1.0"
    } else if lower.contains("mozilla public license") && lower.contains("2.0") {
        "MPL-2.0"
    } else if lower.contains("lgpl") || lower.contains("lesser general public") {
        "LGPL-3.0-only"
    } else {
        return None;
    };
    Some(id.to_owned())
}

/// The dependencies of the POM, split into the ones for the main code and for the tests, and its BOM imports.
fn dependencies(
    resolved: &Resolved,
    interp: &dyn Fn(&str) -> String,
    siblings: &HashMap<String, LocalPackage>,
    outside: &HashSet<String>,
    dir: &Path,
    imported: &mut Imported,
) -> (Vec<Dependency>, Vec<Dependency>, Vec<Dependency>) {
    let key =
        |d: &PomDependency| (d.group.clone(), d.artifact.clone(), d.classifier.clone(), d.kind.clone().unwrap_or_else(|| "jar".to_owned()));
    // What the project and its parents write, with the properties filled in.
    let fill = |d: &PomDependency| PomDependency {
        group: interp(&d.group),
        artifact: interp(&d.artifact),
        version: d.version.as_deref().map(interp),
        scope: d.scope.as_deref().map(interp),
        classifier: d.classifier.as_deref().map(interp),
        kind: d.kind.as_deref().map(interp),
        optional: d.optional,
        exclusions: d.exclusions.iter().map(|(g, a)| (interp(g), interp(a))).collect(),
    };
    let declared: Vec<PomDependency> = resolved.chain.iter().flat_map(|p| p.dependencies.iter()).map(fill).collect();
    let managed: Vec<PomDependency> = resolved.chain.iter().flat_map(|p| p.dependency_management.iter()).map(fill).collect();
    // The BOMs the project imports. The ones a parent from a repository imports for its own management are not the project's.
    let own_management: Vec<PomDependency> =
        resolved.chain[..resolved.local].iter().flat_map(|p| p.dependency_management.iter()).map(fill).collect();
    let mut boms: Vec<Dependency> = Vec::new();
    for bom in own_management.iter().filter(|d| d.is_bom_import()) {
        if !boms.iter().any(|b| b.group == bom.group && b.artifact == bom.artifact) {
            let mut dep = Dependency::new(&bom.group, &bom.artifact, bom.version.clone());
            dep.bom = true;
            boms.push(dep);
        }
    }

    // The effective POM has the versions the build uses; without it the declared dependencies are all there is.
    let effective: Vec<PomDependency> = match &resolved.effective {
        Some(e) => e.dependencies.clone(),
        None => {
            let mut all: Vec<PomDependency> = Vec::new();
            for dep in &declared {
                if !all.iter().any(|d| key(d) == key(dep)) {
                    all.push(dep.clone());
                }
            }
            all
        }
    };

    let mut main = Vec::new();
    let mut dev = Vec::new();
    let mut warned: HashSet<String> = HashSet::new();
    for dep in effective {
        let scope = dep.scope.as_deref().unwrap_or("compile");
        let coords = format!("{}:{}", dep.group, dep.artifact);
        let declared_version = declared.iter().find(|d| key(d) == key(&dep)).and_then(|d| d.version.clone());
        let managed_version = managed.iter().find(|d| !d.is_bom_import() && key(d) == key(&dep)).and_then(|d| d.version.clone());
        let version = declared_version.or(managed_version).or_else(|| if boms.is_empty() { dep.version.clone() } else { None });

        if scope == "system" {
            imported.manual(
                format!("{coords} has scope `system` (a jar on disk); add the jar to a repository or use a `maven-local` install"),
                MIGRATION,
            );
            continue;
        }
        if dep.kind.as_deref() == Some("pom") {
            imported.manual(format!("{coords} is a `pom` type dependency; use a BOM (`bom = true`) or list its dependencies"), MIGRATION);
            continue;
        }

        if dep.kind.as_deref() == Some("test-jar") && siblings.contains_key(&coords) {
            imported.manual(
                format!("{coords} is used through its test-jar; Kargo shares test code between packages through `test-fixtures/`"),
                "guides/workspaces/#test-fixtures",
            );
            continue;
        }
        let mut out = Dependency::new(&dep.group, &dep.artifact, version);
        out.classifier = dep.classifier.clone();
        match dep.kind.as_deref() {
            None | Some("jar" | "bundle" | "maven-plugin" | "ejb") => {}
            Some("test-jar") => out.classifier = out.classifier.or_else(|| Some("tests".to_owned())),
            Some(other) => out.extension = Some(other.to_owned()),
        }
        out.exclude = dep.exclusions.iter().map(|(g, a)| format!("{g}:{a}")).collect();
        out.scope = match scope {
            "runtime" => Scope::Runtime,
            "provided" => Scope::Provided,
            _ => Scope::Compile,
        };
        if let Some(local) = siblings.get(&coords) {
            out.version = None;
            out.path = Some(LocalPackage { name: local.name.clone(), dir: relative_dir(dir, &local.dir) });
            out.classifier = None;
            out.extension = None;
            out.exclude.clear();
            if outside.contains(&coords) && warned.insert(coords.clone()) {
                let path = out.path.as_ref().map(|l| l.dir.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                imported.manual(
                    format!(
                        "{coords} looks like the sibling module `{path}` listed in the parent POM; it became a path dependency, so import the parent as a workspace or make sure `{path}` has a kargo.toml"
                    ),
                    MIGRATION,
                );
            }
        }
        if scope == "test" {
            out.scope = Scope::Compile;
            dev.push(out);
        } else {
            main.push(out);
        }
    }
    (main, dev, boms)
}

/// The Kargo compiler plugins for one `<compilerPlugins><plugin>` of kotlin-maven-plugin; `None`
/// when Kargo has no equivalent. The plugin options are `all-open:annotation=...` strings.
fn compiler_plugins(name: &str, options: &[String], kotlin_version: &str, spring: bool) -> Option<Vec<CompilerPlugin>> {
    let option_values = |prefix: &str| -> Vec<(String, String)> {
        options
            .iter()
            .filter_map(|o| o.strip_prefix(prefix))
            .filter_map(|o| o.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect()
    };
    let allopen = |options: Vec<(String, String)>| -> Vec<CompilerPlugin> {
        options
            .into_iter()
            .map(|option| CompilerPlugin {
                name: "allopen".to_owned(),
                maven: format!("org.jetbrains.kotlin:kotlin-allopen-compiler-plugin-embeddable:{kotlin_version}"),
                id: "org.jetbrains.kotlin.allopen".to_owned(),
                options: vec![option],
            })
            .collect()
    };
    let noarg = |options: Vec<(String, String)>| -> Vec<CompilerPlugin> {
        options
            .into_iter()
            .map(|option| CompilerPlugin {
                name: "noarg".to_owned(),
                maven: format!("org.jetbrains.kotlin:kotlin-noarg-compiler-plugin-embeddable:{kotlin_version}"),
                id: "org.jetbrains.kotlin.noarg".to_owned(),
                options: vec![option],
            })
            .collect()
    };
    match name {
        // Spring Boot's framework sets these up.
        "spring" if spring => Some(Vec::new()),
        "jpa" if spring => Some(Vec::new()),
        "spring" => Some(allopen(vec![("preset".to_owned(), "spring".to_owned())])),
        "jpa" => Some(noarg(vec![("preset".to_owned(), "jpa".to_owned())])),
        "all-open" => Some(allopen(option_values("all-open:"))),
        "no-arg" => Some(noarg(option_values("no-arg:"))),
        "kotlinx-serialization" | "lombok" => Some(Vec::new()),
        _ => None,
    }
}

/// Gives plugins that share a name distinct ones (`allopen`, `allopen-2`); the manifest merges them.
fn dedupe_plugin_names(plugins: &mut [CompilerPlugin]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for plugin in plugins {
        let count = counts.entry(plugin.name.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            plugin.name = format!("{}-{count}", plugin.name);
        }
    }
}

/// Where the sources of a Maven module move to.
fn source_moves(dir: &Path, extras: &[Extras], kotlin: &Option<PluginView<'_>>, interp: &dyn Fn(&str) -> String) -> Vec<DirMove> {
    let own = &extras[0];
    let path = |text: &str| -> Option<PathBuf> {
        let text = interp(text).replace("${project.basedir}", ".").replace("${basedir}", ".");
        if text.contains("${") {
            return None;
        }
        let relative = normalize(Path::new(&text))?;
        Some(dir.join(relative))
    };
    let mut moves: Vec<DirMove> = Vec::new();
    let mut add = |candidates: Vec<Option<PathBuf>>, to: &str| {
        for from in candidates.into_iter().flatten() {
            let to = dir.join(to);
            if from != to && !moves.iter().any(|m| m.from == from) {
                moves.push(DirMove { from, to });
            }
        }
    };
    let list = |paths: Vec<String>| -> Vec<Option<PathBuf>> { paths.iter().map(|p| path(p)).collect() };

    let mut main = vec![own.source_directory.as_deref().and_then(path)];
    main.extend(list(kotlin.iter().flat_map(|k| k.values(&["sourceDirs"], "sourceDir")).collect()));
    main.push(path("src/main/kotlin"));
    main.push(path("src/main/java"));
    add(main, "src");

    let mut test = vec![own.test_source_directory.as_deref().and_then(path)];
    test.extend(list(kotlin.iter().flat_map(|k| k.values(&["testSourceDirs"], "testSourceDir")).collect()));
    test.push(path("src/test/kotlin"));
    test.push(path("src/test/java"));
    add(test, "tests");

    let mut resources = list(own.resources.clone());
    resources.push(path("src/main/resources"));
    add(resources, "resources");
    let mut test_resources = list(own.test_resources.clone());
    test_resources.push(path("src/test/resources"));
    add(test_resources, "test-resources");
    moves
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(home: &Path) -> Config {
        Config::new(home.to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true)
    }

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    const BILLING: &str = r#"<project>
  <groupId>com.example</groupId>
  <artifactId>billing</artifactId>
  <version>2.0.0</version>
  <description>Bills people</description>
  <properties>
    <kotlin.version>2.4.21</kotlin.version>
    <maven.compiler.release>17</maven.compiler.release>
  </properties>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.fasterxml.jackson</groupId>
        <artifactId>jackson-bom</artifactId>
        <version>2.19.0</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-stdlib</artifactId><version>${kotlin.version}</version></dependency>
    <dependency><groupId>com.fasterxml.jackson.module</groupId><artifactId>jackson-module-kotlin</artifactId></dependency>
    <dependency><groupId>org.postgresql</groupId><artifactId>postgresql</artifactId><version>42.7.5</version><scope>runtime</scope></dependency>
    <dependency><groupId>jakarta.servlet</groupId><artifactId>jakarta.servlet-api</artifactId><version>6.1.0</version><scope>provided</scope></dependency>
    <dependency>
      <groupId>com.example</groupId><artifactId>legacy-client</artifactId><version>1.4</version>
      <exclusions><exclusion><groupId>commons-logging</groupId><artifactId>commons-logging</artifactId></exclusion></exclusions>
    </dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-test-junit5</artifactId><version>${kotlin.version}</version><scope>test</scope></dependency>
    <dependency><groupId>io.mockk</groupId><artifactId>mockk-jvm</artifactId><version>1.13.12</version><scope>test</scope></dependency>
  </dependencies>
  <repositories>
    <repository><id>company</id><url>https://repo.example.com/maven</url></repository>
    <repository><id>central</id><url>https://repo.maven.apache.org/maven2</url></repository>
  </repositories>
  <build>
    <plugins>
      <plugin>
        <groupId>org.jetbrains.kotlin</groupId>
        <artifactId>kotlin-maven-plugin</artifactId>
        <version>${kotlin.version}</version>
        <configuration>
          <args><arg>-Xcontext-parameters</arg><arg>-opt-in=kotlin.ExperimentalStdlibApi</arg></args>
        </configuration>
      </plugin>
      <plugin>
        <artifactId>maven-jar-plugin</artifactId>
        <configuration><archive><manifest><mainClass>com.example.billing.MainKt</mainClass></manifest></archive></configuration>
      </plugin>
    </plugins>
  </build>
</project>"#;

    #[test]
    fn translates_the_example_of_the_migration_guide() {
        let home = tempfile::tempdir().unwrap();
        let dir = project(&[
            ("pom.xml", BILLING),
            ("src/main/kotlin/App.kt", ""),
            ("src/test/kotlin/AppTest.kt", ""),
            ("src/main/resources/a.txt", ""),
        ]);
        let imported = read(&config(home.path()), dir.path()).unwrap();
        assert!(!imported.workspace);
        let package = &imported.packages[0];
        assert_eq!(package.name, "billing");
        assert_eq!(package.version, "2.0.0");
        assert_eq!(package.kotlin.version.as_deref(), Some("2.4.21"));
        assert_eq!(package.kotlin.compiler_args, ["-Xcontext-parameters"]);
        assert_eq!(package.kotlin.opt_in, ["kotlin.ExperimentalStdlibApi"]);
        assert_eq!(package.jvm_target.as_deref(), Some("17"));
        assert_eq!(package.main_class.as_deref(), Some("com.example.billing.MainKt"));
        let text = package.manifest(None);
        assert!(text.contains("\"com.fasterxml.jackson:jackson-bom\" = { version = \"2.19.0\", bom = true }"), "{text}");
        assert!(text.contains("\"com.fasterxml.jackson.module:jackson-module-kotlin\" = {}"), "{text}");
        assert!(text.contains("\"org.postgresql:postgresql\" = { version = \"42.7.5\", scope = \"runtime\" }"), "{text}");
        assert!(text.contains("scope = \"provided\""), "{text}");
        assert!(text.contains("exclude = [\"commons-logging:commons-logging\"]"), "{text}");
        assert!(!text.contains("kotlin-stdlib") && !text.contains("kotlin-test"), "{text}");
        assert!(text.contains("[dev-dependencies]\n\"io.mockk:mockk-jvm\" = \"1.13.12\""), "{text}");
        assert!(text.contains("company = \"https://repo.example.com/maven\""), "{text}");
        assert!(!text.contains("repo.maven.apache.org"), "{text}");
        kargo_manifest::Manifest::parse(&text).unwrap();
        let moves: Vec<_> =
            package.moves.iter().map(|m| (m.from.to_string_lossy().replace('\\', "/"), m.to.to_string_lossy().into_owned())).collect();
        assert!(moves.contains(&("src/main/kotlin".to_owned(), "src".to_owned())), "{moves:?}");
        assert!(moves.contains(&("src/test/kotlin".to_owned(), "tests".to_owned())), "{moves:?}");
        assert!(moves.contains(&("src/main/resources".to_owned(), "resources".to_owned())), "{moves:?}");
    }

    #[test]
    fn a_reactor_becomes_a_workspace_with_path_dependencies() {
        let home = tempfile::tempdir().unwrap();
        let parent = r#"<project>
  <groupId>com.acme</groupId><artifactId>shop</artifactId><version>1.0</version><packaging>pom</packaging>
  <modules><module>core</module><module>app</module></modules>
  <properties><kotlin.version>2.4.21</kotlin.version><shared>3.1</shared></properties>
  <dependencyManagement><dependencies>
    <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId><version>2.0.13</version></dependency>
  </dependencies></dependencyManagement>
  <build><plugins><plugin><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-plugin</artifactId><version>${kotlin.version}</version></plugin></plugins></build>
</project>"#;
        let core = r#"<project>
  <parent><groupId>com.acme</groupId><artifactId>shop</artifactId><version>1.0</version></parent>
  <artifactId>core</artifactId>
  <dependencies><dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency></dependencies>
</project>"#;
        let app = r#"<project>
  <parent><groupId>com.acme</groupId><artifactId>shop</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <dependencies>
    <dependency><groupId>com.acme</groupId><artifactId>core</artifactId><version>${project.version}</version></dependency>
    <dependency><groupId>com.acme</groupId><artifactId>core</artifactId><version>${project.version}</version><scope>test</scope><type>test-jar</type></dependency>
  </dependencies>
</project>"#;
        let dir = project(&[("pom.xml", parent), ("core/pom.xml", core), ("app/pom.xml", app)]);
        let imported = read(&config(home.path()), dir.path()).unwrap();
        assert!(imported.workspace);
        let names: Vec<_> = imported.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["core", "app"]);
        let app = &imported.packages[1];
        assert_eq!(app.group.as_deref(), Some("com.acme"));
        assert_eq!(app.version, "1.0");
        let dep = &app.dependencies[0];
        assert_eq!(
            dep.path.as_ref().map(|p| (p.name.as_str(), p.dir.to_string_lossy().replace('\\', "/"))),
            Some(("core", "../core".to_owned()))
        );
        // The inherited, managed dependency of `core`.
        let core = &imported.packages[0];
        assert_eq!(core.dependencies[0].coords(), "org.slf4j:slf4j-api");
        assert_eq!(core.dependencies[0].version.as_deref(), Some("2.0.13"));
        assert_eq!(core.kotlin.version.as_deref(), Some("2.4.21"), "the plugin comes from the parent");
    }

    #[test]
    fn a_child_module_import_links_siblings_listed_in_the_parent() {
        let home = tempfile::tempdir().unwrap();
        let parent = r#"<project>
  <groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0.0</version><packaging>pom</packaging>
  <modules><module>core</module><module>api</module></modules>
  <properties><kotlin.version>2.4.21</kotlin.version></properties>
  <build><plugins><plugin><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-plugin</artifactId><version>${kotlin.version}</version></plugin></plugins></build>
</project>"#;
        let core = r#"<project>
  <parent><groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0.0</version></parent>
  <artifactId>core</artifactId>
</project>"#;
        let api = r#"<project>
  <parent><groupId>com.example</groupId><artifactId>parent</artifactId><version>1.0.0</version></parent>
  <artifactId>api</artifactId>
  <dependencies><dependency><groupId>com.example</groupId><artifactId>core</artifactId><version>1.0.0</version></dependency></dependencies>
</project>"#;
        let dir = project(&[("pom.xml", parent), ("core/pom.xml", core), ("api/pom.xml", api)]);
        let imported = read(&config(home.path()), &dir.path().join("api")).unwrap();
        assert!(!imported.workspace);
        let package = &imported.packages[0];
        assert_eq!(package.name, "api");
        let dep = package.dependencies.iter().find(|d| d.coords() == "com.example:core").expect("sibling dependency");
        assert_eq!(
            dep.path.as_ref().map(|p| (p.name.as_str(), p.dir.to_string_lossy().replace('\\', "/"))),
            Some(("core", "../core".to_owned())),
            "a sibling outside the import becomes a path dependency"
        );
        let text = package.manifest(None);
        assert!(text.contains("core = { path = \"../core\" }"), "{text}");
        assert!(
            imported.notes.iter().any(|n| !n.translated && n.message.contains("com.example:core looks like the sibling module")),
            "a warning names the sibling: {:?}",
            imported.notes
        );
    }

    #[test]
    fn spring_boot_parent_becomes_the_framework() {
        let home = tempfile::tempdir().unwrap();
        let pom = r#"<project>
  <parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.3.1</version><relativePath/></parent>
  <groupId>com.example</groupId><artifactId>demo</artifactId><version>0.0.1</version>
  <properties><java.version>21</java.version><kotlin.version>2.4.21</kotlin.version></properties>
  <dependencies>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-data-jpa</artifactId></dependency>
    <dependency><groupId>com.fasterxml.jackson.module</groupId><artifactId>jackson-module-kotlin</artifactId></dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-reflect</artifactId></dependency>
    <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-stdlib</artifactId></dependency>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-test</artifactId><scope>test</scope></dependency>
  </dependencies>
  <build><plugins>
    <plugin><groupId>org.springframework.boot</groupId><artifactId>spring-boot-maven-plugin</artifactId></plugin>
    <plugin>
      <groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-plugin</artifactId>
      <configuration>
        <args><arg>-Xjsr305=strict</arg></args>
        <compilerPlugins><plugin>spring</plugin><plugin>jpa</plugin></compilerPlugins>
      </configuration>
    </plugin>
  </plugins></build>
</project>"#;
        // The parent is not in a repository the test can reach: the import carries on without it.
        let dir = project(&[("pom.xml", pom)]);
        let imported = read(&config(home.path()), dir.path()).unwrap();
        let package = &imported.packages[0];
        let spring = package.frameworks.spring_boot.as_ref().expect("spring boot");
        assert_eq!(spring.starters, ["web", "data-jpa"]);
        assert_eq!(package.jvm_target.as_deref(), Some("21"));
        assert!(package.kotlin.compiler_args.is_empty(), "{:?}", package.kotlin.compiler_args);
        assert!(package.plugins.is_empty());
        assert_eq!(
            package.dependencies.iter().map(Dependency::coords).collect::<Vec<_>>(),
            ["com.fasterxml.jackson.module:jackson-module-kotlin"]
        );
        assert!(imported.notes.iter().any(|n| !n.translated && n.message.contains("parent POM")), "{:?}", imported.notes);
    }

    #[test]
    fn package_names_are_made_valid() {
        assert_eq!(package_name("my-app"), ("my-app".to_owned(), false));
        assert_eq!(package_name("com.acme.app"), ("com-acme-app".to_owned(), true));
        assert_eq!(package_name("3d"), ("pkg-3d".to_owned(), true));
    }

    #[test]
    fn java_versions() {
        assert_eq!(java_target("17").as_deref(), Some("17"));
        assert_eq!(java_target("1.8").as_deref(), Some("8"));
        assert_eq!(java_target("1.7"), None);
        assert_eq!(java_target("${x}"), None);
    }

    #[test]
    fn licenses_map_to_spdx() {
        assert_eq!(
            spdx("The Apache Software License, Version 2.0 https://www.apache.org/licenses/LICENSE-2.0.txt").as_deref(),
            Some("Apache-2.0")
        );
        assert_eq!(spdx("MIT License https://opensource.org/licenses/MIT").as_deref(), Some("MIT"));
        assert_eq!(spdx("Proprietary"), None);
    }
}
