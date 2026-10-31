//! Code coverage of the JVM tests (`kargo test --coverage`, `kargo coverage`).
//!
//! The test JVM runs with JaCoCo's agent (`-javaagent`), which writes an execution
//! file (`target/<profile>/jvm/coverage/jacoco.exec`). JaCoCo's CLI then reports
//! on the package's own classes and sources, plus the classes and sources of the
//! path dependencies its tests ran against, into HTML, JaCoCo XML and CSV. Kargo
//! derives Cobertura XML, LCOV (`lcov.info`) and JSON from the JaCoCo XML, prints
//! a terminal summary per package, and fails the run below the `[test.coverage]`
//! thresholds (or `--fail-under`).
//!
//! Only the JVM target measures coverage. Other targets run their tests without
//! it, and `kargo coverage` refuses them. JaCoCo is fetched from Maven Central
//! and pinned in `kargo.lock` like ktlint and detekt.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::context::BuildContext;
use kargo_manifest::{CoverageSettings, TargetKind};
use kargo_util::{fs::write_string_atomic, process};

/// JaCoCo version used unless `[test.coverage] jacoco` overrides it.
pub const JACOCO_VERSION: &str = "0.8.16";

const JACOCO_GROUP: &str = "org.jacoco";
const AGENT_ARTIFACT: &str = "org.jacoco.agent";
/// The classifier of the `-javaagent` jar: the plain `org.jacoco.agent` jar has no `Premain-Class`.
const AGENT_CLASSIFIER: &str = "runtime";
const CLI_ARTIFACT: &str = "org.jacoco.cli";
/// The classifier of the self-contained CLI jar, with ASM and the other dependencies bundled.
const CLI_CLASSIFIER: &str = "nodeps";

/// Options of `kargo coverage`: render the report of the last `--coverage` run again.
#[derive(Debug, Clone, Default)]
pub struct CoverageOptions {
    /// Fail below this overall line percentage instead of the manifest's thresholds (`--fail-under`).
    pub fail_under: Option<f64>,
    /// Open the HTML report in a browser.
    pub open: bool,
}

/// A line/branch/method counter of a JaCoCo report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counter {
    pub missed: u64,
    pub covered: u64,
}

impl Counter {
    pub fn total(self) -> u64 {
        self.missed + self.covered
    }

    /// Covered percent, or 100 when there is nothing to cover.
    pub fn percent(self) -> f64 {
        if self.total() == 0 { 100.0 } else { 100.0 * self.covered as f64 / self.total() as f64 }
    }
}

/// Coverage of one package (a dotted name such as `com.example`, or `(default)`).
#[derive(Debug, Clone, Default)]
pub struct PackageCoverage {
    pub name: String,
    pub line: Counter,
    pub branch: Counter,
}

/// One source file of a JaCoCo report, with its per-line hits.
#[derive(Debug, Clone, Default)]
pub struct FileCoverage {
    /// Dotted package name.
    pub package: String,
    /// File name as JaCoCo reports it (`Main.kt`).
    pub name: String,
    pub line: Counter,
    pub branch: Counter,
    /// `(line number, covered)` in file order.
    pub lines: Vec<(u32, bool)>,
    /// `(line number, covered branches, total branches)` for lines with branches.
    pub branches: Vec<(u32, u64, u64)>,
}

/// The parsed JaCoCo XML report.
#[derive(Debug, Clone, Default)]
pub struct CoverageSummary {
    pub line: Counter,
    pub branch: Counter,
    pub method: Counter,
    pub packages: Vec<PackageCoverage>,
    pub files: Vec<FileCoverage>,
}

impl CoverageSummary {
    pub fn line_percent(&self) -> f64 {
        self.line.percent()
    }

    pub fn branch_percent(&self) -> f64 {
        self.branch.percent()
    }
}

/// The JaCoCo version of the package: `[test.coverage] jacoco`, else Kargo's pinned one.
pub fn jacoco_version(settings: &CoverageSettings) -> &str {
    settings.jacoco_version.as_deref().unwrap_or(JACOCO_VERSION)
}

/// `kargo fetch --tools`: downloads the JaCoCo agent and CLI that coverage runs use.
pub fn fetch_tools(ctx: &BuildContext) -> Result<()> {
    let version = jacoco_version(&ctx.ws.manifest().test.coverage);
    agent_jar(ctx, version)?;
    cli_jar(ctx, version)?;
    Ok(())
}

