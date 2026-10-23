//! Transitive dependency resolution.
//!
//! Conflicts are resolved like Gradle: the highest requested version of each
//! module wins, unless a root dependency is `force`d. Kotlin Multiplatform
//! libraries are resolved through Gradle module metadata so that each target
//! platform receives the matching artifact (`.jar` or `.klib`).
//!
//! The graph is made of modules (`group:artifact`), and every module has one version. What a
//! module contributes to the classpath are artifacts: its default files, plus one file for each
//! root that asks for a `classifier` or `extension` (native libraries are declared that way).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use super::coord::{Artifact, ModuleId};
use super::module::{ModuleMetadata, Platform, select_variant};
use super::pom::{EffectivePom, Pom, PomSource, effective_pom};
use super::repo::{MavenClient, resolve_relative};
use super::version::{Version, VersionReq};

/// Bumped whenever resolution semantics change, invalidating locked results.
pub const RESOLVER_VERSION: &str = "5";

/// Plain jars that Kotlin/Wasm libraries depend on for the files their page needs, not for code:
/// Skiko's `skiko.mjs` and `skiko.wasm`, which Compose for Web draws with. They resolve on the Wasm
/// targets like any library, and the build ships their contents next to the module.
pub const WASM_RUNTIME_JARS: &[&str] = &["org.jetbrains.skiko:skiko-js-wasm-runtime"];

/// An exclusion pattern; `*` matches anything.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Exclusion {
    pub group: String,
    pub artifact: String,
}

impl Exclusion {
    pub fn new(group: impl Into<String>, artifact: impl Into<String>) -> Self {
        Self { group: group.into(), artifact: artifact.into() }
    }

    pub fn matches(&self, module: &ModuleId) -> bool {
        (self.group == "*" || self.group == module.group) && (self.artifact == "*" || self.artifact == module.artifact)
    }
}

/// A dependency requested by the project (a resolution root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRequest {
    pub module: ModuleId,
    /// Version requirement; `None` means "take it from a BOM".
    pub version: Option<String>,
    pub classifier: Option<String>,
    pub extension: Option<String>,
    pub exclusions: Vec<Exclusion>,
    /// Pin this exact version even if something transitive asks for newer.
    pub force: bool,
    /// Whether to follow this dependency's own dependencies.
    pub transitive: bool,
}

impl DependencyRequest {
    pub fn new(module: ModuleId, version: Option<String>) -> Self {
        Self { module, version, classifier: None, extension: None, exclusions: Vec::new(), force: false, transitive: true }
    }

    /// Whether this asks for the module's default artifact rather than one with a classifier or extension.
    pub fn is_default_artifact(&self) -> bool {
        self.classifier.is_none() && self.extension.is_none()
    }

    /// Whether both ask for the same artifact: `group:artifact:classifier:extension`.
    pub fn same_artifact(&self, other: &DependencyRequest) -> bool {
        self.module == other.module && self.classifier == other.classifier && self.extension == other.extension
    }
}

/// One resolved module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNode {
    pub module: ModuleId,
    pub version: String,
    /// Downloaded artifact files: the module's own and the ones requested with a classifier or
    /// extension (may be empty for POM-only or redirect modules).
    pub files: Vec<PathBuf>,
    /// Direct dependencies with the version each one asked for.
    pub dependencies: Vec<(ModuleId, Option<String>)>,
}

/// The result of resolving a set of roots for one platform.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The modules the project asks for, once each however many classifiers it declares.
    pub roots: Vec<ModuleId>,
    /// Nodes in breadth-first order (roots first), which is also classpath order.
    pub nodes: Vec<ResolvedNode>,
}

impl Resolution {
    pub fn node(&self, module: &ModuleId) -> Option<&ResolvedNode> {
        self.nodes.iter().find(|n| &n.module == module)
    }

    /// All artifact files in classpath order.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut seen = BTreeSet::new();
        self.nodes.iter().flat_map(|n| n.files.iter()).filter(|f| seen.insert((*f).clone())).cloned().collect()
    }
}

/// `(module, version)` pairs that align versions without adding modules.
type Constraints = Vec<(ModuleId, String)>;

/// A transitive edge discovered in metadata.
#[derive(Debug, Clone)]
struct Edge {
    module: ModuleId,
    version: Option<String>,
    exclusions: Vec<Exclusion>,
}

/// What a single `module:version` contributes to the graph.
#[derive(Debug, Clone, Default)]
struct NodeInfo {
    edges: Vec<Edge>,
    /// Repository-relative artifact paths.
    files: Vec<String>,
    /// Version constraints that apply only if the module is otherwise in the
    /// graph (Gradle `dependencyConstraints` and platform/BOM alignment).
    constraints: Vec<(ModuleId, String)>,
}

/// Modules provided by the Kotlin/Native distribution rather than Maven.
fn provided_by_toolchain(platform: &Platform, module: &ModuleId) -> bool {
    matches!(platform, Platform::Native(_))
        && module.group == "org.jetbrains.kotlin"
        && matches!(
            module.artifact.as_str(),
            "kotlin-stdlib" | "kotlin-stdlib-common" | "kotlin-test" | "kotlin-test-common" | "kotlin-test-annotations-common"
        )
}

pub struct Resolver<'a> {
    client: &'a MavenClient,
    platform: Platform,
    /// Versions managed by BOMs/platforms declared in the manifest.
    constraints: HashMap<ModuleId, String>,
    /// Versions fixed by the caller (`kargo update` keeping locked versions), applied whenever
    /// the module is in the graph and overriding everything else.
    pins: HashMap<ModuleId, Version>,
    infos: Mutex<HashMap<(ModuleId, String), NodeInfo>>,
    poms: Mutex<HashMap<(ModuleId, String), Pom>>,
    effective: Mutex<HashMap<(ModuleId, String), EffectivePom>>,
    platforms: Mutex<HashMap<(ModuleId, String), Constraints>>,
}

struct ClientPomSource<'a, 'b>(&'b Resolver<'a>);

impl PomSource for ClientPomSource<'_, '_> {
    fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom> {
        self.0.raw_pom(module, version)?.with_context(|| format!("POM for {module}:{version} not found"))
    }
}

/// Accumulated requests for one module during a traversal.
#[derive(Debug, Default, Clone)]
struct Requests {
    soft: Vec<Version>,
    ranges: Vec<VersionReq>,
    forced: Option<Version>,
}

impl Requests {
    fn is_empty(&self) -> bool {
        self.soft.is_empty() && self.ranges.is_empty() && self.forced.is_none()
    }
}

const MAX_ITERATIONS: usize = 64;

impl<'a> Resolver<'a> {
    pub fn new(client: &'a MavenClient, platform: Platform) -> Self {
        Self {
            client,
            platform,
            constraints: HashMap::new(),
            pins: HashMap::new(),
            infos: Mutex::new(HashMap::new()),
            poms: Mutex::new(HashMap::new()),
            effective: Mutex::new(HashMap::new()),
            platforms: Mutex::new(HashMap::new()),
        }
    }

