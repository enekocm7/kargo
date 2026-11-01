//! `kargo tidy`: finds the dependencies of `kargo.toml` that no source uses, and with `--fix`
//! removes them.
//!
//! A Maven module is used when a source mentions a package of its jar (an `import`, a star import or
//! a fully qualified name), so the answer comes from the resolution a build uses and needs no
//! compiler. A `path` dependency is used when a source mentions a package that its own sources
//! declare. With `--npm`, an npm package is used when a Kotlin source mentions its generated package
//! or a JavaScript or TypeScript source names the module.
//!
//! The check is conservative: it never reports dependencies that are only on the runtime classpath,
//! `provided`, annotation or KSP processors, BOMs, `force`d versions, native libraries
//! (`classifier`), `exported` path dependencies or what `[frameworks]` injects, and it cannot judge
//! the ones whose jar it cannot read .

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Configuration;
use super::context::BuildContext;
use super::inspect::{self, Visibility};
use crate::workspace::Workspace;
use kargo_manifest::edit::{DependencySection, ManifestEditor};
use kargo_manifest::{Scope, TargetKind};
use kargo_maven::{ModuleId, Resolution};
use kargo_util::fs::collect_files;

const SOURCE_EXTENSIONS: &[&str] = &["kt", "java"];
const JS_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];

#[derive(Debug, Clone, Copy, Default)]
pub struct TidyOptions {
    /// Also check the npm packages of Wasm and Kotlin/JS.
    pub npm: bool,
    /// Also report the dependencies that only Kotlin scripts use.
    pub strict: bool,
}

/// Why a dependency is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No source uses it.
    Unused,
    /// Only tests use it, but it is declared where the main code gets it.
    DevOnly,
}

/// Where a reported dependency is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A Maven module or a `path` dependency, in this table.
    Table(DependencySection),
    /// An npm package of this target; `dev` for `dev-npm-dependencies`.
    Npm(TargetKind, bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub origin: Origin,
    /// The key in `kargo.toml`.
    pub name: String,
    /// `group:artifact`, `path` or `npm`.
    pub kind: &'static str,
    pub verdict: Verdict,
}

impl Finding {
    /// The table as written in `kargo.toml`.
    pub fn table(&self) -> String {
        match &self.origin {
            Origin::Table(section) => section.display(),
            Origin::Npm(kind, dev) => {
                format!("target.{}.{}", kind.as_str(), if *dev { "dev-npm-dependencies" } else { "npm-dependencies" })
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub package: String,
    pub version: String,
    /// How many dependencies were examined.
    pub checked: usize,
    pub findings: Vec<Finding>,
    /// Dependencies that could not be judged, and why.
    pub notes: Vec<String>,
}

impl Report {
    pub fn unused(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.verdict == Verdict::Unused)
    }

    pub fn dev_only(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.verdict == Verdict::DevOnly)
    }
}

/// The dotted names that sources mention, with every prefix of each: `kotlinx.coroutines.flow.map`
/// adds `kotlinx`, `kotlinx.coroutines`, `kotlinx.coroutines.flow` and the whole name.
#[derive(Debug, Default)]
struct Names(HashSet<String>);

impl Names {
    fn add(&mut self, text: &str) {
        let mut segments: Vec<&str> = Vec::new();
        let mut start: Option<usize> = None;
        let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        for (i, c) in text.char_indices().chain(std::iter::once((text.len(), '\n'))) {
            if is_ident(c) {
                start.get_or_insert(i);
                continue;
            }
            let word = start.take().map(|s| &text[s..i]);
            match (word, c) {
                (Some(w), '.') => segments.push(w),
                (Some(w), _) => {
                    segments.push(w);
                    self.flush(&mut segments);
                }
                // `..` or a leading dot ends the chain.
                (None, _) => self.flush(&mut segments),
            }
        }
    }

    fn flush(&mut self, segments: &mut Vec<&str>) {
        let mut name = String::new();
        for segment in segments.iter().take(16) {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(segment);
            self.0.insert(name.clone());
        }
        segments.clear();
    }