/// The JaCoCo agent jar (`-javaagent`), locked in `kargo.lock` as `tools/org.jacoco.agent`.
fn agent_jar(ctx: &BuildContext, version: &str) -> Result<PathBuf> {
    ctx.tool_classified_jar(JACOCO_GROUP, AGENT_ARTIFACT, AGENT_CLASSIFIER, version)
        .with_context(|| format!("failed to fetch JaCoCo {version}"))
}

/// The self-contained JaCoCo CLI jar (`org.jacoco.cli.Main`), locked as `tools/org.jacoco.cli`.
fn cli_jar(ctx: &BuildContext, version: &str) -> Result<PathBuf> {
    ctx.tool_classified_jar(JACOCO_GROUP, CLI_ARTIFACT, CLI_CLASSIFIER, version)
        .with_context(|| format!("failed to obtain JaCoCo {version}"))
}

/// Directory holding the execution data and every report: `target/<profile>/jvm/coverage`.
pub fn coverage_dir(ctx: &BuildContext) -> PathBuf {
    ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("coverage")
}

/// The execution file the agent writes.
pub fn exec_file(ctx: &BuildContext) -> PathBuf {
    coverage_dir(ctx).join("jacoco.exec")
}

/// The `-javaagent` argument that measures the test JVM into `exec`, creating it.
/// `excludes` are JaCoCo class patterns (see [`agent_excludes`]).
pub fn agent_arg(ctx: &BuildContext, exec: &Path) -> Result<String> {
    let version = jacoco_version(&ctx.ws.manifest().test.coverage).to_owned();
    let agent = agent_jar(ctx, &version)?;
    let excludes = agent_excludes(&ctx.ws.manifest().test.coverage.exclude);
    let mut options = format!("destfile={}", process::slash(exec));
    // An existing exec of an earlier run must not linger: its sessions would merge into this one.
    let _ = std::fs::remove_file(exec);
    if !excludes.is_empty() {
        options.push_str(&format!(",excludes={excludes}"));
    }
    Ok(format!("-javaagent:{}={options}", process::slash(&agent)))
}

/// Maps `[test.coverage] exclude` file patterns (gitignore syntax) to JaCoCo agent
/// `excludes` class patterns, joined with `:`. Best effort: the precise filtering
/// happens when the report is read (see [`is_excluded`]); the agent patterns keep
/// the obvious generated classes out of the execution data already.
pub fn agent_excludes(exclude: &[String]) -> String {
    exclude.iter().filter_map(|s| class_pattern(s)).collect::<Vec<_>>().join(":")
}

/// One file pattern as a JaCoCo class pattern (`**/generated/**` becomes `*.generated.*`).
/// Returns `None` when the pattern has nothing a class pattern can use.
fn class_pattern(pattern: &str) -> Option<String> {
    let mut text = pattern.trim().replace('\\', "/");
    text = text.trim_matches('/').to_owned();
    if text.is_empty() {
        return None;
    }
    // A file name stays a class name; a directory becomes the classes below it.
    for ext in [".kt", ".java", ".kts"] {
        if let Some(stripped) = text.strip_suffix(ext) {
            text = stripped.to_owned();
            break;
        }
    }
    let mut dotted = text.replace('/', ".");
    // `**` already means any number of packages in JaCoCo patterns.
    while dotted.contains("...") {
        dotted = dotted.replace("...", "**");
    }
    if !dotted.contains('*') && !dotted.contains('?') {
        dotted.push_str(".*");
    }
    Some(dotted)
}

/// Whether the JaCoCo `package`/`sourcefile` (or `class`) is left out by `exclude`.
/// The patterns are gitignore syntax matched against `package/path/Source.kt`
/// (dots become slashes), and a pattern with no slash matches the file name.
/// Classes whose name contains `Generated` are left out when an exclude pattern
/// names generated code, matching the `@Generated` convention.
pub fn is_excluded(package: &str, source: &str, class: &str, exclude: &[String]) -> bool {
    if exclude.is_empty() {
        return false;
    }
    let path = format!("{}/{source}", package.replace('.', "/"));
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    for pattern in exclude {
        builder.add_line(None, pattern).ok();
    }
    let Ok(matcher) = builder.build() else { return false };
    if matcher.matched(Path::new(&path), false).is_ignore() {
        return true;
    }
    // `@Generated` convention: a pattern naming generated code also drops the
    // classes whose name says they are generated.
    let generated = exclude.iter().any(|p| p.to_ascii_lowercase().contains("generated"));
    generated && (class.contains("Generated") || path.to_ascii_lowercase().contains("generated"))
}