    /// Imports a BOM's `<dependencyManagement>` as version constraints.
    pub fn import_bom(&mut self, module: &ModuleId, version: &str) -> Result<()> {
        let pom = self.effective_pom(module, version).with_context(|| format!("failed to import BOM {module}:{version}"))?;
        for managed in pom.dependency_management {
            let module = managed.module();
            if let Some(v) = managed.version {
                self.constraints.entry(module).or_insert(v);
            }
        }
        Ok(())
    }

    pub fn add_constraint(&mut self, module: ModuleId, version: String) {
        self.constraints.insert(module, version);
    }

    /// Fixes `module` at `version` if it appears in the graph, without adding it.
    pub fn pin(&mut self, module: ModuleId, version: &str) {
        self.pins.insert(module, Version::parse(version));
    }

    /// Like [`Resolver::pin`] for a SNAPSHOT `version`, fixed at the build whose files are named
    /// `build` (`1.0-20240101.120000-1`) instead of the newest one.
    pub fn pin_build(&mut self, module: ModuleId, version: &str, build: &str) {
        self.client.pin_snapshot_build(&module, version, build);
        self.pin(module, version);
    }

    pub fn managed_version(&self, module: &ModuleId) -> Option<&str> {
        self.constraints.get(module).map(String::as_str)
    }

    fn raw_pom(&self, module: &ModuleId, version: &str) -> Result<Option<Pom>> {
        let key = (module.clone(), version.to_owned());
        if let Some(pom) = self.poms.lock().expect("lock poisoned").get(&key) {
            return Ok(Some(pom.clone()));
        }
        let artifact = Artifact::new(module.clone(), version).with_extension("pom");
        let Some(xml) = self.client.fetch_text(&self.client.artifact_path(&artifact)?)? else { return Ok(None) };
        let pom = Pom::parse(&xml).with_context(|| format!("failed to parse POM of {module}:{version}"))?;
        self.poms.lock().expect("lock poisoned").insert(key, pom.clone());
        Ok(Some(pom))
    }

    fn effective_pom(&self, module: &ModuleId, version: &str) -> Result<EffectivePom> {
        let key = (module.clone(), version.to_owned());
        if let Some(eff) = self.effective.lock().expect("lock poisoned").get(&key) {
            return Ok(eff.clone());
        }
        let eff = effective_pom(&ClientPomSource(self), module, version)?;
        self.effective.lock().expect("lock poisoned").insert(key, eff.clone());
        Ok(eff)
    }

    fn node_info(&self, module: &ModuleId, version: &str) -> Result<NodeInfo> {
        let key = (module.clone(), version.to_owned());
        if let Some(info) = self.infos.lock().expect("lock poisoned").get(&key) {
            return Ok(info.clone());
        }
        let info = self.load_node_info(module, version).with_context(|| format!("failed to resolve {module}:{version}"))?;
        self.infos.lock().expect("lock poisoned").insert(key, info.clone());
        Ok(info)
    }

    fn has_info(&self, module: &ModuleId, version: &str) -> bool {
        self.infos.lock().expect("lock poisoned").contains_key(&(module.clone(), version.to_owned()))
    }

    /// Loads node infos concurrently; errors are ignored here and reported when the
    /// traversal itself asks for that node.
    fn prefetch(&self, mut batch: Vec<(ModuleId, String)>) {
        batch.sort();
        batch.dedup();
        batch.retain(|(m, v)| !self.has_info(m, v));
        if batch.len() < 2 {
            return;
        }
        crate::io_pool().install(|| {
            batch.par_iter().for_each(|(m, v)| {
                let _ = self.node_info(m, v);
            })
        });
    }

    fn load_node_info(&self, module: &ModuleId, version: &str) -> Result<NodeInfo> {
        let pom = self.raw_pom(module, version)?;
        let wants_metadata = pom.as_ref().is_none_or(|p| p.has_gradle_metadata);
        if wants_metadata && let Some(info) = self.info_from_module_metadata(module, version, pom.is_some())? {
            return Ok(info);
        }
        if pom.is_none() {
            bail!("{module}:{version} was not found in any repository");
        }
        let web_runtime = matches!(self.platform, Platform::Wasm(_)) && WASM_RUNTIME_JARS.contains(&module.to_string().as_str());
        if self.platform != Platform::Jvm && !web_runtime {
            bail!("{module}:{version} is a JVM-only library and cannot be used for the {} target", self.platform);
        }
        self.info_from_pom(module, version)
    }

    fn info_from_module_metadata(&self, module: &ModuleId, version: &str, has_pom: bool) -> Result<Option<NodeInfo>> {
        let artifact = Artifact::new(module.clone(), version).with_extension("module");
        let Some(json) = self.client.fetch_text(&self.client.artifact_path(&artifact)?)? else { return Ok(None) };
        let meta = ModuleMetadata::parse(&json)?;
        let Some(variant) = select_variant(&meta, module, &self.platform) else {
            if has_pom && self.platform == Platform::Jvm {
                return Ok(None);
            }
            bail!("{module}:{version} does not publish a variant for the {} target", self.platform);
        };

        let base_dir = format!("{}/{version}", module.repo_dir());
        if let Some(at) = &variant.available_at {
            return Ok(Some(NodeInfo {
                edges: vec![Edge {
                    module: ModuleId::new(&at.group, &at.module),
                    // A redirect is an ordinary requirement: conflict resolution may
                    // still pick a newer version of the target (as Gradle does).
                    version: Some(at.version.clone()),
                    exclusions: Vec::new(),
                }],
                files: Vec::new(),
                constraints: Vec::new(),
            }));
        }

        let mut constraints: Vec<(ModuleId, String)> = variant
            .dependency_constraints
            .iter()
            .filter_map(|c| Some((c.module_id(), c.version.as_ref()?.preferred()?.to_owned())))
            .collect();
        let mut edges: Vec<Edge> = Vec::new();
        for dep in &variant.dependencies {
            if dep.is_platform() {
                // A platform (BOM) aligns versions without adding modules.
                if let Some(v) = dep.version.as_ref().and_then(|v| v.preferred()) {
                    match self.platform_constraints(&dep.module_id(), v) {
                        Ok(c) => constraints.extend(c),
                        Err(e) => self.client.shell().verbose("Ignoring", format!("platform {}:{v}: {e:#}", dep.module_id())),
                    }
                }
                continue;
            }
            let id = dep.module_id();
            if edges.iter().any(|e| e.module == id) {
                continue;
            }
            edges.push(Edge {
                module: id,
                version: dep.version.as_ref().and_then(|v| v.preferred()).map(str::to_owned),
                exclusions: dep.excludes.iter().map(|e| Exclusion::new(&e.group, &e.module)).collect(),
            });
        }
        let files = variant.files.iter().map(|f| resolve_relative(&base_dir, &f.url)).collect::<Result<Vec<_>>>()?;
        Ok(Some(NodeInfo { edges, files, constraints }))
    }