    fn add_files(&mut self, files: &BTreeSet<PathBuf>) {
        for file in files {
            if let Ok(text) = std::fs::read_to_string(file) {
                self.add(&text);
            }
        }
    }

    fn mentions_any(&self, packages: &BTreeSet<String>) -> bool {
        packages.iter().any(|p| self.0.contains(p))
    }
}

/// The source files of a package, split by what they belong to.
#[derive(Default)]
struct Sources {
    main: BTreeSet<PathBuf>,
    tests: BTreeSet<PathBuf>,
    scripts: BTreeSet<PathBuf>,
}

fn gather(ctx: &BuildContext) -> Sources {
    let ws = ctx.ws;
    let manifest = ws.manifest();
    let mut out = Sources::default();
    let add = |into: &mut BTreeSet<PathBuf>, dirs: Vec<PathBuf>| {
        for dir in dirs {
            into.extend(collect_files(&dir, SOURCE_EXTENSIONS));
        }
    };
    for target in manifest.targets.enabled() {
        add(&mut out.main, ws.source_dirs(target));
        out.main.extend(crate::generate::sources(ws, &ctx.profile, target, false, SOURCE_EXTENSIONS));
        let mut tests = ws.test_dirs(target);
        tests.extend(ws.test_fixture_dirs(target));
        tests.extend(ws.bench_dirs(target));
        for suite in manifest.test.suites.values() {
            tests.extend(ws.suite_test_dirs(target, Some(suite)));
        }
        add(&mut out.tests, tests);
        out.tests.extend(crate::generate::sources(ws, &ctx.profile, target, true, SOURCE_EXTENSIONS));
    }
    if manifest.targets.is_enabled(TargetKind::Android) {
        add(&mut out.main, vec![ws.android_dir()]);
    }
    // Scripts: `.kts` files in the project directory and the ones tasks and plugins run.
    if let Ok(entries) = std::fs::read_dir(ws.root()) {
        out.scripts.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "kts")));
    }
    if let Ok(text) = std::fs::read_to_string(ws.manifest_path()) {
        out.scripts.extend(kargo_manifest::script::manifest_scripts(&text).into_iter().map(|p| ws.root().join(p)).filter(|p| p.is_file()));
    }
    out
}

/// The packages the classes of `jar` are in.
fn jar_packages(jar: &Path) -> Result<BTreeSet<String>> {
    Ok(inspect::classes(jar, Visibility::Package)?
        .iter()
        .filter_map(|class| class.rsplit_once('/'))
        .map(|(package, _)| package.replace('/', "."))
        .collect())
}

/// The jar (JVM) or klib of `module` in `resolution`; a module that only redirects to platform
/// modules (`name-jvm`, `name-wasm-js`) answers with theirs.
fn artifact_of(resolution: &Resolution, module: &ModuleId, target: TargetKind) -> Option<PathBuf> {
    let extension = if target == TargetKind::Jvm { "jar" } else { "klib" };
    let find = |files: &[PathBuf]| files.iter().find(|f| f.extension().is_some_and(|e| e == extension)).cloned();
    let node = resolution.node(module)?;
    find(&node.files).or_else(|| {
        let prefix = format!("{}-", node.module.artifact);
        node.dependencies
            .iter()
            .filter(|(m, _)| m.group == node.module.group && m.artifact.starts_with(&prefix))
            .find_map(|(m, _)| resolution.node(m).and_then(|n| find(&n.files)))
    })
}

/// The packages a klib (a zip file or an unpacked directory) holds: `default/linkdata/package_<name>/`.
fn klib_packages(klib: &Path) -> Result<BTreeSet<String>> {
    let package_of = |path: &str| -> Option<String> {
        let rest = path.strip_prefix("default/linkdata/package_")?;
        rest.split_once('/').map(|(package, _)| package.to_owned()).filter(|p| !p.is_empty())
    };
    let mut out = BTreeSet::new();
    if klib.is_dir() {
        for path in collect_files(&klib.join("default").join("linkdata"), &["knm"]) {
            let rel = path.strip_prefix(klib).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.extend(package_of(&rel));
        }
    } else {
        let file = std::fs::File::open(klib).with_context(|| format!("failed to open {}", klib.display()))?;
        let mut zip = zip::ZipArchive::new(file).with_context(|| format!("{} is not a valid klib", klib.display()))?;
        for i in 0..zip.len() {
            let entry = zip.by_index(i).with_context(|| format!("failed to read {}", klib.display()))?;
            out.extend(entry.name().ok().and_then(|n| package_of(&n)));
        }
    }
    Ok(out)
}