/// The class files the report covers: the package's own classes plus the jars and
/// class directories of the path dependencies its tests ran against.
pub fn classfiles(ctx: &BuildContext, main_classes: &Path) -> Vec<PathBuf> {
    let mut files = vec![main_classes.to_path_buf()];
    for local in ctx.local_dependencies(TargetKind::Jvm, false).unwrap_or_default() {
        let pkg = ctx.project.package(local.package);
        let dir = pkg.output_dir(&ctx.profile, TargetKind::Jvm);
        for candidate in [dir.join("classes"), dir.join(format!("{}-{}.jar", pkg.name(), pkg.manifest().package.version))] {
            if candidate.exists() && !files.contains(&candidate) {
                files.push(candidate);
            }
        }
    }
    files.retain(|p| p.exists());
    if files.is_empty() {
        files.push(main_classes.to_path_buf());
    }
    files
}

/// The source directories the report reads: `src/`, `src-jvm/`, the intermediate
/// source sets of the JVM target and what `[[generate]]` steps wrote.
pub fn sourcefiles(ctx: &BuildContext) -> Vec<PathBuf> {
    let ws = ctx.ws;
    let mut dirs = vec![ws.common_sources(), ws.target_sources(TargetKind::Jvm)];
    dirs.extend(ws.manifest().source_groups_for(TargetKind::Jvm).into_iter().map(|(g, _)| ws.group_sources(&g.name)));
    dirs.extend(crate::generate::dirs(ws, &ctx.profile, TargetKind::Jvm, kargo_manifest::GeneratedKind::Sources));
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if dir.is_dir() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    if out.is_empty() {
        out.push(ws.common_sources());
    }
    out
}

/// Runs JaCoCo's `Report` on `exec` and writes HTML, JaCoCo XML and CSV into `dir`.
/// Returns the paths of the XML file the other formats are derived from.
pub fn run_cli_report(ctx: &BuildContext, exec: &Path, classfiles: &[PathBuf], sourcefiles: &[PathBuf], dir: &Path) -> Result<PathBuf> {
    let version = jacoco_version(&ctx.ws.manifest().test.coverage).to_owned();
    let cli = cli_jar(ctx, &version)?;
    if !exec.is_file() {
        bail!("no coverage data at {}; run `kargo test --coverage` first", exec.display());
    }
    let html = dir.join("html");
    let xml = dir.join("jacoco.xml");
    let csv = dir.join("jacoco.csv");
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let jdk = ctx.compiler_jdk()?;
    let mut cmd = Command::new(jdk.java());
    cmd.args(jdk.default_run_args()).arg("-jar").arg(process::slash(&cli)).arg("report").arg(process::slash(exec));
    for classes in classfiles {
        cmd.arg("--classfiles").arg(process::slash(classes));
    }
    for sources in sourcefiles {
        cmd.arg("--sourcefiles").arg(process::slash(sources));
    }
    cmd.arg("--html")
        .arg(process::slash(&html))
        .arg("--xml")
        .arg(process::slash(&xml))
        .arg("--csv")
        .arg(process::slash(&csv))
        .arg("--name")
        .arg(&ctx.ws.manifest().package.name)
        .current_dir(ctx.ws.root());
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().context("failed to start JaCoCo")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("JaCoCo failed ({}): {}{}", process::describe_status(output.status), stdout.trim(), stderr.trim());
    }
    Ok(xml)
}

