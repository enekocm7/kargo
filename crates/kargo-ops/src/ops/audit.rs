//! `kargo audit`: checks the packages a project resolves (the Maven modules of every target and the
//! npm packages of Wasm and Kotlin/JS, optionally the build's tools too) against the OSV advisory
//! database ([`crate::advisories`]), and with `--fix` raises the versions in `kargo.toml` to ones that
//! fix what it finds.
//!
//! This is the one command that contacts the advisory database; nothing else does.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Configuration;
use super::context::BuildContext;
use crate::advisories::{Advisory, Ecosystem, Osv, Query};
use crate::lockfile::LockedResolution;
use kargo_manifest::edit::ManifestEditor;
use kargo_manifest::{AuditIgnore, DenyLevel, TargetKind};
use kargo_maven::{ModuleId, Resolution, Version, VersionReq};

#[derive(Debug, Clone, Default)]
pub struct AuditOptions {
    /// Only these targets; every enabled one when empty.
    pub targets: Vec<TargetKind>,
    /// Also the build's tools: what `kargo.lock` holds for the Kotlin compiler, KSP, the JUnit
    /// launcher, compiler plugins and `[[generate]]` tools, and the npm packages of the TypeScript compiler.
    pub tooling: bool,
}

/// A package version that was audited, and where the project gets it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub query: Query,
    /// The shortest chain of dependencies from the project to the package, the package last:
    /// `group:artifact:version` for a Maven module, `name@version` for an npm package.
    pub path: Vec<String>,
    /// The resolutions it is part of, such as `jvm/runtime`, `wasm-js/test` or `tools/kotlin-compiler`.
    pub resolutions: Vec<String>,
    /// Whether only the build's tools use it.
    pub tooling: bool,
}

/// An advisory that affects an audited package.
#[derive(Debug, Clone)]
pub struct Finding {
    pub subject: Subject,
    pub advisory: Advisory,
    /// The `[audit] ignore` entry that covers it (one that has not expired).
    pub ignored: Option<AuditIgnore>,
}

/// The result of auditing one package.
#[derive(Debug, Clone)]
pub struct Report {
    pub package: String,
    pub version: String,
    /// The OSV server asked.
    pub database: String,
    /// How many package versions were checked.
    pub audited: usize,
    /// Most severe first.
    pub findings: Vec<Finding>,
    /// Why some packages could not be checked.
    pub skipped: Vec<String>,
}

impl Report {
    /// The findings that are not ignored.
    pub fn reported(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.ignored.is_none())
    }

    /// The findings that are not ignored and that `deny` fails on.
    pub fn denied(&self, deny: DenyLevel) -> impl Iterator<Item = &Finding> {
        self.reported().filter(move |f| deny.denies(f.advisory.severity))
    }
}

/// `kargo audit` for one package: collects what it resolves and asks OSV about every version.
pub fn audit(ctx: &BuildContext, opts: &AuditOptions) -> Result<Report> {
    let (subjects, skipped) = collect(ctx, opts)?;
    let osv = Osv::new(ctx.config);
    let pkg = &ctx.ws.manifest().package;
    ctx.shell().status("Auditing", format!("{} v{} ({} packages, advisories from {})", pkg.name, pkg.version, subjects.len(), osv.url()));
    let queries: Vec<Query> = subjects.iter().map(|s| s.query.clone()).collect();
    let advisories = osv.advisories(&queries)?;

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let today = &crate::generate::rfc3339(now)[..10];
    let ignores = &ctx.ws.manifest().audit.ignore;
    let mut used = vec![false; ignores.len()];
    let mut findings = Vec::new();
    for (subject, found) in subjects.iter().zip(advisories) {
        for advisory in found {
            let ignored = ignores.iter().position(|i| advisory.is_named(&i.id)).and_then(|i| {
                used[i] = true;
                let entry = &ignores[i];
                (entry.until.as_deref().is_none_or(|until| until >= today)).then(|| entry.clone())
            });
            findings.push(Finding { subject: subject.clone(), advisory, ignored });
        }
    }
    for (entry, used) in ignores.iter().zip(used) {
        match entry.until.as_deref() {
            Some(until) if used && until < today => {
                ctx.shell().warn(format!("[audit] ignore entry `{}` expired on {until}; the advisory is reported again", entry.id))
            }
            _ if !used && opts.targets.is_empty() => ctx.shell().note(format!(
                "[audit] ignore entry `{}` matches no advisory of the audited packages{}; remove it if it is no longer needed",
                entry.id,
                if opts.tooling { "" } else { " (without --tooling)" }
            )),
            _ => {}
        }
    }
    findings.sort_by(|a, b| {
        b.advisory
            .severity
            .cmp(&a.advisory.severity)
            .then_with(|| a.subject.query.cmp(&b.subject.query))
            .then_with(|| a.advisory.id.cmp(&b.advisory.id))
    });
    Ok(Report {
        package: pkg.name.clone(),
        version: pkg.version.clone(),
        database: osv.url().to_owned(),
        audited: subjects.len(),
        findings,
        skipped,
    })
}