/// The packages that the sources of the package at `root` declare.
fn declared_packages(root: &Path) -> Result<BTreeSet<String>> {
    let dep = Workspace::load(&root.join("kargo.toml"))?;
    let mut files = BTreeSet::new();
    for target in TargetKind::ALL {
        for dir in dep.source_dirs(target) {
            files.extend(collect_files(&dir, SOURCE_EXTENSIONS));
        }
        for dir in dep.test_fixture_dirs(target) {
            files.extend(collect_files(&dir, SOURCE_EXTENSIONS));
        }
    }
    let mut packages = BTreeSet::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let declaration = text.lines().map(str::trim).find_map(|l| l.strip_prefix("package "));
        if let Some(name) = declaration.map(|d| d.trim_end_matches(';').trim().replace('`', "")).filter(|n| !n.is_empty()) {
            packages.insert(name);
        }
    }
    Ok(packages)
}

/// Whether `text` names the npm package `name` as a module (`'name'`, `"name/sub"`, ...).
fn names_module(text: &str, name: &str) -> bool {
    ['\'', '"', '`'].iter().any(|q| text.contains(&format!("{q}{name}{q}")) || text.contains(&format!("{q}{name}/")))
}

/// The checks of one dependency against what the sources use.
struct Use {
    main: bool,
    tests: bool,
    scripts: bool,
}

impl Use {
    fn of(packages: &BTreeSet<String>, main: &Names, tests: &Names, scripts: &Names) -> Self {
        Self { main: main.mentions_any(packages), tests: tests.mentions_any(packages), scripts: scripts.mentions_any(packages) }
    }

    /// `dev` for a dependency declared where only tests get it.
    fn verdict(&self, dev: bool, strict: bool) -> Option<Verdict> {
        if self.main || (self.tests && dev) || (self.scripts && !strict) {
            return None;
        }
        Some(if self.tests { Verdict::DevOnly } else { Verdict::Unused })
    }
}