    /// Constraints published by a platform module (Gradle platform variant or Maven BOM).
    fn platform_constraints(&self, module: &ModuleId, version: &str) -> Result<Vec<(ModuleId, String)>> {
        let key = (module.clone(), version.to_owned());
        if let Some(cached) = self.platforms.lock().expect("lock poisoned").get(&key) {
            return Ok(cached.clone());
        }
        let artifact = Artifact::new(module.clone(), version).with_extension("module");
        let mut constraints = Vec::new();
        let from_metadata = match self.client.fetch_text(&self.client.artifact_path(&artifact)?)? {
            Some(json) => {
                let meta = ModuleMetadata::parse(&json)?;
                meta.variants.iter().find(|v| v.attributes.get("org.gradle.category").and_then(|c| c.as_str()) == Some("platform")).map(
                    |v| {
                        v.dependency_constraints
                            .iter()
                            .filter_map(|c| Some((c.module_id(), c.version.as_ref()?.preferred()?.to_owned())))
                            .collect::<Vec<_>>()
                    },
                )
            }
            None => None,
        };
        match from_metadata {
            Some(c) => constraints.extend(c),
            None => {
                let pom = self.effective_pom(module, version)?;
                constraints.extend(pom.dependency_management.iter().filter_map(|d| Some((d.module(), d.version.clone()?))));
            }
        }
        self.platforms.lock().expect("lock poisoned").insert(key, constraints.clone());
        Ok(constraints)
    }

    fn info_from_pom(&self, module: &ModuleId, version: &str) -> Result<NodeInfo> {
        let pom = self.effective_pom(module, version)?;
        if let Some((group, artifact, new_version)) = &pom.relocation {
            let target = ModuleId::new(
                group.clone().unwrap_or_else(|| module.group.clone()),
                artifact.clone().unwrap_or_else(|| module.artifact.clone()),
            );
            let target_version = new_version.clone().unwrap_or_else(|| version.to_owned());
            if target != *module || target_version != version {
                return Ok(NodeInfo {
                    edges: vec![Edge { module: target, version: Some(target_version), exclusions: Vec::new() }],
                    files: Vec::new(),
                    constraints: Vec::new(),
                });
            }
        }

        let mut edges = Vec::new();
        for dep in &pom.dependencies {
            let scope = dep.scope.as_deref().unwrap_or("compile");
            if dep.optional || !matches!(scope, "compile" | "runtime") {
                continue;
            }
            let id = dep.module();
            let version = dep.version.clone().or_else(|| self.constraints.get(&id).cloned());
            edges.push(Edge { module: id, version, exclusions: dep.exclusions.iter().map(|(g, a)| Exclusion::new(g, a)).collect() });
        }

        let extension = match pom.packaging.as_str() {
            "pom" => None,
            "aar" => Some("aar"),
            "klib" => Some("klib"),
            _ => Some("jar"),
        };
        let files = match extension {
            Some(ext) => vec![self.client.artifact_path(&Artifact::new(module.clone(), version).with_extension(ext))?],
            None => Vec::new(),
        };
        Ok(NodeInfo { edges, files, constraints: Vec::new() })
    }

    /// Chooses the version for a module given everything that asked for it.
    fn choose(&self, module: &ModuleId, requests: &Requests) -> Result<Version> {
        if let Some(pinned) = self.pins.get(module) {
            return Ok(pinned.clone());
        }
        if let Some(forced) = &requests.forced {
            return Ok(forced.clone());
        }
        let mut softs: Vec<Version> = requests.soft.clone();
        if let Some(managed) = self.constraints.get(module) {
            softs.push(Version::parse(managed));
        }
        if requests.ranges.is_empty() {
            return softs.into_iter().max().with_context(|| format!("no version requested for {module}"));
        }
        // Exact pins such as `[1.0]` are candidates without consulting the repository.
        softs.extend(requests.ranges.iter().filter_map(|r| r.exact().cloned()));
        let satisfies_all = |v: &Version| requests.ranges.iter().all(|r| r.matches(v));
        if let Some(best) = softs.iter().filter(|v| satisfies_all(v)).max() {
            return Ok(best.clone());
        }
        let available = self.client.versions(module)?;
        available.into_iter().filter(|v| satisfies_all(v)).max().with_context(|| {
            let ranges: Vec<String> = requests.ranges.iter().map(ToString::to_string).collect();
            format!("no version of {module} satisfies all requirements: {}", ranges.join(", "))
        })
    }

    /// Resolves the dependency graph and downloads all artifacts.
    pub fn resolve(&self, roots: &[DependencyRequest]) -> Result<Resolution> {
        let roots: Vec<&DependencyRequest> = roots.iter().filter(|r| !provided_by_toolchain(&self.platform, &r.module)).collect();

        let mut root_requests: HashMap<ModuleId, Requests> = HashMap::new();
        for root in &roots {
            let raw = match &root.version {
                Some(v) => v.clone(),
                None => self
                    .constraints
                    .get(&root.module)
                    .cloned()
                    .with_context(|| format!("dependency `{}` has no version and no BOM manages it", root.module))?,
            };
            let req: VersionReq = raw.parse().with_context(|| format!("invalid version for `{}`", root.module))?;
            let entry = root_requests.entry(root.module.clone()).or_default();
            if root.force {
                let pinned = req.exact().cloned().with_context(|| format!("`force` requires an exact version for `{}`", root.module))?;
                entry.forced = Some(pinned);
            }
            match req {
                VersionReq::Soft(v) => entry.soft.push(v),
                ranges => entry.ranges.push(ranges),
            }
        }

        let mut selected: HashMap<ModuleId, Version> = HashMap::new();
        for _ in 0..MAX_ITERATIONS {
            let (order, used, requests, edges_by_module) = self.traverse(&roots, &root_requests, &selected)?;
            let mut next: HashMap<ModuleId, Version> = HashMap::new();
            for module in &order {
                let chosen = self.choose(module, &requests[module])?;
                next.insert(module.clone(), chosen);
            }
            let stable = order.iter().all(|m| next.get(m) == used.get(m));
            if stable {
                return self.finish(&roots, order, &used, &edges_by_module);
            }
            selected.extend(next);
        }
        let (order, used, requests, _) = self.traverse(&roots, &root_requests, &selected)?;
        let unstable: Vec<String> = order
            .iter()
            .filter_map(|m| {
                let next = self.choose(m, &requests[m]).ok()?;
                (Some(&next) != used.get(m)).then(|| format!("{m} ({} vs {next})", used.get(m).map(Version::as_str).unwrap_or("?")))
            })
            .collect();
        bail!("dependency resolution did not converge after {MAX_ITERATIONS} iterations; conflicting modules: {}", unstable.join(", "))
    }

