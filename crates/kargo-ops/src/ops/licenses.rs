//! `kargo licenses`: reports the licenses of the packages a project ships (the Maven modules of every
//! target and the npm packages of Wasm and Kotlin/JS, plus the tests and the build's tools with `--all`),
//! writes a third-party notice file, and checks them against the `[licenses]` policy.
//!
//! The packages are the ones `kargo audit` collects ([`super::audit::collect`]). A Maven module's licenses are
//! the `<licenses>` of its POM or of the nearest parent that has them, mapped to SPDX ids by
//! [`crate::spdx`]; an npm package's are the `license` field of the `package.json` installed in
//! `node_modules`. Nothing here needs the network beyond the POMs, which `--offline` reads from the cache.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::audit::{self, AuditOptions, Subject, display_name};
use super::context::BuildContext;
use crate::advisories::{Ecosystem, Query};
use crate::spdx::{self, UNKNOWN};
use kargo_manifest::{LicenseSettings, TargetKind};
use kargo_maven::pom::{self, Pom, PomLicense, PomSource};
use kargo_maven::{Artifact, MavenClient, ModuleId};
use kargo_util::shell::Shell;

#[derive(Debug, Clone, Default)]
pub struct LicensesOptions {
    /// Only these targets; every enabled one when empty.
    pub targets: Vec<TargetKind>,
    /// Also the tests and the build's tools, not only what a build ships.
    pub all: bool,
}

/// Why a package fails the `[licenses]` policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Its license is in `deny`.
    Denied(String),
    /// `allow` is set and does not list its license.
    NotAllowed(String),
    /// `allow` is set and the license could not be told.
    Unknown,
}

impl Violation {
    fn as_str(&self) -> &'static str {
        match self {
            Violation::Denied(_) => "denied",
            Violation::NotAllowed(_) => "not-allowed",
            Violation::Unknown => "unknown",
        }
    }
}

/// A package and its license.
#[derive(Debug, Clone)]
pub struct Package {
    pub subject: Subject,
    /// The licenses its POM or `package.json` declares, as written.
    pub declared: Vec<PomLicense>,
    /// The licenses it is under: alternatives to choose from, each the SPDX ids that apply together
    /// ([`UNKNOWN`] for one that could not be told). The license of `[licenses] exceptions` when it has one.
    pub alternatives: Vec<Vec<String>>,
    /// Whether `[licenses] exceptions` gives the license.
    pub exception: bool,
    pub violation: Option<Violation>,
}

impl Package {
    /// The license as one SPDX expression, such as `Apache-2.0` or `Apache-2.0 OR MIT`.
    pub fn license(&self) -> String {
        self.alternatives.iter().map(|ids| ids.join(" AND ")).collect::<Vec<_>>().join(" OR ")
    }

    /// `group:artifact:version`, or `name@version` for an npm package.
    fn coordinate(&self) -> String {
        let query = &self.subject.query;
        match query.ecosystem {
            Ecosystem::Maven => format!("{}:{}", query.name, query.version),
            Ecosystem::Npm => format!("{}@{}", query.name, query.version),
        }
    }

    /// What the POM or `package.json` writes, to tell a license Kargo does not know.
    fn declared_text(&self) -> String {
        let texts: Vec<String> = self
            .declared
            .iter()
            .filter_map(|l| match (&l.name, &l.url) {
                (Some(name), Some(url)) => Some(format!("{name}, {url}")),
                (name, url) => name.clone().or_else(|| url.clone()),
            })
            .collect();
        if texts.is_empty() { "no license".to_owned() } else { texts.join("; ") }
    }

    /// The sentence that says what is wrong, after the package.
    pub fn problem(&self) -> Option<String> {
        Some(match self.violation.as_ref()? {
            Violation::Denied(id) => format!("is under {id}, which [licenses] deny lists"),
            Violation::NotAllowed(id) => format!("is under {id}, which [licenses] allow does not list"),
            Violation::Unknown => format!(
                "has a license Kargo cannot identify ({}); give the license it has in [licenses] exceptions, such as \"{}\" = \"Apache-2.0\"",
                self.declared_text(),
                self.subject.query.name
            ),
        })
    }
}

/// The result of checking one package.
#[derive(Debug, Clone)]
pub struct Report {
    pub package: String,
    pub version: String,
    /// Every package, by name and version.
    pub packages: Vec<Package>,
    /// Why some packages could not be checked.
    pub skipped: Vec<String>,
    pub policy: LicenseSettings,
}