/// `kargo tidy` for one package.
pub fn tidy(ctx: &BuildContext, opts: &TidyOptions) -> Result<Report> {
    let manifest = ctx.ws.manifest();
    let sources = gather(ctx);
    let (mut main, mut tests, mut scripts) = (Names::default(), Names::default(), Names::default());
    main.add_files(&sources.main);
    tests.add_files(&sources.tests);
    scripts.add_files(&sources.scripts);

    let mut report = Report {
        package: manifest.package.name.clone(),
        version: manifest.package.version.clone(),
        checked: 0,
        findings: Vec::new(),
        notes: Vec::new(),
    };
    let section_of = |section: DependencySection| matches!(section, DependencySection::Dev);

    // Maven modules, read from the resolution of each target: jars on the JVM, klibs elsewhere.
    let declared = super::upgrade::declared(manifest);
    let mut resolutions: BTreeMap<TargetKind, Vec<(String, Resolution)>> = BTreeMap::new();
    for target in manifest.targets.enabled().into_iter().filter(|t| *t != TargetKind::Android) {
        if declared.is_empty() {
            break;
        }
        match super::resolution_for(ctx, target, Configuration::Test) {
            Ok(found) => {
                resolutions.insert(target, found);
            }
            Err(e) => report.notes.push(format!("the {} dependencies were not checked: {e:#}", target.as_str())),
        }
    }
    for (section, dep) in declared {
        let skip = dep.scope != Scope::Compile
            || dep.bom
            || dep.force
            || dep.classifier.is_some()
            || dep.extension.is_some()
            || dep.module.artifact.starts_with("kotlin-stdlib");
        if skip {
            continue;
        }
        // The targets that get it: the common ones everywhere, a target's own only there.
        let targets: Vec<TargetKind> = match section {
            DependencySection::Target(kind) => vec![kind],
            _ => manifest.targets.enabled(),
        };
        if targets == [TargetKind::Android] {
            report.notes.push(format!("`{}` was not checked: Gradle resolves Android dependencies", dep.name));
            continue;
        }
        let mut packages = BTreeSet::new();
        for target in targets {
            for (_, resolution) in resolutions.get(&target).into_iter().flatten() {
                if let Some(file) = artifact_of(resolution, &dep.module, target) {
                    packages.extend(if target == TargetKind::Jvm { jar_packages(&file)? } else { klib_packages(&file)? });
                }
            }
        }
        if packages.is_empty() {
            report.notes.push(format!("`{}` has no classes to read in its resolution; it was not checked", dep.name));
            continue;
        }
        report.checked += 1;
        let used = Use::of(&packages, &main, &tests, &scripts);
        if let Some(verdict) = used.verdict(section_of(section), opts.strict) {
            report.findings.push(Finding { origin: Origin::Table(section), name: dep.name.clone(), kind: "maven", verdict });
        }
    }

    // Dependencies on other packages of the project.
    for dep in &manifest.path_dependencies {
        if dep.scope != Scope::Compile || dep.exported {
            continue;
        }
        let root = ctx.ws.root().join(&dep.path);
        let packages = match declared_packages(&root) {
            Ok(packages) if !packages.is_empty() => packages,
            Ok(_) => {
                report.notes.push(format!("path dependency `{}` declares no package; it was not checked", dep.name));
                continue;
            }
            Err(e) => {
                report.notes.push(format!("path dependency `{}` was not checked: {e:#}", dep.name));
                continue;
            }
        };
        report.checked += 1;
        let section = match (dep.dev, dep.target) {
            (true, _) => DependencySection::Dev,
            (false, Some(kind)) => DependencySection::Target(kind),
            (false, None) => DependencySection::Main,
        };
        let used = Use::of(&packages, &main, &tests, &scripts);
        if let Some(verdict) = used.verdict(dep.dev, opts.strict) {
            report.findings.push(Finding { origin: Origin::Table(section), name: dep.name.clone(), kind: "path", verdict });
        }
    }

    if opts.npm {
        npm(ctx, &sources, &main, &tests, &mut report);
    }
    report.findings.sort_by(|a, b| (a.verdict as u8, a.table(), &a.name).cmp(&(b.verdict as u8, b.table(), &b.name)));
    Ok(report)
}

/// The npm packages of Wasm and Kotlin/JS.
fn npm(ctx: &BuildContext, sources: &Sources, main: &Names, tests: &Names, report: &mut Report) {
    let ws = ctx.ws;
    for (kind, dev, name, _) in super::upgrade::declared_npm(ws.manifest()) {
        report.checked += 1;
        let package: BTreeSet<String> = std::iter::once(crate::dts::kotlin_package(&name)).collect();
        let js_files: Vec<PathBuf> = crate::js_sources::dirs(ws, kind).iter().flat_map(|d| collect_files(d, JS_EXTENSIONS)).collect();
        let in_js = js_files.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|t| names_module(&t, &name)));
        let named_in = |files: &BTreeSet<PathBuf>| files.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|t| names_module(&t, &name)));
        let used = Use {
            main: main.mentions_any(&package) || in_js || named_in(&sources.main),
            tests: tests.mentions_any(&package) || named_in(&sources.tests),
            scripts: false,
        };
        if let Some(verdict) = used.verdict(dev, true) {
            report.findings.push(Finding { origin: Origin::Npm(kind, dev), name, kind: "npm", verdict });
        }
    }
}