/// The package versions to audit and the notes about the ones that cannot be.
pub fn collect(ctx: &BuildContext, opts: &AuditOptions) -> Result<(Vec<Subject>, Vec<String>)> {
    let manifest = ctx.ws.manifest();
    let targets = if opts.targets.is_empty() { manifest.targets.enabled() } else { opts.targets.clone() };
    let mut subjects = Collector::default();
    let mut skipped = Vec::new();
    for target in targets {
        if target == TargetKind::Android {
            skipped.push("the dependencies of the Android target are resolved by Gradle and are not audited".to_owned());
            continue;
        }
        let configurations: &[Configuration] = match target {
            TargetKind::Jvm => &[Configuration::Compile, Configuration::Runtime, Configuration::Test],
            _ => &[Configuration::Runtime, Configuration::Test],
        };
        for &configuration in configurations {
            for (label, resolution) in super::resolution_for(ctx, target, configuration)? {
                let name = match (target, configuration) {
                    (_, Configuration::Test) => format!("{label}/test"),
                    (TargetKind::Jvm, Configuration::Compile) => format!("{label}/compile"),
                    (TargetKind::Jvm, _) => format!("{label}/runtime"),
                    _ => format!("{label}/main"),
                };
                let graph = Graph::from_resolution(&resolution);
                subjects.add_graph(&graph, &name, false);
                if matches!(target, TargetKind::Wasm | TargetKind::Js) {
                    npm_subjects(ctx, target, &resolution, &graph, &name, &mut subjects, &mut skipped)?;
                }
            }
        }
    }
    if opts.tooling {
        for (key, locked) in ctx.locked_tools() {
            subjects.add_graph(&Graph::from_locked(&locked), &key, true);
        }
        let installed = installed_npm(&ctx.ws.work_dir("typescript").join("node_modules"));
        add_npm(&mut subjects, &BTreeMap::new(), &installed, "npm-tools/typescript", true, &mut skipped);
    }
    skipped.sort();
    skipped.dedup();
    Ok((subjects.subjects.into_values().collect(), skipped))
}

#[derive(Default)]
struct Collector {
    subjects: BTreeMap<Query, Subject>,
}

impl Collector {
    fn add(&mut self, query: Query, path: Vec<String>, resolution: &str, tooling: bool) {
        let subject =
            self.subjects.entry(query.clone()).or_insert_with(|| Subject { query, path: path.clone(), resolutions: Vec::new(), tooling });
        // A path through the project's own dependencies says more than one through a tool.
        if subject.tooling && !tooling {
            subject.path = path;
            subject.tooling = false;
        }
        if !subject.resolutions.iter().any(|r| r == resolution) {
            subject.resolutions.push(resolution.to_owned());
        }
    }

    fn add_graph(&mut self, graph: &Graph, resolution: &str, tooling: bool) {
        for (module, path) in graph.paths() {
            let query = Query { ecosystem: Ecosystem::Maven, name: module.to_owned(), version: graph.nodes[module].0.clone() };
            self.add(query, path, resolution, tooling);
        }
    }
}

/// A resolved dependency graph keyed by `group:artifact`, from a resolution or a `kargo.lock` entry.
struct Graph {
    roots: Vec<String>,
    /// The version and the dependencies of each module.
    nodes: BTreeMap<String, (String, Vec<String>)>,
}