/// Reads the JaCoCo XML report at `xml`, leaving out what `exclude` filters.
pub fn parse_jacoco_xml(xml: &str, exclude: &[String]) -> Result<CoverageSummary> {
    // JaCoCo writes `<!DOCTYPE report PUBLIC "-//JACOCO//DTD Report 1.1//EN" "report.dtd">`,
    // which the XML reader refuses; it carries no information the report needs.
    let xml = strip_doctype(xml);
    let doc = roxmltree::Document::parse(&xml).context("failed to read the JaCoCo XML report")?;
    let root = doc.root_element();
    if root.tag_name().name() != "report" {
        bail!("failed to read the JaCoCo XML report: expected a <report> element");
    }
    let mut summary = CoverageSummary::default();
    for counter in root.children().filter(|n| n.tag_name().name() == "counter") {
        apply_counter(&mut summary, &counter, true);
    }
    for package in root.children().filter(|n| n.tag_name().name() == "package") {
        let package_name = package.attribute("name").unwrap_or_default().replace('/', ".");
        let mut pkg =
            PackageCoverage { name: if package_name.is_empty() { "(default)".to_owned() } else { package_name }, ..Default::default() };
        for counter in package.children().filter(|n| n.tag_name().name() == "counter") {
            apply_counter_node(&mut pkg, &counter);
        }
        for source in package.children().filter(|n| n.tag_name().name() == "sourcefile") {
            let source_name = source.attribute("name").unwrap_or_default().to_owned();
            let mut file = FileCoverage { package: pkg.name.clone(), name: source_name.clone(), ..Default::default() };
            for counter in source.children().filter(|n| n.tag_name().name() == "counter") {
                apply_counter_file(&mut file, &counter);
            }
            for line in source.children().filter(|n| n.tag_name().name() == "line") {
                let nr: u32 = line.attribute("nr").unwrap_or("0").parse().unwrap_or(0);
                let mi: u64 = line.attribute("mi").unwrap_or("0").parse().unwrap_or(0);
                let ci: u64 = line.attribute("ci").unwrap_or("0").parse().unwrap_or(0);
                let mb: u64 = line.attribute("mb").unwrap_or("0").parse().unwrap_or(0);
                let cb: u64 = line.attribute("cb").unwrap_or("0").parse().unwrap_or(0);
                if nr > 0 {
                    file.lines.push((nr, ci > 0 || (mi == 0 && mb == 0)));
                    if mb + cb > 0 {
                        file.branches.push((nr, cb, mb + cb));
                    }
                }
            }
            // The class names live on the <class> elements; a source file is excluded
            // when any of its classes is excluded.
            let excluded = package
                .children()
                .filter(|n| n.tag_name().name() == "class")
                .filter(|c| c.attribute("sourcefilename").unwrap_or_default() == file.name)
                .any(|c| is_excluded(&pkg.name, &file.name, c.attribute("name").unwrap_or_default(), exclude))
                || is_excluded(&pkg.name, &file.name, "", exclude);
            if !excluded {
                // Recompute the file counters from what the report says (they already exclude nothing else).
                summary.files.push(file);
            }
        }
        // Recompute the package counters from the files kept, so `exclude` applies
        // to the summary too. Packages without any kept file are dropped.
        let kept: Vec<&FileCoverage> = summary.files.iter().filter(|f| f.package == pkg.name).collect();
        if kept.is_empty() {
            continue;
        }
        pkg.line = Counter { missed: kept.iter().map(|f| f.line.missed).sum(), covered: kept.iter().map(|f| f.line.covered).sum() };
        pkg.branch = Counter { missed: kept.iter().map(|f| f.branch.missed).sum(), covered: kept.iter().map(|f| f.branch.covered).sum() };
        summary.packages.push(pkg);
    }
    summary.packages.sort_by(|a, b| a.name.cmp(&b.name));
    summary.files.sort_by(|a, b| (&a.package, &a.name).cmp(&(&b.package, &b.name)));
    // Totals follow the kept files, so thresholds see what the report shows.
    summary.line =
        Counter { missed: summary.files.iter().map(|f| f.line.missed).sum(), covered: summary.files.iter().map(|f| f.line.covered).sum() };
    summary.branch = Counter {
        missed: summary.files.iter().map(|f| f.branch.missed).sum(),
        covered: summary.files.iter().map(|f| f.branch.covered).sum(),
    };
    Ok(summary)
}

fn counter_values(node: &roxmltree::Node) -> (u64, u64) {
    let missed = node.attribute("missed").unwrap_or("0").parse().unwrap_or(0);
    let covered = node.attribute("covered").unwrap_or("0").parse().unwrap_or(0);
    (missed, covered)
}

fn apply_counter(summary: &mut CoverageSummary, node: &roxmltree::Node, top: bool) {
    let (missed, covered) = counter_values(node);
    match node.attribute("type").unwrap_or_default() {
        "LINE" => summary.line = Counter { missed, covered },
        "BRANCH" => summary.branch = Counter { missed, covered },
        "METHOD" => summary.method = Counter { missed, covered },
        _ => {}
    }
    let _ = top;
}

fn apply_counter_node(pkg: &mut PackageCoverage, node: &roxmltree::Node) {
    let (missed, covered) = counter_values(node);
    match node.attribute("type").unwrap_or_default() {
        "LINE" => pkg.line = Counter { missed, covered },
        "BRANCH" => pkg.branch = Counter { missed, covered },
        _ => {}
    }
}

fn apply_counter_file(file: &mut FileCoverage, node: &roxmltree::Node) {
    let (missed, covered) = counter_values(node);
    match node.attribute("type").unwrap_or_default() {
        "LINE" => file.line = Counter { missed, covered },
        "BRANCH" => file.branch = Counter { missed, covered },
        _ => {}
    }
}