/// Removes the unused dependencies from `kargo.toml`. Returns how many.
pub fn fix(ctx: &BuildContext, report: &Report) -> Result<usize> {
    let mut editor = ManifestEditor::load(&ctx.ws.manifest_path())?;
    let mut removed = 0;
    for finding in report.unused() {
        match &finding.origin {
            Origin::Table(section) => editor.remove_dependency(*section, &finding.name)?,
            Origin::Npm(kind, dev) => editor.remove_npm_dependency(*kind, *dev, &finding.name)?,
        };
        ctx.shell().status("Removing", format!("{} from [{}]", finding.name, finding.table()));
        removed += 1;
    }
    if removed > 0 {
        ctx.ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
        editor.save()?;
    }
    Ok(removed)
}

/// The text report: one line per dependency, grouped by verdict.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let mut group = |title: &str, findings: Vec<&Finding>| {
        if findings.is_empty() {
            return;
        }
        let width = findings.iter().map(|f| f.name.len()).max().unwrap_or(0);
        out.push_str(&format!("{title} in {} v{}:\n", report.package, report.version));
        for f in findings {
            out.push_str(&format!("  {:width$}  [{}]\n", f.name, f.table()));
        }
    };
    group("unused dependencies", report.unused().collect());
    group("dependencies only tests use (move them to [dev-dependencies])", report.dev_only().collect());
    out
}

/// The JSON report of `--format json`.
pub fn json(report: &Report) -> Value {
    let entry = |f: &Finding| json!({ "name": f.name, "kind": f.kind, "table": f.table() });
    json!({
        "package": report.package,
        "version": report.version,
        "checked": report.checked,
        "unused": report.unused().map(entry).collect::<Vec<_>>(),
        "dev-only": report.dev_only().map(entry).collect::<Vec<_>>(),
        "notes": report.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Names {
        let mut names = Names::default();
        names.add(text);
        names
    }

    fn packages(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn finds_imports_star_imports_and_qualified_names() {
        let n = names("import kotlinx.coroutines.flow.map\nimport com.acme.*\nval x = org.apache.Foo.bar()\n");
        assert!(n.mentions_any(&packages(&["kotlinx.coroutines"])));
        assert!(n.mentions_any(&packages(&["kotlinx.coroutines.flow"])));
        assert!(n.mentions_any(&packages(&["com.acme"])));
        assert!(n.mentions_any(&packages(&["org.apache"])));
        assert!(!n.mentions_any(&packages(&["com.other"])));
        assert!(!n.mentions_any(&packages(&["kotlinx.serialization"])));
    }

    #[test]
    fn a_package_must_match_whole_segments() {
        let n = names("import com.acmetools.Widget");
        assert!(!n.mentions_any(&packages(&["com.acme"])));
    }

    #[test]
    fn dev_only_when_tests_alone_use_a_main_dependency() {
        let used = Use { main: false, tests: true, scripts: false };
        assert_eq!(used.verdict(false, false), Some(Verdict::DevOnly));
        assert_eq!(used.verdict(true, false), None);
        let unused = Use { main: false, tests: false, scripts: false };
        assert_eq!(unused.verdict(false, false), Some(Verdict::Unused));
        assert_eq!(unused.verdict(true, false), Some(Verdict::Unused));
    }

    #[test]
    fn scripts_keep_a_dependency_unless_strict() {
        let used = Use { main: false, tests: false, scripts: true };
        assert_eq!(used.verdict(false, false), None);
        assert_eq!(used.verdict(false, true), Some(Verdict::Unused));
    }

    #[test]
    fn reads_the_packages_of_a_klib() {
        let dir = std::env::temp_dir().join(format!("kargo-tidy-klib-{}", std::process::id()));
        let fragments = dir.join("default/linkdata/package_com.acme");
        std::fs::create_dir_all(&fragments).unwrap();
        std::fs::write(fragments.join("0_com.acme.knm"), b"").unwrap();
        assert_eq!(klib_packages(&dir).unwrap(), packages(&["com.acme"]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_an_npm_module() {
        assert!(names_module("import x from 'left-pad';", "left-pad"));
        assert!(names_module("import x from \"lodash/fp\";", "lodash"));
        assert!(!names_module("import x from 'left-padding';", "left-pad"));
    }
}