impl Graph {
    fn from_resolution(resolution: &Resolution) -> Self {
        Self {
            roots: resolution.roots.iter().map(ToString::to_string).collect(),
            nodes: resolution
                .nodes
                .iter()
                .map(|n| (n.module.to_string(), (n.version.clone(), n.dependencies.iter().map(|(m, _)| m.to_string()).collect())))
                .collect(),
        }
    }

    fn from_locked(locked: &LockedResolution) -> Self {
        // Locked dependencies are `group:artifact[:requested-version]`.
        let module = |d: &str| d.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
        Self {
            roots: locked.roots.clone(),
            nodes: locked
                .packages
                .iter()
                .map(|p| (p.module.clone(), (p.version.clone(), p.dependencies.iter().map(|d| module(d)).collect())))
                .collect(),
        }
    }

    /// The shortest path from a root to each module, as `group:artifact:version` steps.
    fn paths(&self) -> BTreeMap<&str, Vec<String>> {
        let mut parent: HashMap<&str, Option<&str>> = HashMap::new();
        let mut queue = VecDeque::new();
        for root in &self.roots {
            if self.nodes.contains_key(root) && !parent.contains_key(root.as_str()) {
                parent.insert(root, None);
                queue.push_back(root.as_str());
            }
        }
        while let Some(module) = queue.pop_front() {
            for dep in &self.nodes[module].1 {
                if self.nodes.contains_key(dep) && !parent.contains_key(dep.as_str()) {
                    parent.insert(dep, Some(module));
                    queue.push_back(dep);
                }
            }
        }
        self.nodes
            .keys()
            .map(|module| {
                let mut path = Vec::new();
                let mut current = Some(module.as_str());
                while let Some(m) = current
                    && path.len() <= self.nodes.len()
                {
                    path.push(format!("{m}:{}", self.nodes[m].0));
                    current = parent.get(m).copied().flatten();
                }
                path.reverse();
                (module.as_str(), path)
            })
            .collect()
    }
}

/// An npm package found in a `node_modules` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NpmPackage {
    name: String,
    version: String,
    /// The packages it depends on, by name.
    dependencies: Vec<String>,
}

/// The npm packages of `target`: the ones installed next to the build output (with the ones they pull in),
/// else the versions the libraries and `[target.<target>.npm-dependencies]` ask for, when exact.
fn npm_subjects(
    ctx: &BuildContext,
    target: TargetKind,
    resolution: &Resolution,
    graph: &Graph,
    name: &str,
    subjects: &mut Collector,
    skipped: &mut Vec<String>,
) -> Result<()> {
    let paths = graph.paths();
    // Who asks for each package: a library (from its klib) or kargo.toml, which wins.
    let mut asked: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for node in &resolution.nodes {
        for (package, spec) in kargo_compiler::wasm::npm_dependencies(&node.files)? {
            let path = paths.get(node.module.to_string().as_str()).cloned().unwrap_or_default();
            asked.entry(package).or_insert((spec, path));
        }
    }
    let manifest = ctx.ws.manifest();
    let own = match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref().map(|w| &w.npm_dependencies),
        _ => manifest.targets.js.as_ref().map(|j| &j.npm_dependencies),
    };
    for (package, spec) in own.into_iter().flatten() {
        asked.insert(package.clone(), (spec.clone(), Vec::new()));
    }
    if asked.is_empty() {
        return Ok(());
    }
    let installed = installed_npm(&crate::npm::install_dir(ctx.ws, &ctx.profile, target).join("node_modules"));
    add_npm(subjects, &asked, &installed, name, false, skipped);
    Ok(())
}