/// Removes the `<!DOCTYPE ...>` declaration of an XML document, if any. The reader
/// refuses documents with a DTD, and reports carry no internal subset worth keeping.
fn strip_doctype(xml: &str) -> std::borrow::Cow<'_, str> {
    let Some(start) = xml.find("<!DOCTYPE") else { return std::borrow::Cow::Borrowed(xml) };
    let mut in_single = false;
    let mut in_double = false;
    let mut end = None;
    for (i, c) in xml[start..].char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '>' if !in_single && !in_double => {
                end = Some(start + i + c.len_utf8());
                break;
            }
            _ => {}
        }
    }
    match end {
        Some(end) => {
            let mut out = String::with_capacity(xml.len());
            out.push_str(&xml[..start]);
            out.push_str(&xml[end..]);
            std::borrow::Cow::Owned(out)
        }
        None => std::borrow::Cow::Borrowed(xml),
    }
}

/// Parses an LCOV report (`lcov.info`) into per-file records: `(source path, line hits, branch hits)`.
/// Used by the offline unit tests and by editors that read the LCOV file back.
pub fn parse_lcov(text: &str) -> Vec<LcovRecord> {
    let mut records = Vec::new();
    let mut current: Option<LcovRecord> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            current = Some(LcovRecord { source: path.trim().to_owned(), lines: Vec::new(), branches: Vec::new() });
        } else if let Some(rest) = line.strip_prefix("DA:") {
            if let Some(record) = current.as_mut() {
                let mut parts = rest.split(',');
                if let (Some(nr), Some(hits)) = (parts.next(), parts.next())
                    && let (Ok(nr), Ok(hits)) = (nr.trim().parse::<u32>(), hits.trim().parse::<u64>())
                {
                    record.lines.push((nr, hits));
                }
            }
        } else if let Some(rest) = line.strip_prefix("BRDA:") {
            if let Some(record) = current.as_mut() {
                let parts: Vec<&str> = rest.split(',').collect();
                if parts.len() >= 4
                    && let (Ok(nr), Ok(taken)) = (parts[0].trim().parse::<u32>(), parts[3].trim().parse::<i64>())
                {
                    record.branches.push((nr, taken > 0));
                }
            }
        } else if line.trim() == "end_of_record"
            && let Some(record) = current.take()
        {
            records.push(record);
        }
    }
    records
}

/// One file of an LCOV report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcovRecord {
    pub source: String,
    pub lines: Vec<(u32, u64)>,
    pub branches: Vec<(u32, bool)>,
}