/// The packages under one license.
pub struct Group<'a> {
    pub license: String,
    pub packages: Vec<&'a Package>,
}

impl Report {
    /// The packages that fail the policy.
    pub fn violations(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(|p| p.violation.is_some())
    }

    /// The packages by license: the most used first, then by name, and the unknown ones last.
    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut groups: BTreeMap<String, Vec<&Package>> = BTreeMap::new();
        for package in &self.packages {
            groups.entry(package.license()).or_default().push(package);
        }
        let mut groups: Vec<Group> = groups.into_iter().map(|(license, packages)| Group { license, packages }).collect();
        groups.sort_by_key(|g| (g.license == UNKNOWN, std::cmp::Reverse(g.packages.len())));
        groups
    }
}

/// Reads the POMs of Maven modules through the client: its repositories, the cache and `--offline`.
struct Poms<'a>(&'a MavenClient);

impl PomSource for Poms<'_> {
    fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom> {
        let rel = self.0.artifact_path(&Artifact::new(module.clone(), version).with_extension("pom"))?;
        let xml = self.0.fetch_text(&rel)?.with_context(|| format!("the POM of {module}:{version} is in no repository"))?;
        Pom::parse(&xml).with_context(|| format!("failed to parse the POM of {module}:{version}"))
    }
}

/// `kargo licenses` for one package: collects what it resolves, reads each package's licenses and checks
/// them against `[licenses]`.
pub fn licenses(ctx: &BuildContext, opts: &LicensesOptions) -> Result<Report> {
    let audit_opts = AuditOptions { targets: opts.targets.clone(), tooling: opts.all, runtime_only: !opts.all };
    let (subjects, mut skipped) = audit::collect(ctx, &audit_opts)?;
    let pkg = &ctx.ws.manifest().package;
    ctx.shell().status("Checking", format!("the licenses of {} v{} ({} packages)", pkg.name, pkg.version, subjects.len()));
    let poms = Poms(&ctx.client);
    let declared = kargo_maven::par_map_io(&subjects, |subject| declared_licenses(&poms, subject));

    let policy = ctx.ws.manifest().licenses.clone();
    let mut used = BTreeSet::new();
    let mut packages = Vec::new();
    for (subject, declared) in subjects.into_iter().zip(declared) {
        let declared = declared.unwrap_or_else(|note| {
            skipped.push(note);
            Vec::new()
        });
        let exception = policy.exceptions.get(&subject.query.name);
        used.extend(exception.map(|_| subject.query.name.clone()));
        let (alternatives, violation) = match exception {
            Some(license) => (vec![vec![spdx::canonical(license)]], None),
            None => {
                let alternatives = alternatives(&subject, &declared);
                let violation = judge(&policy, &alternatives);
                (alternatives, violation)
            }
        };
        packages.push(Package { subject, declared, alternatives, exception: exception.is_some(), violation });
    }
    if opts.targets.is_empty() {
        for package in policy.exceptions.keys().filter(|p| !used.contains(*p)) {
            ctx.shell().note(format!(
                "[licenses] exceptions entry `{package}` matches no package{}; remove it if it is no longer needed",
                if opts.all { "" } else { " (without --all)" }
            ));
        }
    }
    skipped.sort();
    skipped.dedup();
    Ok(Report { package: pkg.name.clone(), version: pkg.version.clone(), packages, skipped, policy })
}

/// The licenses `subject` declares, or the note that says why they could not be read.
fn declared_licenses(poms: &Poms, subject: &Subject) -> std::result::Result<Vec<PomLicense>, String> {
    let query = &subject.query;
    match query.ecosystem {
        Ecosystem::Npm => Ok(subject.licenses.iter().map(|l| PomLicense { name: Some(l.clone()), url: None }).collect()),
        Ecosystem::Maven => {
            let module: ModuleId = query.name.parse().map_err(|e| format!("`{}` is not a Maven module: {e:#}", query.name))?;
            pom::licenses(poms, &module, &query.version)
                .map_err(|e| format!("could not read the licenses of {}:{}: {e:#}", query.name, query.version))
        }
    }
}