/// Adds the `installed` npm packages, each with the path of the package that was `asked` for and
/// pulls it in, and the asked-for packages that are not installed when their version is exact.
fn add_npm(
    subjects: &mut Collector,
    asked: &BTreeMap<String, (String, Vec<String>)>,
    installed: &[NpmPackage],
    resolution: &str,
    tooling: bool,
    skipped: &mut Vec<String>,
) {
    let versions: HashMap<&str, &str> = installed.iter().rev().map(|p| (p.name.as_str(), p.version.as_str())).collect();
    let mut required_by: HashMap<&str, &str> = HashMap::new();
    for package in installed {
        for dep in &package.dependencies {
            required_by.entry(dep.as_str()).or_insert(package.name.as_str());
        }
    }
    for package in installed {
        let mut chain = vec![format!("{}@{}", package.name, package.version)];
        let mut current = package.name.as_str();
        let mut base = Vec::new();
        loop {
            if let Some((_, path)) = asked.get(current) {
                base = path.clone();
                break;
            }
            match required_by.get(current) {
                Some(&parent) if chain.len() < 64 && parent != current => {
                    chain.push(format!("{parent}@{}", versions.get(parent).unwrap_or(&"?")));
                    current = parent;
                }
                _ => break,
            }
        }
        chain.reverse();
        base.extend(chain);
        let query = Query { ecosystem: Ecosystem::Npm, name: package.name.clone(), version: package.version.clone() };
        subjects.add(query, base, resolution, tooling);
    }
    for (name, (spec, path)) in asked {
        if versions.contains_key(name.as_str()) {
            continue;
        }
        match exact_version(spec) {
            Some(version) => {
                let mut path = path.clone();
                path.push(format!("{name}@{version}"));
                let query = Query { ecosystem: Ecosystem::Npm, name: name.clone(), version: version.to_owned() };
                subjects.add(query, path, resolution, tooling);
            }
            None => skipped.push(format!(
                "npm package `{name}` {spec} is not installed, so its version is unknown; build the target once to install it, then audit again"
            )),
        }
    }
}

/// The version of an npm version spec that names exactly one, such as `1.2.3` or `=1.2.3-beta.1`.
fn exact_version(spec: &str) -> Option<&str> {
    let version = spec.trim().trim_start_matches(['=', 'v']);
    let core = version.split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    (parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))).then_some(version)
}