/// Writes `lcov.info` for `summary` into `path`.
pub fn write_lcov(path: &Path, summary: &CoverageSummary) -> Result<()> {
    let mut text = String::new();
    for file in &summary.files {
        text.push_str(&format!("SF:{}/{}\n", file.package.replace('.', "/"), file.name));
        for (nr, covered) in &file.lines {
            text.push_str(&format!("DA:{},{}\n", nr, if *covered { 1 } else { 0 }));
        }
        for (nr, covered, total) in &file.branches {
            for branch in 0..*total {
                text.push_str(&format!("BRDA:{},0,{},{}\n", nr, branch, if branch < *covered { "1" } else { "-" }));
            }
        }
        let found = file.line.total();
        text.push_str(&format!("LF:{found}\nLH:{}\n", file.line.covered));
        let (br_found, br_hit) =
            (file.branches.iter().map(|(_, _, t)| t).sum::<u64>(), file.branches.iter().map(|(_, c, _)| c).sum::<u64>());
        text.push_str(&format!("BRF:{br_found}\nBRH:{br_hit}\nend_of_record\n"));
    }
    write_string_atomic(path, &text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Writes a Cobertura XML report for `summary` into `path` (for SonarQube and Codecov).
pub fn write_cobertura(path: &Path, summary: &CoverageSummary, package_name: &str) -> Result<()> {
    let mut text = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let total_lines = summary.line.total();
    let line_rate = if total_lines == 0 { 1.0 } else { summary.line.covered as f64 / total_lines as f64 };
    let total_branches = summary.branch.total();
    let branch_rate = if total_branches == 0 { 1.0 } else { summary.branch.covered as f64 / total_branches as f64 };
    text.push_str(&format!(
        "<coverage version=\"1.0\" line-rate=\"{line_rate:.4}\" branch-rate=\"{branch_rate:.4}\" lines-covered=\"{}\" lines-valid=\"{total_lines}\" branches-covered=\"{}\" branches-valid=\"{total_branches}\">\n",
        summary.line.covered, summary.branch.covered
    ));
    text.push_str("  <sources><source>.</source></sources>\n  <packages>\n");
    for pkg in &summary.packages {
        let files: Vec<&FileCoverage> = summary.files.iter().filter(|f| f.package == pkg.name).collect();
        let pkg_lines = pkg.line.total();
        let pkg_line_rate = if pkg_lines == 0 { 1.0 } else { pkg.line.covered as f64 / pkg_lines as f64 };
        let pkg_branches = pkg.branch.total();
        let pkg_branch_rate = if pkg_branches == 0 { 1.0 } else { pkg.branch.covered as f64 / pkg_branches as f64 };
        text.push_str(&format!(
            "    <package name=\"{}\" line-rate=\"{pkg_line_rate:.4}\" branch-rate=\"{pkg_branch_rate:.4}\">\n      <classes>\n",
            xml_escape(&pkg.name)
        ));
        for file in files {
            let class_name = format!("{}.{}", file.package, file.name.trim_end_matches(".kt").trim_end_matches(".java"));
            let file_lines = file.line.total();
            let file_rate = if file_lines == 0 { 1.0 } else { file.line.covered as f64 / file_lines as f64 };
            text.push_str(&format!(
                "        <class name=\"{}\" filename=\"{}/{}\" line-rate=\"{file_rate:.4}\" branch-rate=\"{file_rate:.4}\">\n          <lines>\n",
                xml_escape(&class_name),
                xml_escape(&file.package.replace('.', "/")),
                xml_escape(&file.name)
            ));
            for (nr, covered) in &file.lines {
                text.push_str(&format!(
                    "            <line number=\"{nr}\" hits=\"{}\" branch=\"{}\" />\n",
                    if *covered { 1 } else { 0 },
                    file.branches.iter().any(|(b, _, _)| b == nr)
                ));
            }
            text.push_str("          </lines>\n        </class>\n");
        }
        text.push_str("      </classes>\n    </package>\n");
    }
    text.push_str("  </packages>\n</coverage>\n");
    let _ = package_name;
    write_string_atomic(path, &text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Writes a JSON report for `summary` into `path` (for Codecov and editor gutters).
pub fn write_json(path: &Path, summary: &CoverageSummary, package_name: &str) -> Result<()> {
    let doc = serde_json::json!({
        "package": package_name,
        "lines": { "covered": summary.line.covered, "missed": summary.line.missed, "total": summary.line.total(), "percent": summary.line_percent() },
        "branches": { "covered": summary.branch.covered, "missed": summary.branch.missed, "total": summary.branch.total(), "percent": summary.branch_percent() },
        "packages": summary.packages.iter().map(|p| serde_json::json!({
            "name": p.name,
            "lines": { "covered": p.line.covered, "missed": p.line.missed, "percent": p.line.percent() },
            "branches": { "covered": p.branch.covered, "missed": p.branch.missed, "percent": p.branch.percent() },
        })).collect::<Vec<_>>(),
        "files": summary.files.iter().map(|f| serde_json::json!({
            "package": f.package,
            "name": f.name,
            "lines": { "covered": f.line.covered, "missed": f.line.missed, "percent": f.line.percent() },
            "uncovered": f.lines.iter().filter(|(_, c)| !c).map(|(nr, _)| nr).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&doc).context("failed to serialize the coverage JSON")?;
    write_string_atomic(path, &(text + "\n")).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Prints the terminal summary: totals plus one line per package. It goes where the
/// human-readable test report goes (see `super::test::human_out`).
pub fn print_summary(summary: &CoverageSummary) -> Result<()> {
    let mut out = super::test::human_out();
    writeln!(
        out,
        "\ncoverage: {:.1}% lines ({}/{}), {:.1}% branches ({}/{})",
        summary.line_percent(),
        summary.line.covered,
        summary.line.total(),
        summary.branch_percent(),
        summary.branch.covered,
        summary.branch.total()
    )?;
    for pkg in &summary.packages {
        writeln!(out, "    {:<40} {:>5.1}% lines {:>5.1}% branches", pkg.name, pkg.line.percent(), pkg.branch.percent())?;
    }
    Ok(())
}

/// Checks the thresholds: `--fail-under` (or `[test.coverage] fail-under`/`min-line`)
/// on overall lines, and `[test.coverage] min-branch` on overall branches.
/// Returns `true` when the coverage passes. With `--message-format json` the caller
/// also emits the summary message.
pub fn check_thresholds(settings: &CoverageSettings, summary: &CoverageSummary, fail_under: Option<f64>) -> Result<bool> {
    if let Some(min) = settings.line_threshold(fail_under)
        && summary.line_percent() < min
    {
        anyhow::bail!("line coverage {:.1}% is below the minimum of {min:.1}%", summary.line_percent());
    }
    if let Some(min) = settings.min_branch
        && summary.branch_percent() < min
    {
        anyhow::bail!("branch coverage {:.1}% is below the minimum of {min:.1}%", summary.branch_percent());
    }
    Ok(true)
}

/// Emits the `coverage-summary` JSON message (`--message-format json`).
pub fn emit_summary(ctx: &BuildContext, summary: &CoverageSummary, success: bool) {
    if !kargo_compiler::diagnostics::is_json() {
        return;
    }
    let json = serde_json::json!({
        "reason": "coverage-summary",
        "lines": { "covered": summary.line.covered, "missed": summary.line.missed, "percent": summary.line_percent() },
        "branches": { "covered": summary.branch.covered, "missed": summary.branch.missed, "percent": summary.branch_percent() },
        "success": success,
    });
    if let serde_json::Value::Object(message) = json {
        super::test::emit_event(&ctx.ws.manifest().package.name, TargetKind::Jvm, message);
    }
}

/// Generates every report from the execution data at `exec` into the coverage
/// directory, prints the summary and checks the thresholds. Returns the summary
/// and whether the thresholds passed.
pub fn report(
    ctx: &BuildContext,
    exec: &Path,
    classfiles: &[PathBuf],
    sourcefiles: &[PathBuf],
    fail_under: Option<f64>,
) -> Result<(CoverageSummary, bool)> {
    let dir = coverage_dir(ctx);
    let xml = run_cli_report(ctx, exec, classfiles, sourcefiles, &dir)?;
    finish(ctx, &xml, fail_under)
}

/// Reads the JaCoCo XML at `xml`, writes the derived reports, prints the summary
/// and checks the thresholds. Shared by `kargo test --coverage` and `kargo coverage`.
pub fn finish(ctx: &BuildContext, xml: &Path, fail_under: Option<f64>) -> Result<(CoverageSummary, bool)> {
    let settings = &ctx.ws.manifest().test.coverage;
    let text = std::fs::read_to_string(xml).with_context(|| format!("failed to read {}", xml.display()))?;
    let summary = parse_jacoco_xml(&text, &settings.exclude)?;
    let dir = coverage_dir(ctx);
    write_lcov(&dir.join("lcov.info"), &summary)?;
    write_cobertura(&dir.join("cobertura.xml"), &summary, &ctx.ws.manifest().package.name)?;
    write_json(&dir.join("coverage.json"), &summary, &ctx.ws.manifest().package.name)?;
    print_summary(&summary)?;
    let shown = dir.strip_prefix(ctx.ws.root()).unwrap_or(&dir);
    ctx.shell().status("Reported", format!("coverage in {}", shown.display()));
    match check_thresholds(settings, &summary, fail_under) {
        Ok(passed) => {
            emit_summary(ctx, &summary, passed);
            Ok((summary, passed))
        }
        Err(e) => {
            emit_summary(ctx, &summary, false);
            ctx.shell().error(format!("{e:#}"));
            Ok((summary, false))
        }
    }
}

/// Regenerates the reports from the last `--coverage` run without running tests (`kargo coverage`).
pub fn coverage(ctx: &BuildContext, opts: &CoverageOptions, main_classes: Option<&Path>) -> Result<bool> {
    if !ctx.ws.manifest().targets.is_enabled(TargetKind::Jvm) {
        bail!("code coverage needs the jvm target; add it with `kargo target add jvm`");
    }
    let exec = exec_file(ctx);
    if !exec.is_file() {
        bail!("no coverage data at {}; run `kargo test --coverage` first", exec.display());
    }
    let classes = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("classes");
    let main = main_classes.map(Path::to_path_buf).unwrap_or(classes);
    let classfiles = classfiles(ctx, &main);
    let sourcefiles = sourcefiles(ctx);
    let xml = run_cli_report(ctx, &exec, &classfiles, &sourcefiles, &coverage_dir(ctx))?;
    let (_, passed) = finish(ctx, &xml, opts.fail_under)?;
    if opts.open {
        crate::web::open_in_browser(coverage_dir(ctx).join("html").join("index.html").as_os_str())?;
    }
    Ok(passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE report PUBLIC "-//JACOCO//DTD Report 1.1//EN" "report.dtd">
<report name="app">
  <package name="com/example">
    <class name="com/example/Calculator" sourcefilename="Calculator.kt">
      <method name="add" desc="(II)I"><counter type="LINE" missed="0" covered="3"/><counter type="BRANCH" missed="0" covered="0"/></method>
      <counter type="LINE" missed="1" covered="9"/><counter type="BRANCH" missed="1" covered="3"/>
    </class>
    <sourcefile name="Calculator.kt">
      <line nr="1" mi="0" ci="4" mb="0" cb="0"/>
      <line nr="2" mi="0" ci="5" mb="1" cb="3"/>
      <line nr="3" mi="2" ci="0" mb="0" cb="0"/>
      <counter type="LINE" missed="1" covered="9"/><counter type="BRANCH" missed="1" covered="3"/>
    </sourcefile>
    <sourcefile name="Generated.kt">
      <line nr="1" mi="0" ci="6" mb="0" cb="0"/>
      <counter type="LINE" missed="0" covered="2"/><counter type="BRANCH" missed="0" covered="0"/>
    </sourcefile>
    <counter type="LINE" missed="1" covered="11"/><counter type="BRANCH" missed="1" covered="3"/>
  </package>
  <counter type="LINE" missed="1" covered="11"/><counter type="BRANCH" missed="1" covered="3"/>
</report>"#;

    #[test]
    fn parses_jacoco_xml_with_covered_and_uncovered_lines() {
        let summary = parse_jacoco_xml(SAMPLE_XML, &[]).unwrap();
        assert_eq!((summary.line.covered, summary.line.missed), (11, 1));
        assert_eq!((summary.branch.covered, summary.branch.missed), (3, 1));
        assert_eq!(summary.packages.len(), 1);
        assert_eq!(summary.packages[0].name, "com.example");
        let calc = summary.files.iter().find(|f| f.name == "Calculator.kt").unwrap();
        assert_eq!(calc.lines, [(1, true), (2, true), (3, false)]);
        assert_eq!(calc.branches, [(2, 3, 4)]);
        // LCOV round-trips the hits.
        let dir = tempfile::tempdir().unwrap();
        let lcov = dir.path().join("lcov.info");
        write_lcov(&lcov, &summary).unwrap();
        let records = parse_lcov(&std::fs::read_to_string(&lcov).unwrap());
        let record = records.iter().find(|r| r.source.ends_with("Calculator.kt")).unwrap();
        assert!(record.lines.contains(&(1, 1)) && record.lines.contains(&(3, 0)));
        assert!(record.branches.iter().any(|(nr, taken)| *nr == 2 && *taken));
    }

    #[test]
    fn exclude_leaves_out_generated_sources() {
        let exclude = ["**/generated/**".to_owned(), "**/Generated.kt".to_owned()];
        let summary = parse_jacoco_xml(SAMPLE_XML, &exclude).unwrap();
        assert!(summary.files.iter().all(|f| f.name != "Generated.kt"));
        assert_eq!((summary.line.covered, summary.line.missed), (9, 1));
        assert!(is_excluded("com.example", "Generated.kt", "com/example/Generated", &exclude));
        assert!(!is_excluded("com.example", "Calculator.kt", "com/example/Calculator", &exclude));
    }

    #[test]
    fn thresholds_fail_below_the_minimum() {
        let summary = parse_jacoco_xml(SAMPLE_XML, &[]).unwrap();
        let settings = CoverageSettings { min_line: Some(80.0), min_branch: Some(70.0), ..Default::default() };
        assert!(check_thresholds(&settings, &summary, None).is_ok());
        let strict = CoverageSettings { min_line: Some(99.0), ..Default::default() };
        assert!(check_thresholds(&strict, &summary, None).is_err());
        assert!(check_thresholds(&settings, &summary, Some(99.0)).is_err(), "--fail-under overrides the manifest");
    }

    #[test]
    fn agent_excludes_map_file_patterns_to_class_patterns() {
        assert_eq!(agent_excludes(&["**/generated/**".to_owned()]), "**.generated.**");
        assert_eq!(agent_excludes(&[]), "");
        assert!(agent_excludes(&["**/*Generated.kt".to_owned()]).contains("Generated"));
    }

    #[test]
    fn writes_cobertura_and_json() {
        let summary = parse_jacoco_xml(SAMPLE_XML, &[]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_cobertura(&dir.path().join("cobertura.xml"), &summary, "app").unwrap();
        write_json(&dir.path().join("coverage.json"), &summary, "app").unwrap();
        let cobertura = std::fs::read_to_string(dir.path().join("cobertura.xml")).unwrap();
        assert!(cobertura.contains("<coverage") && cobertura.contains("Calculator.kt"));
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("coverage.json")).unwrap()).unwrap();
        assert_eq!(json["lines"]["covered"], 11);
        assert!(json["files"].as_array().unwrap().iter().any(|f| f["name"] == "Calculator.kt"));
    }
}