/// The alternatives the declared licenses of `subject` offer; a lone [`UNKNOWN`] when none is known.
fn alternatives(subject: &Subject, declared: &[PomLicense]) -> Vec<Vec<String>> {
    let mut alternatives: Vec<Vec<String>> = Vec::new();
    for license in declared {
        match subject.query.ecosystem {
            // Several `<license>` elements are licenses to choose from.
            Ecosystem::Maven => {
                alternatives.push(vec![spdx::from_pom(license.name.as_deref(), license.url.as_deref()).unwrap_or(UNKNOWN).to_owned()])
            }
            Ecosystem::Npm => match license.name.as_deref().and_then(spdx::parse_expression) {
                Some(parsed) => alternatives.extend(parsed),
                None => alternatives.push(vec![UNKNOWN.to_owned()]),
            },
        }
    }
    for ids in &mut alternatives {
        ids.sort();
        ids.dedup();
    }
    alternatives.sort();
    alternatives.dedup();
    // A license that cannot be told does not matter when another is there to choose.
    if alternatives.len() > 1 {
        alternatives.retain(|ids| ids != &[UNKNOWN]);
    }
    if alternatives.is_empty() {
        alternatives.push(vec![UNKNOWN.to_owned()]);
    }
    alternatives
}

/// Whether `alternatives` can be used under `policy`: the first problem of the first alternative, when
/// every one has a problem.
fn judge(policy: &LicenseSettings, alternatives: &[Vec<String>]) -> Option<Violation> {
    let listed = |list: &[String], id: &str| list.iter().any(|entry| spdx::canonical(entry).eq_ignore_ascii_case(id));
    let mut first = None;
    for ids in alternatives {
        let problem = ids.iter().find_map(|id| {
            if id == UNKNOWN {
                (!policy.allow.is_empty()).then_some(Violation::Unknown)
            } else if listed(&policy.deny, id) {
                Some(Violation::Denied(id.clone()))
            } else {
                (!policy.allow.is_empty() && !listed(&policy.allow, id)).then(|| Violation::NotAllowed(id.clone()))
            }
        });
        // An alternative without a problem is the one to use.
        let problem = problem?;
        first.get_or_insert(problem);
    }
    first
}

/// Says on stderr why each package of the reports fails the policy; returns how many do.
pub fn report_violations(shell: &Shell, reports: &[Report]) -> usize {
    let mut count = 0;
    for report in reports {
        for package in report.violations() {
            shell.error(format!("{} {}", package.coordinate(), package.problem().unwrap_or_default()));
            count += 1;
        }
    }
    count
}

/// How many packages are listed in a row of the text table before the rest is left out.
const SHOWN: usize = 3;