    #[allow(clippy::type_complexity)]
    fn traverse(
        &self,
        roots: &[&DependencyRequest],
        root_requests: &HashMap<ModuleId, Requests>,
        selected: &HashMap<ModuleId, Version>,
    ) -> Result<(Vec<ModuleId>, HashMap<ModuleId, Version>, HashMap<ModuleId, Requests>, HashMap<ModuleId, Vec<Edge>>)> {
        let mut requests: HashMap<ModuleId, Requests> = root_requests.clone();
        let mut order: Vec<ModuleId> = Vec::new();
        let mut used: HashMap<ModuleId, Version> = HashMap::new();
        let mut exclusions_seen: HashMap<ModuleId, BTreeSet<Exclusion>> = HashMap::new();
        let mut edges_by_module: HashMap<ModuleId, Vec<Edge>> = HashMap::new();
        let mut queue: VecDeque<(ModuleId, BTreeSet<Exclusion>, bool)> = VecDeque::new();

        for root in roots {
            let excl: BTreeSet<Exclusion> = root.exclusions.iter().cloned().collect();
            queue.push_back((root.module.clone(), excl, root.transitive));
        }

        // Consecutive deferrals of modules whose version is not known yet.
        let mut stalled = 0usize;
        while let Some((module, excl, transitive)) = queue.pop_front() {
            // A module reached only through version-less edges waits until
            // another path requests a version (Gradle semantics).
            let decidable =
                used.contains_key(&module) || selected.contains_key(&module) || requests.get(&module).is_some_and(|r| !r.is_empty());
            if !decidable {
                stalled += 1;
                if stalled > queue.len() {
                    bail!("{module} is required without a version and nothing else in the graph requests one; declare it with a version");
                }
                queue.push_back((module, excl, transitive));
                continue;
            }
            stalled = 0;

            let version = match used.get(&module) {
                Some(v) => v.clone(),
                None => {
                    // Reuse the previous iteration's decision: it was made with
                    // complete information, unlike the partial requests seen so far.
                    let v = match selected.get(&module) {
                        Some(v) => v.clone(),
                        None => self.choose(&module, &requests[&module])?,
                    };
                    used.insert(module.clone(), v.clone());
                    order.push(module.clone());
                    v
                }
            };
            // A non-transitive root only asks for the module itself. It does not mark the module as
            // visited: another path, or a root for another classifier of it, may still bring its
            // dependencies in.
            if !transitive {
                continue;
            }

            // Revisit only if this path excludes less than every previous one.
            match exclusions_seen.get_mut(&module) {
                Some(prev) => {
                    let narrowed: BTreeSet<Exclusion> = prev.intersection(&excl).cloned().collect();
                    if narrowed == *prev {
                        continue;
                    }
                    *prev = narrowed;
                }
                None => {
                    exclusions_seen.insert(module.clone(), excl.clone());
                }
            }
            let excl = exclusions_seen[&module].clone();
            if !self.has_info(&module, version.as_str()) {
                // Fetch the metadata of everything queued whose version is already known in
                // parallel, instead of one network round-trip per module.
                let mut batch = vec![(module.clone(), version.to_string())];
                for (m, _, t) in &queue {
                    let guess = used.get(m).or_else(|| selected.get(m)).cloned().or_else(|| {
                        requests
                            .get(m)
                            .filter(|r| r.ranges.is_empty())
                            .and_then(|r| r.forced.clone().or_else(|| r.soft.iter().max().cloned()))
                    });
                    if let (true, Some(v)) = (*t, guess) {
                        batch.push((m.clone(), v.to_string()));
                    }
                }
                self.prefetch(batch);
            }
            let info = self.node_info(&module, version.as_str())?;
            for (target, v) in &info.constraints {
                if let Ok(VersionReq::Soft(v)) = v.parse::<VersionReq>() {
                    requests.entry(target.clone()).or_default().soft.push(v);
                }
            }
            let mut kept = Vec::new();
            for edge in info.edges {
                if excl.iter().any(|e| e.matches(&edge.module)) || provided_by_toolchain(&self.platform, &edge.module) {
                    continue;
                }
                let edge_version = edge.version.clone().or_else(|| self.constraints.get(&edge.module).cloned());
                let entry = requests.entry(edge.module.clone()).or_default();
                if let Some(raw) = edge_version {
                    let req: VersionReq =
                        raw.parse().with_context(|| format!("{module}:{version} has an invalid version for {}", edge.module))?;
                    match req {
                        VersionReq::Soft(v) => entry.soft.push(v),
                        ranges => entry.ranges.push(ranges),
                    }
                }
                let mut child_excl = excl.clone();
                child_excl.extend(edge.exclusions.iter().cloned());
                queue.push_back((edge.module.clone(), child_excl, true));
                kept.push(edge);
            }
            edges_by_module.insert(module, kept);
        }
        Ok((order, used, requests, edges_by_module))
    }