/// Every package installed under `node_modules`, in the layouts of npm and bun (nested `node_modules`)
/// and pnpm (`node_modules/.pnpm`); each name and version once.
fn installed_npm(node_modules: &Path) -> Vec<NpmPackage> {
    struct Scan {
        out: Vec<NpmPackage>,
        dirs: HashSet<PathBuf>,
        seen: HashSet<(String, String)>,
    }
    impl Scan {
        fn node_modules(&mut self, dir: &Path, depth: usize) {
            if depth > 32 || !dir.canonicalize().is_ok_and(|d| self.dirs.insert(d)) {
                return;
            }
            for path in subdirs(dir) {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if name == ".pnpm" {
                    for store in subdirs(&path) {
                        self.node_modules(&store.join("node_modules"), depth + 1);
                    }
                } else if name.starts_with('@') {
                    for package in subdirs(&path) {
                        self.package(&package, depth);
                    }
                } else if !name.starts_with('.') {
                    self.package(&path, depth);
                }
            }
        }

        fn package(&mut self, dir: &Path, depth: usize) {
            let manifest = std::fs::read_to_string(dir.join("package.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
            if let Some(manifest) = manifest
                && let (Some(name), Some(version)) =
                    (manifest.get("name").and_then(Value::as_str), manifest.get("version").and_then(Value::as_str))
                && self.seen.insert((name.to_owned(), version.to_owned()))
            {
                let dependencies = ["dependencies", "optionalDependencies", "peerDependencies"]
                    .iter()
                    .filter_map(|key| manifest.get(*key).and_then(Value::as_object))
                    .flat_map(|deps| deps.keys().cloned())
                    .collect();
                self.out.push(NpmPackage { name: name.to_owned(), version: version.to_owned(), dependencies });
            }
            let nested = dir.join("node_modules");
            if nested.is_dir() {
                self.node_modules(&nested, depth + 1);
            }
        }
    }
    fn subdirs(dir: &Path) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> =
            std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
        dirs.sort();
        dirs
    }
    let mut scan = Scan { out: Vec::new(), dirs: HashSet::new(), seen: HashSet::new() };
    if node_modules.is_dir() {
        scan.node_modules(node_modules, 0);
    }
    scan.out
}

/// How a package is named in the output: `group:artifact`, or the npm package with `(npm)`.
fn display_name(query: &Query) -> String {
    match query.ecosystem {
        Ecosystem::Maven => query.name.clone(),
        Ecosystem::Npm => format!("{} (npm)", query.name),
    }
}

fn severity_cell(advisory: &Advisory) -> String {
    match (advisory.severity, advisory.score) {
        (Some(s), Some(score)) => format!("{s} ({score:.1})"),
        (Some(s), None) => s.to_string(),
        (None, _) => "unknown".to_owned(),
    }
}

/// Renders the advisories of a report that are not ignored: a table by severity, then the details
/// of each (aliases, link, dependency path).
pub fn render(report: &Report) -> String {
    let reported: Vec<&Finding> = report.reported().collect();
    let mut out = String::new();
    if reported.is_empty() {
        return out;
    }
    let mut table: Vec<[String; 5]> = vec![["severity", "package", "version", "advisory", "fixed in"].map(str::to_owned)];
    for f in &reported {
        table.push([
            severity_cell(&f.advisory),
            display_name(&f.subject.query),
            f.subject.query.version.clone(),
            f.advisory.id.clone(),
            f.advisory.fixed_after(&f.subject.query.version).unwrap_or("-").to_owned(),
        ]);
    }
    let widths: Vec<usize> = (0..5).map(|i| table.iter().map(|row| row[i].len()).max().unwrap_or(0)).collect();
    for row in &table {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(cell, w)| format!("{cell:w$}")).collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    for f in &reported {
        let a = &f.advisory;
        let summary = if a.summary.is_empty() { String::new() } else { format!(": {}", a.summary) };
        let _ = writeln!(out, "\n{}{summary}", a.id);
        if !a.aliases.is_empty() {
            let _ = writeln!(out, "  aliases:  {}", a.aliases.join(", "));
        }
        let _ = writeln!(out, "  url:      {}", a.url);
        let mut path = vec![format!("{} v{}", report.package, report.version)];
        path.extend(f.subject.path.iter().cloned());
        let _ = writeln!(out, "  path:     {}", path.join(" -> "));
        let tooling = if f.subject.tooling { " (build tooling)" } else { "" };
        let _ = writeln!(out, "  found in: {}{tooling}", f.subject.resolutions.join(", "));
    }
    out
}

/// Describes a report for `kargo audit --format json`.
pub fn json(report: &Report, deny: DenyLevel) -> Value {
    let advisories: Vec<Value> = report
        .findings
        .iter()
        .map(|f| {
            let a = &f.advisory;
            json!({
                "id": a.id,
                "aliases": a.aliases,
                "summary": a.summary,
                "severity": a.severity.map(|s| s.as_str()),
                "score": a.score,
                "url": a.url,
                "ecosystem": f.subject.query.ecosystem.as_str(),
                "package": f.subject.query.name,
                "version": f.subject.query.version,
                "fixed": a.fixed_after(&f.subject.query.version),
                "path": f.subject.path,
                "resolutions": f.subject.resolutions,
                "tooling": f.subject.tooling,
                "ignored": f.ignored.as_ref().map(|i| json!({ "reason": i.reason, "until": i.until })),
                "denied": f.ignored.is_none() && deny.denies(a.severity),
            })
        })
        .collect();
    json!({
        "package": { "name": report.package, "version": report.version },
        "database": report.database,
        "deny": deny.as_str(),
        "audited": report.audited,
        "advisories": advisories,
        "skipped": report.skipped,
    })
}

/// `kargo audit --fix`: raises the versions in `kargo.toml` of the vulnerable packages it declares to
/// the lowest version that fixes all their advisories, when that version is compatible with the
/// declared one (as `kargo upgrade` would). For packages it does not declare, says which dependency
/// brings them in. Returns how many versions were changed.
pub fn fix(ctx: &BuildContext, report: &Report) -> Result<usize> {
    let shell = ctx.shell();
    // The version each vulnerable package needs, with its current version and path.
    let mut wanted: BTreeMap<(Ecosystem, String), (String, String, Vec<String>)> = BTreeMap::new();
    for f in report.reported() {
        let q = &f.subject.query;
        let Some(fixed) = f.advisory.fixed_after(&q.version) else {
            shell.note(format!("{} has no fixed version of `{}` {} yet", f.advisory.id, q.name, q.version));
            continue;
        };
        let entry =
            wanted.entry((q.ecosystem, q.name.clone())).or_insert_with(|| (q.version.clone(), fixed.to_owned(), f.subject.path.clone()));
        if Version::parse(fixed) > Version::parse(&entry.1) {
            entry.1 = fixed.to_owned();
        }
    }
    let manifest = ctx.ws.manifest();
    let mut editor = ManifestEditor::load(&ctx.ws.manifest_path())?;
    let mut changed = 0;
    for ((ecosystem, name), (current, fixed, path)) in wanted {
        // The first step of the path is the dependency of the project that brings the package in.
        let via = path.first().filter(|_| path.len() > 1);
        match ecosystem {
            Ecosystem::Maven => {
                let module: ModuleId = name.parse()?;
                let declared: Vec<_> = super::upgrade::declared(manifest).into_iter().filter(|(_, d)| d.module == module).collect();
                if declared.is_empty() {
                    match via {
                        Some(via) => shell.note(format!(
                            "`{name}` {current} comes from `{via}`; upgrade that to a version that uses {name} {fixed} or later, or declare \"{name}\" = \"{fixed}\" in [dependencies] to override it"
                        )),
                        None => shell.note(format!("`{name}` {current} is fixed in {fixed}, but kargo.toml does not declare it")),
                    }
                    continue;
                }
                for (section, dep) in declared {
                    if editor.is_inherited(section, &dep.name) {
                        shell.note(format!(
                            "`{}` is inherited from [workspace.dependencies]; raise it to {fixed} in the workspace's kargo.toml",
                            dep.name
                        ));
                        continue;
                    }
                    let Some(raw) = &dep.version else {
                        shell.note(format!(
                            "the version of `{}` is managed by a BOM; upgrade the BOM to one with {name} {fixed} or later",
                            dep.name
                        ));
                        continue;
                    };
                    let Ok(VersionReq::Soft(declared)) = raw.parse::<VersionReq>() else {
                        shell.note(format!("`{}` is declared as `{raw}`; change it to {fixed} or later in kargo.toml", dep.name));
                        continue;
                    };
                    let target = Version::parse(&fixed);
                    if declared >= target {
                        continue;
                    }
                    if !declared.is_compatible_with(&target) {
                        shell.note(format!(
                            "`{}` is fixed in {fixed}, which is not compatible with {raw}; change it in kargo.toml after reading its release notes",
                            dep.name
                        ));
                        continue;
                    }
                    editor.set_dependency_version(section, &module, &fixed)?;
                    shell.status("Fixing", format!("{} v{raw} -> v{fixed} in [{}]", dep.name, section.display()));
                    changed += 1;
                }
            }
            Ecosystem::Npm => {
                let own = [
                    (TargetKind::Wasm, manifest.targets.wasm.as_ref().map(|w| &w.npm_dependencies)),
                    (TargetKind::Js, manifest.targets.js.as_ref().map(|j| &j.npm_dependencies)),
                ];
                let declared: Vec<(TargetKind, &String)> =
                    own.into_iter().filter_map(|(kind, deps)| Some((kind, deps?.get(&name)?))).collect();
                if declared.is_empty() {
                    match via {
                        Some(via) => shell.note(format!(
                            "npm package `{name}` {current} comes from `{via}`; upgrade that to a version that uses {name} {fixed} or later"
                        )),
                        None => {
                            shell.note(format!("npm package `{name}` {current} is fixed in {fixed}, but kargo.toml does not declare it"))
                        }
                    }
                    continue;
                }
                for (kind, spec) in declared {
                    // Keep a caret or tilde range a range, starting at the fix.
                    let prefix: String = spec.trim().chars().take_while(|c| matches!(c, '^' | '~')).collect();
                    let Some(version) = exact_version(&spec.trim()[prefix.len()..]) else {
                        shell.note(format!("npm package `{name}` is declared as `{spec}`; change it to {fixed} or later in kargo.toml"));
                        continue;
                    };
                    if Version::parse(version) >= Version::parse(&fixed) {
                        continue;
                    }
                    let new = format!("{prefix}{fixed}");
                    editor.set_npm_dependency_version(kind, &name, &new)?;
                    shell.status("Fixing", format!("npm package {name} {spec} -> {new} in [target.{kind}.npm-dependencies]"));
                    changed += 1;
                }
            }
        }
    }
    if changed > 0 {
        ctx.ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
        editor.save()?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_maven::resolver::ResolvedNode;

    fn node(m: &str, v: &str, deps: &[&str]) -> ResolvedNode {
        ResolvedNode {
            module: m.parse().unwrap(),
            version: v.into(),
            files: vec![],
            dependencies: deps.iter().map(|d| (d.parse().unwrap(), None)).collect(),
        }
    }

    #[test]
    fn finds_the_shortest_path_to_each_module() {
        let resolution = Resolution {
            roots: vec!["g:app".parse().unwrap(), "g:widget".parse().unwrap()],
            nodes: vec![
                node("g:app", "1", &["g:core"]),
                node("g:core", "2", &["g:leaf"]),
                node("g:widget", "3", &["g:leaf"]),
                node("g:leaf", "4", &[]),
            ],
        };
        let graph = Graph::from_resolution(&resolution);
        let paths = graph.paths();
        assert_eq!(paths["g:app"], ["g:app:1"]);
        assert_eq!(paths["g:core"], ["g:app:1", "g:core:2"]);
        assert_eq!(paths["g:leaf"], ["g:widget:3", "g:leaf:4"], "the shortest path");

        let locked = LockedResolution {
            inputs: String::new(),
            roots: vec!["g:tool".into()],
            packages: vec![
                crate::lockfile::LockedPackage {
                    module: "g:tool".into(),
                    version: "1".into(),
                    dependencies: vec!["g:dep:[1,2)".into()],
                    files: vec![],
                },
                crate::lockfile::LockedPackage { module: "g:dep".into(), version: "1.5".into(), dependencies: vec![], files: vec![] },
            ],
        };
        assert_eq!(Graph::from_locked(&locked).paths()["g:dep"], ["g:tool:1", "g:dep:1.5"]);
    }

    #[test]
    fn exact_npm_versions() {
        assert_eq!(exact_version("1.2.3"), Some("1.2.3"));
        assert_eq!(exact_version("=1.2.3-beta.1"), Some("1.2.3-beta.1"));
        assert_eq!(exact_version("v2.0.0"), Some("2.0.0"));
        for range in ["^1.2.3", "~1.2", "1.x", ">=1.0.0", "*", "latest", "1.2"] {
            assert_eq!(exact_version(range), None, "{range}");
        }
    }

    #[test]
    fn reads_installed_npm_packages() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        let write = |rel: &str, json: &str| {
            let d = nm.join(rel);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("package.json"), json).unwrap();
        };
        write("@js-joda/core", r#"{"name":"@js-joda/core","version":"3.2.0","dependencies":{"left-pad":"^1"}}"#);
        write("left-pad", r#"{"name":"left-pad","version":"1.3.0"}"#);
        write("left-pad/node_modules/nested", r#"{"name":"nested","version":"0.1.0"}"#);
        write(".pnpm/ws@8.0.0/node_modules/ws", r#"{"name":"ws","version":"8.0.0"}"#);
        std::fs::create_dir_all(nm.join(".bin")).unwrap();
        let mut found: Vec<(String, String)> = installed_npm(&nm).into_iter().map(|p| (p.name, p.version)).collect();
        found.sort();
        let expected = [("@js-joda/core", "3.2.0"), ("left-pad", "1.3.0"), ("nested", "0.1.0"), ("ws", "8.0.0")];
        assert_eq!(found, expected.map(|(n, v)| (n.to_owned(), v.to_owned())));

        let mut subjects = Collector::default();
        let mut asked = BTreeMap::new();
        asked.insert("@js-joda/core".to_owned(), ("3.2.0".to_owned(), vec!["org.jetbrains.kotlinx:kotlinx-datetime:0.6.0".to_owned()]));
        asked.insert("missing".to_owned(), ("^2.0.0".to_owned(), Vec::new()));
        asked.insert("pinned".to_owned(), ("2.0.0".to_owned(), Vec::new()));
        let mut skipped = Vec::new();
        add_npm(&mut subjects, &asked, &installed_npm(&nm), "wasm-js/main", false, &mut skipped);
        let pad = subjects.subjects.values().find(|s| s.query.name == "left-pad").unwrap();
        assert_eq!(pad.path, ["org.jetbrains.kotlinx:kotlinx-datetime:0.6.0", "@js-joda/core@3.2.0", "left-pad@1.3.0"]);
        assert!(subjects.subjects.values().any(|s| s.query.name == "pinned" && s.query.version == "2.0.0"));
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].contains("`missing` ^2.0.0 is not installed"), "{skipped:?}");
    }
}