/// Renders the report as a table of licenses, the most used first: the license, how many packages are under
/// it and the first of them.
pub fn render(report: &Report) -> String {
    let rows: Vec<[String; 3]> = report
        .groups()
        .iter()
        .map(|group| {
            let mut packages: Vec<String> = group.packages.iter().take(SHOWN).map(|p| p.coordinate()).collect();
            if group.packages.len() > SHOWN {
                packages.push(format!("... (+{} more)", group.packages.len() - SHOWN));
            }
            [group.license.clone(), group.packages.len().to_string(), packages.join(", ")]
        })
        .collect();
    let widths: Vec<usize> = (0..2).map(|i| rows.iter().map(|row| row[i].len()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for [license, count, packages] in &rows {
        let _ = writeln!(out, "{license:<w0$}  {count:>w1$}  {packages}", w0 = widths[0], w1 = widths[1]);
    }
    out
}

/// Renders the report as a Markdown table of every package and its license.
pub fn markdown(report: &Report) -> String {
    let mut out =
        format!("## Licenses of {} v{}\n\n| Package | Version | License |\n| --- | --- | --- |\n", report.package, report.version);
    let mut packages: Vec<&Package> = report.groups().into_iter().flat_map(|g| g.packages).collect();
    packages.sort_by_key(|p| (p.license() == UNKNOWN, p.license(), &p.subject.query));
    for package in packages {
        let query = &package.subject.query;
        let _ = writeln!(out, "| `{}` | {} | {} |", display_name(query), query.version, package.license());
    }
    out
}

/// Describes a report for `kargo licenses --format json`.
pub fn json(report: &Report) -> Value {
    let groups: Vec<Value> = report
        .groups()
        .iter()
        .map(|group| {
            let packages: Vec<Value> = group
                .packages
                .iter()
                .map(|p| {
                    json!({
                        "ecosystem": p.subject.query.ecosystem.as_str(),
                        "name": p.subject.query.name,
                        "version": p.subject.query.version,
                        "declared": p.declared.iter().map(|l| json!({ "name": l.name, "url": l.url })).collect::<Vec<_>>(),
                        "path": p.subject.path,
                        "resolutions": p.subject.resolutions,
                        "tooling": p.subject.tooling,
                        "exception": p.exception,
                        "violation": p.violation.as_ref().map(Violation::as_str),
                    })
                })
                .collect();
            json!({ "license": group.license, "count": group.packages.len(), "packages": packages })
        })
        .collect();
    json!({
        "package": { "name": report.package, "version": report.version },
        "policy": { "allow": report.policy.allow, "deny": report.policy.deny, "exceptions": report.policy.exceptions },
        "licenses": groups,
        "violations": report.violations().count(),
        "skipped": report.skipped,
    })
}

/// The third-party notice file for the packages of the reports: each license, where its text is and the
/// packages under it.
pub fn notice(reports: &[Report]) -> String {
    let mut by_license: BTreeMap<String, BTreeMap<&Query, &Package>> = BTreeMap::new();
    for package in reports.iter().flat_map(|r| &r.packages) {
        by_license.entry(package.license()).or_default().insert(&package.subject.query, package);
    }
    let mut out =
        String::from("THIRD-PARTY NOTICES\n\nThis product includes software from the packages below, under the licenses named.\n");
    let unknown = by_license.remove(UNKNOWN);
    for (license, packages) in by_license.into_iter().chain(unknown.map(|p| (UNKNOWN.to_owned(), p))) {
        let rule = "=".repeat(80);
        let _ = write!(out, "\n{rule}\n{license}\n{rule}\n");
        let urls: BTreeSet<&str> = packages.values().flat_map(|p| &p.declared).filter_map(|l| l.url.as_deref()).collect();
        for url in urls {
            let _ = writeln!(out, "License text: {url}");
        }
        out.push('\n');
        for package in packages.values() {
            let _ = write!(out, "  {}", package.coordinate());
            if license == UNKNOWN {
                let _ = write!(out, " ({})", package.declared_text());
            }
            out.push('\n');
        }
    }
    out
}

/// Writes the notice file of `reports` to `path`.
pub fn write_notice(shell: &Shell, path: &Path, reports: &[Report]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, notice(reports)).with_context(|| format!("failed to write {}", path.display()))?;
    shell.status("Wrote", format!("the third-party notices to {}", path.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(ecosystem: Ecosystem, name: &str, version: &str, licenses: &[&str]) -> Subject {
        Subject {
            query: Query { ecosystem, name: name.into(), version: version.into() },
            path: vec![format!("{name}:{version}")],
            resolutions: vec!["jvm/runtime".into()],
            tooling: false,
            licenses: licenses.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    fn declared(name: &str, url: &str) -> PomLicense {
        PomLicense { name: (!name.is_empty()).then(|| name.to_owned()), url: (!url.is_empty()).then(|| url.to_owned()) }
    }

    fn package(name: &str, version: &str, alternatives: &[&[&str]], policy: &LicenseSettings) -> Package {
        let alternatives: Vec<Vec<String>> = alternatives.iter().map(|ids| ids.iter().map(|id| (*id).to_owned()).collect()).collect();
        let violation = judge(policy, &alternatives);
        Package {
            subject: subject(Ecosystem::Maven, name, version, &[]),
            declared: vec![declared("Custom License", "https://example.com/license")],
            alternatives,
            exception: false,
            violation,
        }
    }

    fn policy(allow: &[&str], deny: &[&str]) -> LicenseSettings {
        let list = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect();
        LicenseSettings { allow: list(allow), deny: list(deny), exceptions: BTreeMap::new() }
    }

    fn verdict(policy: &LicenseSettings, alternatives: &[&[&str]]) -> Option<Violation> {
        let alternatives: Vec<Vec<String>> = alternatives.iter().map(|ids| ids.iter().map(|id| (*id).to_owned()).collect()).collect();
        judge(policy, &alternatives)
    }

    #[test]
    fn maven_licenses_become_alternatives() {
        let s = subject(Ecosystem::Maven, "g:a", "1", &[]);
        let one =
            alternatives(&s, &[declared("The Apache Software License, Version 2.0", "https://www.apache.org/licenses/LICENSE-2.0.txt")]);
        assert_eq!(one, [["Apache-2.0"]]);
        let url_only = alternatives(&s, &[declared("", "https://opensource.org/licenses/MIT")]);
        assert_eq!(url_only, [["MIT"]]);
        let dual = alternatives(&s, &[declared("MIT", ""), declared("Apache 2.0", ""), declared("Apache License, Version 2.0", "")]);
        assert_eq!(dual, [vec!["Apache-2.0"], vec!["MIT"]], "licenses to choose from, each once");
        let partly = alternatives(&s, &[declared("Custom", ""), declared("MIT", "")]);
        assert_eq!(partly, [["MIT"]], "an unknown license does not matter when another is known");
        assert_eq!(alternatives(&s, &[declared("Custom", "")]), [[UNKNOWN]]);
        assert_eq!(alternatives(&s, &[]), [[UNKNOWN]], "a POM without licenses");
    }

    #[test]
    fn npm_licenses_are_spdx_expressions() {
        let s = subject(Ecosystem::Npm, "left-pad", "1.0.0", &[]);
        let of = |text: &str| alternatives(&s, &[declared(text, "")]);
        assert_eq!(of("MIT"), [["MIT"]]);
        assert_eq!(of("(MIT OR Apache-2.0)"), [vec!["Apache-2.0"], vec!["MIT"]]);
        assert_eq!(of("MIT AND Zlib"), [["MIT", "Zlib"]]);
        assert_eq!(of("SEE LICENSE IN LICENSE.txt"), [[UNKNOWN]]);
        assert_eq!(of("UNLICENSED"), [["UNLICENSED"]]);
        let legacy = alternatives(&s, &[declared("MIT", ""), declared("ISC", "")]);
        assert_eq!(legacy, [vec!["ISC"], vec!["MIT"]], "the old `licenses` array lists licenses to choose from");
    }

    #[test]
    fn the_policy_allows_denies_and_does_not_know() {
        let nothing = LicenseSettings::default();
        assert_eq!(verdict(&nothing, &[&["GPL-3.0-only"]]), None, "no policy checks nothing");
        assert_eq!(verdict(&nothing, &[&[UNKNOWN]]), None);

        let allow = policy(&["Apache-2.0", "mit"], &[]);
        assert_eq!(verdict(&allow, &[&["Apache-2.0"]]), None);
        assert_eq!(verdict(&allow, &[&["MIT"]]), None, "ids compare without regard to case");
        assert_eq!(verdict(&allow, &[&["EPL-2.0"]]), Some(Violation::NotAllowed("EPL-2.0".into())));
        assert_eq!(verdict(&allow, &[&[UNKNOWN]]), Some(Violation::Unknown), "unknown fails under an allow list");
        assert_eq!(verdict(&allow, &[&["MIT", "EPL-2.0"]]), Some(Violation::NotAllowed("EPL-2.0".into())), "AND needs every license");
        assert_eq!(verdict(&allow, &[&["EPL-2.0"], &["MIT"]]), None, "OR needs one");
        assert_eq!(verdict(&allow, &[&["EPL-2.0"], &["GPL-2.0-only"]]), Some(Violation::NotAllowed("EPL-2.0".into())));

        let deny = policy(&[], &["GPL-3.0", "AGPL-3.0-only"]);
        assert_eq!(verdict(&deny, &[&["GPL-3.0-only"]]), Some(Violation::Denied("GPL-3.0-only".into())), "deprecated spellings match");
        assert_eq!(verdict(&deny, &[&["AGPL-3.0-only"]]), Some(Violation::Denied("AGPL-3.0-only".into())));
        assert_eq!(verdict(&deny, &[&["GPL-3.0-only"], &["MIT"]]), None, "another license to choose");
        assert_eq!(verdict(&deny, &[&["EPL-2.0"]]), None, "without an allow list, only denied licenses fail");
        assert_eq!(verdict(&deny, &[&[UNKNOWN]]), None, "and unknown ones do not");
    }

    #[test]
    fn renders_groups_by_license() {
        let nothing = LicenseSettings::default();
        let report = Report {
            package: "app".into(),
            version: "1.0.0".into(),
            packages: vec![
                package("g:legacy", "1.0", &[&[UNKNOWN]], &nothing),
                package("g:a", "1", &[&["MIT"]], &nothing),
                package("g:b", "2", &[&["Apache-2.0"]], &nothing),
                package("g:c", "3", &[&["Apache-2.0"]], &nothing),
                package("g:d", "4", &[&["Apache-2.0"]], &nothing),
                package("g:e", "5", &[&["Apache-2.0"]], &nothing),
                package("g:f", "6", &[&["Apache-2.0"], &["MIT"]], &nothing),
            ],
            skipped: Vec::new(),
            policy: nothing,
        };
        let licenses: Vec<String> = report.groups().iter().map(|g| g.license.clone()).collect();
        assert_eq!(licenses, ["Apache-2.0", "Apache-2.0 OR MIT", "MIT", UNKNOWN]);
        let table = render(&report);
        assert_eq!(
            table,
            "Apache-2.0         4  g:b:2, g:c:3, g:d:4, ... (+1 more)\n\
             Apache-2.0 OR MIT  1  g:f:6\n\
             MIT                1  g:a:1\n\
             unknown            1  g:legacy:1.0\n"
        );
        let markdown = markdown(&report);
        assert!(
            markdown.starts_with(
                "## Licenses of app v1.0.0\n\n| Package | Version | License |\n| --- | --- | --- |\n| `g:b` | 2 | Apache-2.0 |\n"
            ),
            "{markdown}"
        );
        assert!(markdown.ends_with("| `g:legacy` | 1.0 | unknown |\n"), "{markdown}");
    }

    #[test]
    fn json_lists_every_package_with_its_violation() {
        let denied = policy(&[], &["GPL-3.0-only"]);
        let report = Report {
            package: "app".into(),
            version: "1.0.0".into(),
            packages: vec![package("g:gpl", "1", &[&["GPL-3.0-only"]], &denied), package("g:mit", "1", &[&["MIT"]], &denied)],
            skipped: vec!["a note".into()],
            policy: denied,
        };
        let document = json(&report);
        assert_eq!(document["violations"], 1);
        assert_eq!(document["policy"]["deny"][0], "GPL-3.0-only");
        assert_eq!(document["skipped"][0], "a note");
        let licenses = document["licenses"].as_array().unwrap();
        assert_eq!(licenses.len(), 2);
        let gpl = licenses.iter().find(|l| l["license"] == "GPL-3.0-only").unwrap();
        assert_eq!(gpl["count"], 1);
        assert_eq!(gpl["packages"][0]["violation"], "denied");
        assert_eq!(gpl["packages"][0]["ecosystem"], "maven");
        assert_eq!(gpl["packages"][0]["declared"][0]["name"], "Custom License");
        let mit = licenses.iter().find(|l| l["license"] == "MIT").unwrap();
        assert!(mit["packages"][0]["violation"].is_null());
        assert_eq!(report.violations().next().unwrap().problem().as_deref(), Some("is under GPL-3.0-only, which [licenses] deny lists"));
    }

    #[test]
    fn the_notice_lists_licenses_and_their_packages() {
        let nothing = LicenseSettings::default();
        let mut apache = package("g:a", "1", &[&["Apache-2.0"]], &nothing);
        apache.declared = vec![declared("Apache 2.0", "https://www.apache.org/licenses/LICENSE-2.0.txt")];
        let mut legacy = package("g:legacy", "1.0", &[&[UNKNOWN]], &nothing);
        legacy.declared = vec![declared("Custom License", "https://example.com/license")];
        let mut npm = package("left-pad", "1.3.0", &[&["MIT"]], &nothing);
        npm.subject = subject(Ecosystem::Npm, "left-pad", "1.3.0", &["MIT"]);
        npm.declared = vec![declared("MIT", "")];
        let first = Report {
            package: "app".into(),
            version: "1".into(),
            packages: vec![legacy, apache.clone(), npm],
            skipped: vec![],
            policy: nothing.clone(),
        };
        // A workspace member with the same package: it is listed once.
        let second = Report { package: "lib".into(), version: "1".into(), packages: vec![apache], skipped: vec![], policy: nothing };
        let rule = "=".repeat(80);
        assert_eq!(
            notice(&[first, second]),
            format!(
                "THIRD-PARTY NOTICES\n\nThis product includes software from the packages below, under the licenses named.\n\
                 \n{rule}\nApache-2.0\n{rule}\nLicense text: https://www.apache.org/licenses/LICENSE-2.0.txt\n\n  g:a:1\n\
                 \n{rule}\nMIT\n{rule}\n\n  left-pad@1.3.0\n\
                 \n{rule}\nunknown\n{rule}\nLicense text: https://example.com/license\n\n  g:legacy:1.0 (Custom License, https://example.com/license)\n"
            )
        );
    }
}