    fn finish(
        &self,
        roots: &[&DependencyRequest],
        order: Vec<ModuleId>,
        used: &HashMap<ModuleId, Version>,
        edges: &HashMap<ModuleId, Vec<Edge>>,
    ) -> Result<Resolution> {
        // A module gets its default files when a root without classifier or extension, or another
        // module, depends on it. Roots with a classifier or extension add their own artifact on top
        // of those, and are the only artifacts of a module nothing else depends on (as in Gradle).
        let mut wants_default: HashSet<&ModuleId> = edges.values().flatten().map(|e| &e.module).collect();
        wants_default.extend(roots.iter().filter(|r| r.is_default_artifact()).map(|r| &r.module));

        let file_lists: Vec<Vec<String>> = order
            .iter()
            .map(|module| {
                let version = used[module].as_str();
                let mut files = if wants_default.contains(module) { self.node_info(module, version)?.files } else { Vec::new() };
                for root in roots.iter().filter(|r| &r.module == module && !r.is_default_artifact()) {
                    let artifact = Artifact::new(module.clone(), version)
                        .with_classifier(root.classifier.clone())
                        .with_extension(root.extension.clone().unwrap_or_else(|| "jar".into()));
                    let path = self.client.artifact_path(&artifact)?;
                    if !files.contains(&path) {
                        files.push(path);
                    }
                }
                Ok(files)
            })
            .collect::<Result<_>>()?;

        let downloaded: Vec<Vec<PathBuf>> = crate::io_pool().install(|| {
            file_lists
                .par_iter()
                .map(|files| files.iter().map(|rel| self.client.fetch_required(rel)).collect::<Result<Vec<_>>>())
                .collect::<Result<_>>()
        })?;

        let nodes = order
            .iter()
            .zip(downloaded)
            .map(|(module, files)| ResolvedNode {
                module: module.clone(),
                version: used[module].to_string(),
                files,
                dependencies: edges
                    .get(module)
                    .map(|es| es.iter().map(|e| (e.module.clone(), e.version.clone())).collect())
                    .unwrap_or_default(),
            })
            .collect();
        let mut root_modules: Vec<ModuleId> = Vec::new();
        for root in roots {
            if !root_modules.contains(&root.module) {
                root_modules.push(root.module.clone());
            }
        }
        Ok(Resolution { roots: root_modules, nodes })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::repo::Repository;
    use kargo_util::config::Config;
    use kargo_util::shell::{Shell, Verbosity};

    /// A tiny on-disk Maven repository for tests.
    struct TestRepo {
        dir: tempfile::TempDir,
        home: tempfile::TempDir,
    }

    impl TestRepo {
        fn new() -> Self {
            Self { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() }
        }

        fn write(&self, rel: &str, contents: &str) {
            let p = self.dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }

        /// Publishes a jar module with the given `(group:artifact, version, scope/optional)` deps.
        fn publish(&self, coord: &str, version: &str, deps: &[(&str, &str, &str)]) {
            let (group, artifact) = coord.split_once(':').unwrap();
            let dir = format!("{}/{artifact}/{version}", group.replace('.', "/"));
            self.write(&format!("{dir}/{artifact}-{version}.pom"), &pom_xml(group, artifact, version, deps));
            self.write(&format!("{dir}/{artifact}-{version}.jar"), "jar");
            self.add_metadata_version(group, artifact, version);
        }

        /// Publishes a SNAPSHOT the way `mvn deploy` stores it: files named after the build (`stamp` is
        /// `20240101.120000-1`) in the `-SNAPSHOT` directory, which the version's `maven-metadata.xml` lists.
        fn publish_snapshot(&self, coord: &str, version: &str, stamp: &str, deps: &[(&str, &str, &str)]) {
            let (group, artifact) = coord.split_once(':').unwrap();
            let dir = format!("{}/{artifact}/{version}", group.replace('.', "/"));
            let file_version = format!("{}-{stamp}", version.strip_suffix("-SNAPSHOT").unwrap());
            self.write(&format!("{dir}/{artifact}-{file_version}.pom"), &pom_xml(group, artifact, version, deps));
            self.write(&format!("{dir}/{artifact}-{file_version}.jar"), &format!("jar {file_version}"));
            self.write_snapshot_metadata(&dir, &file_version, &["jar", "pom"]);
            self.add_metadata_version(group, artifact, version);
        }

        fn write_snapshot_metadata(&self, dir: &str, file_version: &str, extensions: &[&str]) {
            let updated = file_version.rsplit_once('-').unwrap().0.rsplit('-').next().unwrap().replace('.', "");
            let entries: String = extensions
                .iter()
                .map(|ext| {
                    format!("<snapshotVersion><extension>{ext}</extension><value>{file_version}</value><updated>{updated}</updated></snapshotVersion>")
                })
                .collect();
            self.write(
                &format!("{dir}/maven-metadata.xml"),
                &format!("<metadata><versioning><snapshotVersions>{entries}</snapshotVersions></versioning></metadata>"),
            );
        }

        fn add_metadata_version(&self, group: &str, artifact: &str, version: &str) {
            let rel = format!("{}/{artifact}/maven-metadata.xml", group.replace('.', "/"));
            let path = self.dir.path().join(&rel);
            let mut versions: Vec<String> = fs::read_to_string(&path)
                .ok()
                .map(|s| s.split("<version>").skip(1).map(|p| p.split('<').next().unwrap().to_owned()).collect())
                .unwrap_or_default();
            versions.push(version.to_owned());
            let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
            self.write(&rel, &format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"));
        }

        fn client(&self) -> MavenClient {
            let config = Config::new(self.home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
            MavenClient::new(&config, vec![Repository::new("test", format!("file://{}", self.dir.path().display()))])
        }
    }

    fn pom_xml(group: &str, artifact: &str, version: &str, deps: &[(&str, &str, &str)]) -> String {
        let deps_xml: String = deps
            .iter()
            .map(|(c, v, extra)| {
                let (g, a) = c.split_once(':').unwrap();
                format!("<dependency><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version>{extra}</dependency>")
            })
            .collect();
        format!(
            "<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><dependencies>{deps_xml}</dependencies></project>"
        )
    }

    fn req(coord: &str, version: &str) -> DependencyRequest {
        DependencyRequest::new(coord.parse().unwrap(), Some(version.to_owned()))
    }

    fn versions(res: &Resolution) -> Vec<String> {
        res.nodes.iter().map(|n| format!("{}:{}", n.module.artifact, n.version)).collect()
    }

    fn file_names(res: &Resolution) -> Vec<String> {
        res.files().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn resolves_transitive_graph_in_bfs_order() {
        let repo = TestRepo::new();
        repo.publish("g:app-lib", "1.0", &[("g:a", "1.0", ""), ("g:b", "1.0", "")]);
        repo.publish("g:a", "1.0", &[("g:c", "1.0", "")]);
        repo.publish("g:b", "1.0", &[]);
        repo.publish("g:c", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:app-lib", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["app-lib:1.0", "a:1.0", "b:1.0", "c:1.0"]);
        assert_eq!(file_names(&res), ["app-lib-1.0.jar", "a-1.0.jar", "b-1.0.jar", "c-1.0.jar"]);
        assert_eq!(res.node(&"g:a".parse().unwrap()).unwrap().dependencies.len(), 1);
    }

    #[test]
    fn highest_version_wins_and_drops_stale_subtrees() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:shared", "1.0", "")]);
        repo.publish("g:y", "1.0", &[("g:shared", "2.0", "")]);
        repo.publish("g:shared", "1.0", &[("g:old-only", "1.0", "")]);
        repo.publish("g:shared", "2.0", &[]);
        repo.publish("g:old-only", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0"), req("g:y", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["x:1.0", "y:1.0", "shared:2.0"]);
    }

    #[test]
    fn force_pins_lower_version() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:shared", "2.0", "")]);
        repo.publish("g:shared", "1.0", &[]);
        repo.publish("g:shared", "2.0", &[]);
        let client = repo.client();
        let mut pinned = req("g:shared", "1.0");
        pinned.force = true;
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0"), pinned]).unwrap();
        assert_eq!(res.node(&"g:shared".parse().unwrap()).unwrap().version, "1.0");
    }

    #[test]
    fn pins_override_transitive_versions_without_adding_modules() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:shared", "2.0", "")]);
        repo.publish("g:shared", "1.0", &[]);
        repo.publish("g:shared", "2.0", &[]);
        let client = repo.client();
        let mut resolver = Resolver::new(&client, Platform::Jvm);
        resolver.pin("g:shared".parse().unwrap(), "1.0");
        resolver.pin("g:absent".parse().unwrap(), "9.9");
        let res = resolver.resolve(&[req("g:x", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["x:1.0", "shared:1.0"]);
    }

    #[test]
    fn skips_test_provided_and_optional_scopes() {
        let repo = TestRepo::new();
        repo.publish(
            "g:x",
            "1.0",
            &[
                ("g:t", "1.0", "<scope>test</scope>"),
                ("g:p", "1.0", "<scope>provided</scope>"),
                ("g:o", "1.0", "<optional>true</optional>"),
                ("g:r", "1.0", "<scope>runtime</scope>"),
            ],
        );
        repo.publish("g:r", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["x:1.0", "r:1.0"]);
    }

    #[test]
    fn exclusions_apply_to_subtree() {
        let repo = TestRepo::new();
        repo.publish(
            "g:x",
            "1.0",
            &[("g:a", "1.0", "<exclusions><exclusion><groupId>g</groupId><artifactId>c</artifactId></exclusion></exclusions>")],
        );
        repo.publish("g:a", "1.0", &[("g:c", "1.0", ""), ("g:d", "1.0", "")]);
        repo.publish("g:c", "1.0", &[]);
        repo.publish("g:d", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["x:1.0", "a:1.0", "d:1.0"]);

        // A second path without the exclusion brings `c` back.
        repo.publish("g:y", "1.0", &[("g:c", "1.0", "")]);
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0"), req("g:y", "1.0")]).unwrap();
        assert!(versions(&res).contains(&"c:1.0".to_owned()));

        // Root-level wildcard exclusions.
        let mut root = req("g:x", "1.0");
        root.exclusions.push(Exclusion::new("*", "*"));
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[root]).unwrap();
        assert_eq!(versions(&res), ["x:1.0"]);
    }

    #[test]
    fn non_transitive_roots() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:a", "1.0", "")]);
        let client = repo.client();
        let mut root = req("g:x", "1.0");
        root.transitive = false;
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[root]).unwrap();
        assert_eq!(versions(&res), ["x:1.0"]);
    }

    #[test]
    fn version_ranges_pick_highest_available() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:lib", "[1.0,2.0)", "")]);
        for v in ["1.0", "1.5", "2.0"] {
            repo.publish("g:lib", v, &[]);
        }
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0")]).unwrap();
        assert_eq!(res.node(&"g:lib".parse().unwrap()).unwrap().version, "1.5");
    }

    #[test]
    fn bom_supplies_missing_versions() {
        let repo = TestRepo::new();
        repo.write(
            "g/bom/1.0/bom-1.0.pom",
            "<project><groupId>g</groupId><artifactId>bom</artifactId><version>1.0</version><packaging>pom</packaging>
             <dependencyManagement><dependencies>
               <dependency><groupId>g</groupId><artifactId>lib</artifactId><version>3.0</version></dependency>
             </dependencies></dependencyManagement></project>",
        );
        repo.publish("g:lib", "3.0", &[]);
        let client = repo.client();
        let mut resolver = Resolver::new(&client, Platform::Jvm);
        resolver.import_bom(&"g:bom".parse().unwrap(), "1.0").unwrap();
        assert_eq!(resolver.managed_version(&"g:lib".parse().unwrap()), Some("3.0"));
        let res = resolver.resolve(&[DependencyRequest::new("g:lib".parse().unwrap(), None)]).unwrap();
        assert_eq!(versions(&res), ["lib:3.0"]);

        let err = Resolver::new(&client, Platform::Jvm).resolve(&[DependencyRequest::new("g:lib".parse().unwrap(), None)]).unwrap_err();
        assert!(err.to_string().contains("no BOM manages it"), "{err}");
    }

    #[test]
    fn root_classifier_and_extension() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[]);
        repo.write("g/x/1.0/x-1.0-natives.zip", "zip");
        let client = repo.client();
        let mut root = req("g:x", "1.0");
        root.classifier = Some("natives".into());
        root.extension = Some("zip".into());
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[root]).unwrap();
        assert_eq!(file_names(&res), ["x-1.0-natives.zip"]);
    }

    #[test]
    fn classifier_roots_add_artifacts_to_the_module() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:dep", "1.0", "")]);
        repo.publish("g:dep", "1.0", &[]);
        repo.write("g/x/1.0/x-1.0-natives.jar", "natives");
        repo.write("g/x/1.0/x-1.0-sources.jar", "sources");
        let client = repo.client();
        let natives = |non_transitive: bool| {
            let mut r = req("g:x", "1.0");
            r.classifier = Some("natives".into());
            r.transitive = !non_transitive;
            r
        };

        // The main jar and the classifier jar are both on the classpath, whichever comes first,
        // and the module is one node at one version.
        for roots in [[req("g:x", "1.0"), natives(false)], [natives(false), req("g:x", "1.0")]] {
            let res = Resolver::new(&client, Platform::Jvm).resolve(&roots).unwrap();
            assert_eq!(versions(&res), ["x:1.0", "dep:1.0"]);
            let mut names = file_names(&res);
            names.sort();
            assert_eq!(names, ["dep-1.0.jar", "x-1.0-natives.jar", "x-1.0.jar"]);
            assert_eq!(res.roots, ["g:x".parse().unwrap()], "one module, not one root per classifier");
        }

        // A non-transitive classifier root (the usual way to declare natives) must not stop the
        // plain declaration from bringing in the module's dependencies, even when it is listed first.
        for roots in [[req("g:x", "1.0"), natives(true)], [natives(true), req("g:x", "1.0")]] {
            let res = Resolver::new(&client, Platform::Jvm).resolve(&roots).unwrap();
            assert_eq!(versions(&res), ["x:1.0", "dep:1.0"]);
            assert_eq!(res.node(&"g:x".parse().unwrap()).unwrap().files.len(), 2);
        }

        // Only the classifier artifact when nothing else needs the main jar, as in Gradle.
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[natives(true)]).unwrap();
        assert_eq!(file_names(&res), ["x-1.0-natives.jar"]);

        // Another module that depends on `x` brings its main jar back.
        repo.publish("g:user", "1.0", &[("g:x", "1.0", "")]);
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[natives(true), req("g:user", "1.0")]).unwrap();
        let mut names = file_names(&res);
        names.sort();
        assert_eq!(names, ["dep-1.0.jar", "user-1.0.jar", "x-1.0-natives.jar", "x-1.0.jar"]);

        // Several classifiers, each once.
        let mut sources = req("g:x", "1.0");
        sources.classifier = Some("sources".into());
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[natives(false), sources.clone(), natives(true), sources]).unwrap();
        let mut names = file_names(&res);
        names.sort();
        assert_eq!(names, ["dep-1.0.jar", "x-1.0-natives.jar", "x-1.0-sources.jar"]);
    }

    #[test]
    fn classifier_roots_share_the_module_version() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[]);
        repo.publish("g:x", "2.0", &[]);
        repo.write("g/x/1.0/x-1.0-natives.jar", "natives 1");
        repo.write("g/x/2.0/x-2.0-natives.jar", "natives 2");
        let client = repo.client();
        let mut natives = req("g:x", "1.0");
        natives.classifier = Some("natives".into());
        // The highest version wins for every classifier of the module.
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[natives.clone(), req("g:x", "2.0")]).unwrap();
        assert_eq!(res.node(&"g:x".parse().unwrap()).unwrap().version, "2.0");
        let mut names = file_names(&res);
        names.sort();
        assert_eq!(names, ["x-2.0-natives.jar", "x-2.0.jar"]);

        // `force` pins them all.
        natives.force = true;
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[natives, req("g:x", "2.0")]).unwrap();
        assert_eq!(res.node(&"g:x".parse().unwrap()).unwrap().version, "1.0");
        let mut names = file_names(&res);
        names.sort();
        assert_eq!(names, ["x-1.0-natives.jar", "x-1.0.jar"]);
    }

    #[test]
    fn missing_module_reports_path() {
        let repo = TestRepo::new();
        repo.publish("g:x", "1.0", &[("g:ghost", "1.0", "")]);
        let client = repo.client();
        let err = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:x", "1.0")]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("g:ghost:1.0"), "{msg}");
    }

    fn publish_kmp(repo: &TestRepo) {
        let marker = "<!-- do_not_remove: published-with-gradle-metadata -->";
        repo.write(
            "k/lib/1.0/lib-1.0.pom",
            &format!("<project>{marker}<groupId>k</groupId><artifactId>lib</artifactId><version>1.0</version></project>"),
        );
        repo.write(
            "k/lib/1.0/lib-1.0.module",
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "jvmRuntimeElements-published", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "jvm"},
               "available-at": {"url": "../../lib-jvm/1.0/lib-jvm-1.0.module", "group": "k", "module": "lib-jvm", "version": "1.0"}},
              {"name": "linuxX64ApiElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "kotlin-api", "org.jetbrains.kotlin.platform.type": "native", "org.jetbrains.kotlin.native.target": "linux_x64"},
               "dependencies": [{"group": "org.jetbrains.kotlin", "module": "kotlin-stdlib", "version": {"requires": "2.0.0"}}],
               "files": [{"name": "lib.klib", "url": "lib-linuxx64-1.0.klib"}]}
            ]}"#,
        );
        repo.write("k/lib/1.0/lib-linuxx64-1.0.klib", "klib");
        repo.write(
            "k/lib-jvm/1.0/lib-jvm-1.0.pom",
            &format!("<project>{marker}<groupId>k</groupId><artifactId>lib-jvm</artifactId><version>1.0</version></project>"),
        );
        repo.write(
            "k/lib-jvm/1.0/lib-jvm-1.0.module",
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "jvmRuntimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime", "org.jetbrains.kotlin.platform.type": "jvm"},
               "dependencies": [{"group": "k", "module": "dep", "version": {"requires": "1.0"}}],
               "files": [{"name": "lib-jvm-1.0.jar", "url": "lib-jvm-1.0.jar"}]}
            ]}"#,
        );
        repo.write("k/lib-jvm/1.0/lib-jvm-1.0.jar", "jar");
        repo.publish("k:dep", "1.0", &[]);
    }

    #[test]
    fn kmp_library_resolves_jvm_variant_via_available_at() {
        let repo = TestRepo::new();
        publish_kmp(&repo);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("k:lib", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["lib:1.0", "lib-jvm:1.0", "dep:1.0"]);
        assert_eq!(file_names(&res), ["lib-jvm-1.0.jar", "dep-1.0.jar"]);
    }

    #[test]
    fn kmp_library_resolves_native_klib_and_skips_stdlib() {
        let repo = TestRepo::new();
        publish_kmp(&repo);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Native("linux_x64".into())).resolve(&[req("k:lib", "1.0")]).unwrap();
        assert_eq!(file_names(&res), ["lib-linuxx64-1.0.klib"]);

        let err = Resolver::new(&client, Platform::Native("mingw_x64".into())).resolve(&[req("k:lib", "1.0")]).unwrap_err();
        assert!(format!("{err:#}").contains("does not publish a variant"), "{err:#}");

        let err = Resolver::new(&client, Platform::Native("linux_x64".into())).resolve(&[req("k:dep", "1.0")]).unwrap_err();
        assert!(format!("{err:#}").contains("JVM-only"), "{err:#}");
    }

    #[test]
    fn wasm_resolves_the_skiko_runtime_jar_and_no_other_jvm_library() {
        let repo = TestRepo::new();
        repo.publish("org.jetbrains.skiko:skiko-js-wasm-runtime", "0.150.1", &[]);
        repo.publish("k:dep", "1.0", &[]);
        let client = repo.client();
        let wasm = Platform::Wasm("js".into());
        let res = Resolver::new(&client, wasm.clone()).resolve(&[req("org.jetbrains.skiko:skiko-js-wasm-runtime", "0.150.1")]).unwrap();
        assert_eq!(file_names(&res), ["skiko-js-wasm-runtime-0.150.1.jar"]);
        let err = Resolver::new(&client, wasm).resolve(&[req("k:dep", "1.0")]).unwrap_err();
        assert!(format!("{err:#}").contains("JVM-only"), "{err:#}");
    }

    #[test]
    fn platform_dependencies_align_versions_without_adding_modules() {
        let repo = TestRepo::new();
        let marker = "<!-- do_not_remove: published-with-gradle-metadata -->";
        repo.write(
            "j/api/2.0/api-2.0.pom",
            &format!("<project>{marker}<groupId>j</groupId><artifactId>api</artifactId><version>2.0</version></project>"),
        );
        repo.write(
            "j/api/2.0/api-2.0.module",
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "runtimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime"},
               "dependencies": [{"group": "j", "module": "bom", "version": {"requires": "2.0"}, "attributes": {"org.gradle.category": "platform"}}],
               "dependencyConstraints": [{"group": "j", "module": "extra", "version": {"requires": "2.0"}}],
               "files": [{"name": "api-2.0.jar", "url": "api-2.0.jar"}]}
            ]}"#,
        );
        repo.write("j/api/2.0/api-2.0.jar", "jar");
        repo.write("j/bom/2.0/bom-2.0.pom", &format!("<project>{marker}<groupId>j</groupId><artifactId>bom</artifactId><version>2.0</version><packaging>pom</packaging></project>"));
        repo.write(
            "j/bom/2.0/bom-2.0.module",
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "apiElements", "attributes": {"org.gradle.category": "platform", "org.gradle.usage": "java-api"},
               "dependencyConstraints": [{"group": "j", "module": "launcher", "version": {"requires": "2.0"}}]}
            ]}"#,
        );
        repo.publish("old:runner", "1.0", &[("j:launcher", "1.0", ""), ("j:extra", "1.0", "")]);
        repo.publish("j:launcher", "1.0", &[]);
        repo.publish("j:launcher", "2.0", &[]);
        repo.publish("j:extra", "1.0", &[]);
        repo.publish("j:extra", "2.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("j:api", "2.0"), req("old:runner", "1.0")]).unwrap();
        assert_eq!(versions(&res), ["api:2.0", "runner:1.0", "launcher:2.0", "extra:2.0"]);
        assert!(res.node(&"j:bom".parse().unwrap()).is_none(), "platforms are not part of the graph");

        // Without anything depending on them, constrained modules are not added.
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("j:api", "2.0")]).unwrap();
        assert_eq!(versions(&res), ["api:2.0"]);
    }

    #[test]
    fn versionless_module_metadata_edges_take_versions_from_elsewhere() {
        let repo = TestRepo::new();
        let marker = "<!-- do_not_remove: published-with-gradle-metadata -->";
        repo.write(
            "m/ui/1.0/ui-1.0.pom",
            &format!("<project>{marker}<groupId>m</groupId><artifactId>ui</artifactId><version>1.0</version></project>"),
        );
        repo.write(
            "m/ui/1.0/ui-1.0.module",
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "runtimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime"},
               "dependencies": [{"group": "m", "module": "std"}],
               "files": [{"name": "ui-1.0.jar", "url": "ui-1.0.jar"}]}
            ]}"#,
        );
        repo.write("m/ui/1.0/ui-1.0.jar", "jar");
        repo.publish("m:std", "3.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("m:ui", "1.0"), req("m:std", "3.0")]).unwrap();
        assert_eq!(versions(&res), ["ui:1.0", "std:3.0"]);

        let err = Resolver::new(&client, Platform::Jvm).resolve(&[req("m:ui", "1.0")]).unwrap_err();
        assert!(err.to_string().contains("without a version"), "{err}");
    }

    #[test]
    fn relocation_is_followed() {
        let repo = TestRepo::new();
        repo.write(
            "old/x/1.0/x-1.0.pom",
            "<project><groupId>old</groupId><artifactId>x</artifactId><version>1.0</version>
             <distributionManagement><relocation><groupId>new</groupId></relocation></distributionManagement></project>",
        );
        repo.publish("new:x", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("old:x", "1.0")]).unwrap();
        assert_eq!(file_names(&res), ["x-1.0.jar"]);
        assert!(res.files()[0].to_string_lossy().replace('\\', "/").contains("new/x/1.0"));
    }

    #[test]
    fn timestamped_snapshots_resolve_to_the_newest_build() {
        let repo = TestRepo::new();
        repo.publish_snapshot("g:lib", "1.0-SNAPSHOT", "20240101.120000-1", &[("g:dep", "1.0-SNAPSHOT", "")]);
        repo.publish_snapshot("g:dep", "1.0-SNAPSHOT", "20240101.130000-4", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:lib", "1.0-SNAPSHOT")]).unwrap();
        assert_eq!(versions(&res), ["lib:1.0-SNAPSHOT", "dep:1.0-SNAPSHOT"]);
        assert_eq!(file_names(&res), ["lib-1.0-20240101.120000-1.jar", "dep-1.0-20240101.130000-4.jar"]);

        // A later deployment replaces the metadata; the next resolution follows it.
        repo.publish_snapshot("g:lib", "1.0-SNAPSHOT", "20240102.080000-2", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:lib", "1.0-SNAPSHOT")]).unwrap();
        assert_eq!(versions(&res), ["lib:1.0-SNAPSHOT"]);
        assert_eq!(file_names(&res), ["lib-1.0-20240102.080000-2.jar"]);
    }

    #[test]
    fn snapshots_stored_under_their_plain_name_resolve() {
        let repo = TestRepo::new();
        repo.publish("g:lib", "2.0-SNAPSHOT", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("g:lib", "2.0-SNAPSHOT")]).unwrap();
        assert_eq!(file_names(&res), ["lib-2.0-SNAPSHOT.jar"]);
    }

    #[test]
    fn timestamped_snapshot_root_extension() {
        let repo = TestRepo::new();
        repo.publish_snapshot("g:lib", "1.0-SNAPSHOT", "20240101.120000-1", &[]);
        let dir = "g/lib/1.0-SNAPSHOT";
        repo.write(&format!("{dir}/lib-1.0-20240101.120000-1.zip"), "zip");
        repo.write_snapshot_metadata(dir, "1.0-20240101.120000-1", &["jar", "pom", "zip"]);
        let mut root = req("g:lib", "1.0-SNAPSHOT");
        root.extension = Some("zip".into());
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[root]).unwrap();
        assert_eq!(file_names(&res), ["lib-1.0-20240101.120000-1.zip"]);
    }

    #[test]
    fn timestamped_snapshot_gradle_metadata_and_parent_poms() {
        let repo = TestRepo::new();
        let marker = "<!-- do_not_remove: published-with-gradle-metadata -->";
        let dir = "s/ui/1.0-SNAPSHOT";
        repo.write(
            &format!("{dir}/ui-1.0-20240101.120000-1.pom"),
            &format!("<project>{marker}<groupId>s</groupId><artifactId>ui</artifactId><version>1.0-SNAPSHOT</version></project>"),
        );
        repo.write(
            &format!("{dir}/ui-1.0-20240101.120000-1.module"),
            r#"{"formatVersion": "1.1", "variants": [
              {"name": "runtimeElements", "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime"},
               "dependencies": [{"group": "s", "module": "core", "version": {"requires": "1.0-SNAPSHOT"}}],
               "files": [{"name": "ui-1.0-SNAPSHOT.jar", "url": "ui-1.0-20240101.120000-1.jar"}]}
            ]}"#,
        );
        repo.write(&format!("{dir}/ui-1.0-20240101.120000-1.jar"), "jar");
        repo.write_snapshot_metadata(dir, "1.0-20240101.120000-1", &["pom", "module", "jar"]);
        // `core` inherits its dependencies from a SNAPSHOT parent.
        repo.write(
            "s/parent/1.0-SNAPSHOT/parent-1.0-20240101.100000-9.pom",
            "<project><groupId>s</groupId><artifactId>parent</artifactId><version>1.0-SNAPSHOT</version><packaging>pom</packaging>
             <dependencies><dependency><groupId>s</groupId><artifactId>inherited</artifactId><version>1.0</version></dependency></dependencies></project>",
        );
        repo.write_snapshot_metadata("s/parent/1.0-SNAPSHOT", "1.0-20240101.100000-9", &["pom"]);
        repo.write(
            "s/core/1.0-SNAPSHOT/core-1.0-20240101.110000-2.pom",
            "<project><parent><groupId>s</groupId><artifactId>parent</artifactId><version>1.0-SNAPSHOT</version></parent><artifactId>core</artifactId><version>1.0-SNAPSHOT</version></project>",
        );
        repo.write("s/core/1.0-SNAPSHOT/core-1.0-20240101.110000-2.jar", "jar");
        repo.write_snapshot_metadata("s/core/1.0-SNAPSHOT", "1.0-20240101.110000-2", &["pom", "jar"]);
        repo.publish("s:inherited", "1.0", &[]);
        let client = repo.client();
        let res = Resolver::new(&client, Platform::Jvm).resolve(&[req("s:ui", "1.0-SNAPSHOT")]).unwrap();
        assert_eq!(versions(&res), ["ui:1.0-SNAPSHOT", "core:1.0-SNAPSHOT", "inherited:1.0"]);
        assert_eq!(file_names(&res), ["ui-1.0-20240101.120000-1.jar", "core-1.0-20240101.110000-2.jar", "inherited-1.0.jar"]);
    }

    #[test]
    fn exclusion_wildcards() {
        let m: ModuleId = "a.b:c".parse().unwrap();
        assert!(Exclusion::new("*", "*").matches(&m));
        assert!(Exclusion::new("a.b", "*").matches(&m));
        assert!(Exclusion::new("*", "c").matches(&m));
        assert!(!Exclusion::new("a", "c").matches(&m));
    }

    #[allow(dead_code)]
    fn _assert_send_sync(_: &Path) {
        fn is_sync<T: Sync>() {}
        is_sync::<MavenClient>();
    }
}
