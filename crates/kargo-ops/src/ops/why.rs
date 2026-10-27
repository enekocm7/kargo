//! `kargo why` and `kargo tree --invert`: why a module is on a classpath, and why at its version.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use kargo_maven::resolver::{Reason, RequestSource, VersionRequest};
use kargo_maven::{Explanation, ModuleId, Resolution};

/// At most this many paths are listed with `--all-paths`.
const MAX_PATHS: usize = 100;

/// The module asked about: `group:artifact`, optionally with a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub module: ModuleId,
    pub version: Option<String>,
}

impl FromStr for Query {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        let (module, version) = match parts.as_slice() {
            [group, artifact] => (format!("{group}:{artifact}"), None),
            [group, artifact, version] if !version.is_empty() => (format!("{group}:{artifact}"), Some((*version).to_owned())),
            _ => bail!("invalid module `{s}`: expected `group:artifact` or `group:artifact:version`"),
        };
        let module = module.parse().with_context(|| format!("invalid module `{s}`"))?;
        Ok(Self { module, version })
    }
}

impl Query {
    /// Whether the resolution has the module, at the version asked for if there is one: the selected
    /// version, or one that something requested.
    pub fn matches(&self, resolution: &Resolution) -> bool {
        let Some(node) = resolution.node(&self.module) else { return false };
        match &self.version {
            None => true,
            Some(v) => {
                *v == node.version
                    || resolution
                        .nodes
                        .iter()
                        .flat_map(|n| &n.dependencies)
                        .any(|(m, requested)| *m == self.module && requested.as_deref() == Some(v.as_str()))
            }
        }
    }
}

/// Why one resolution has the module asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The resolution: `jvm`, `native-linux_x64`, ...
    pub target: String,
    pub module: ModuleId,
    /// The version on the classpath, which `kargo.lock` holds.
    pub version: String,
    /// What chose the version, when resolving again picks the same one.
    pub reason: Option<Reason>,
    /// The version resolving again picks, and why, when `kargo.lock` holds another one.
    pub fresh: Option<(String, Reason)>,
    pub requests: Vec<VersionRequest>,
    /// From a dependency of the project to the module, shortest first, as `group:artifact:version`.
    pub paths: Vec<Vec<String>>,
    /// Whether `paths` stops at [`MAX_PATHS`].
    pub truncated: bool,
}

/// Answers `query` for one resolution, or `None` when it does not have the module.
pub fn answer(target: &str, resolution: &Resolution, explanation: &Explanation, query: &Query, all_paths: bool) -> Option<Answer> {
    if !query.matches(resolution) {
        return None;
    }
    let version = resolution.node(&query.module)?.version.clone();
    let explained = explanation.modules.get(&query.module);
    let (reason, fresh) = match explained {
        Some(e) if e.version == version => (Some(e.reason.clone()), None),
        Some(e) => (None, Some((e.version.clone(), e.reason.clone()))),
        None => (None, None),
    };
    let (paths, truncated) =
        if all_paths { every_path(resolution, &query.module) } else { (shortest_path(resolution, &query.module), false) };
    let versions: HashMap<&ModuleId, &str> = resolution.nodes.iter().map(|n| (&n.module, n.version.as_str())).collect();
    let paths = paths.iter().map(|p| p.iter().map(|m| format!("{m}:{}", versions.get(m).copied().unwrap_or("?"))).collect()).collect();
    Some(Answer {
        target: target.to_owned(),
        module: query.module.clone(),
        version,
        reason,
        fresh,
        requests: explained.map(|e| e.requests.clone()).unwrap_or_default(),
        paths,
        truncated,
    })
}

/// The modules that depend on each module, with the version each asked for, in classpath order.
fn dependents(resolution: &Resolution) -> HashMap<&ModuleId, Vec<(&ModuleId, Option<&str>)>> {
    let mut out: HashMap<&ModuleId, Vec<(&ModuleId, Option<&str>)>> = HashMap::new();
    for node in &resolution.nodes {
        for (dep, requested) in &node.dependencies {
            out.entry(dep).or_default().push((&node.module, requested.as_deref()));
        }
    }
    out
}

/// The shortest path from a dependency of the project to `module` (breadth-first, so the first found).
fn shortest_path(resolution: &Resolution, module: &ModuleId) -> Vec<Vec<ModuleId>> {
    let deps: HashMap<&ModuleId, &Vec<(ModuleId, Option<String>)>> =
        resolution.nodes.iter().map(|n| (&n.module, &n.dependencies)).collect();
    let mut previous: HashMap<&ModuleId, Option<&ModuleId>> = HashMap::new();
    let mut queue: VecDeque<&ModuleId> = VecDeque::new();
    for root in &resolution.roots {
        if previous.insert(root, None).is_none() {
            queue.push_back(root);
        }
    }
    while let Some(current) = queue.pop_front() {
        if current == module {
            let mut path = vec![current.clone()];
            let mut at = current;
            while let Some(Some(p)) = previous.get(at) {
                path.push((*p).clone());
                at = p;
            }
            path.reverse();
            return vec![path];
        }
        for (child, _) in deps.get(current).map(|d| d.as_slice()).unwrap_or(&[]) {
            if !previous.contains_key(child) {
                previous.insert(child, Some(current));
                queue.push_back(child);
            }
        }
    }
    Vec::new()
}

/// Every path from a dependency of the project to `module`, shortest first, and whether there were
/// more than [`MAX_PATHS`].
fn every_path(resolution: &Resolution, module: &ModuleId) -> (Vec<Vec<ModuleId>>, bool) {
    let parents = dependents(resolution);
    let roots: HashSet<&ModuleId> = resolution.roots.iter().collect();
    let mut paths: Vec<Vec<ModuleId>> = Vec::new();
    let mut truncated = false;

    fn walk<'a>(
        at: &'a ModuleId,
        stack: &mut Vec<&'a ModuleId>,
        parents: &HashMap<&'a ModuleId, Vec<(&'a ModuleId, Option<&'a str>)>>,
        roots: &HashSet<&ModuleId>,
        paths: &mut Vec<Vec<ModuleId>>,
        truncated: &mut bool,
    ) {
        if *truncated {
            return;
        }
        stack.push(at);
        if roots.contains(at) {
            if paths.len() == MAX_PATHS {
                *truncated = true;
            } else {
                paths.push(stack.iter().rev().map(|m| (*m).clone()).collect());
            }
        }
        for (parent, _) in parents.get(at).map(|p| p.as_slice()).unwrap_or(&[]) {
            if !stack.contains(parent) {
                walk(parent, stack, parents, roots, paths, truncated);
            }
        }
        stack.pop();
    }
    walk(module, &mut Vec::new(), &parents, &roots, &mut paths, &mut truncated);
    paths.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    paths.dedup();
    (paths, truncated)
}

/// Renders the modules that depend on `module`, up to the project (`title`), as an inverted tree for
/// `kargo tree --invert`. A dependent that asked for another version than the selected one shows it.
pub fn render_inverted(title: &str, resolution: &Resolution, module: &ModuleId, max_depth: Option<usize>) -> String {
    let versions: HashMap<&ModuleId, &str> = resolution.nodes.iter().map(|n| (&n.module, n.version.as_str())).collect();
    let parents = dependents(resolution);
    let roots: HashSet<&ModuleId> = resolution.roots.iter().collect();
    let mut out = String::new();
    let _ = writeln!(out, "{module}:{}", versions.get(module).copied().unwrap_or("?"));

    struct Walk<'a> {
        title: &'a str,
        versions: HashMap<&'a ModuleId, &'a str>,
        parents: HashMap<&'a ModuleId, Vec<(&'a ModuleId, Option<&'a str>)>>,
        roots: HashSet<&'a ModuleId>,
        max_depth: Option<usize>,
        expanded: HashSet<&'a ModuleId>,
    }

    impl<'a> Walk<'a> {
        /// The lines above `module`: the modules that depend on it, and the project when it declares it.
        fn children(&self, module: &'a ModuleId) -> Vec<Option<(&'a ModuleId, Option<&'a str>)>> {
            let mut children: Vec<Option<(&ModuleId, Option<&str>)>> =
                self.parents.get(module).map(|p| p.iter().map(|p| Some(*p)).collect()).unwrap_or_default();
            if self.roots.contains(module) {
                children.push(None);
            }
            children
        }

        fn below(&mut self, out: &mut String, module: &'a ModuleId, prefix: &str, depth: usize) {
            if self.max_depth.is_some_and(|m| depth >= m) {
                return;
            }
            let children = self.children(module);
            for (i, child) in children.iter().enumerate() {
                let last = i + 1 == children.len();
                let branch = if last { "└── " } else { "├── " };
                let Some((parent, requested)) = child else {
                    let _ = writeln!(out, "{prefix}{branch}{}", self.title);
                    continue;
                };
                let selected = self.versions.get(module).copied().unwrap_or("?");
                let asked = match requested {
                    Some(r) if *r != selected => format!(" (requests {module}:{r})"),
                    _ => String::new(),
                };
                let repeated = self.expanded.contains(parent) && !self.children(parent).is_empty();
                let version = self.versions.get(parent).copied().unwrap_or("?");
                let _ = writeln!(out, "{prefix}{branch}{parent}:{version}{asked}{}", if repeated { " (*)" } else { "" });
                if repeated {
                    continue;
                }
                self.expanded.insert(parent);
                let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
                self.below(out, parent, &child_prefix, depth + 1);
            }
        }
    }

    let mut walk = Walk { title, versions, parents, roots, max_depth, expanded: HashSet::new() };
    walk.expanded.insert(module);
    walk.below(&mut out, module, "", 0);
    out
}

/// What `request` is shown as: `kargo.toml`, `g:a:1.0`, `BOM g:bom:1.0`, ...
fn source(request: &RequestSource) -> String {
    match request {
        RequestSource::Root { force: false } => "kargo.toml".to_owned(),
        RequestSource::Root { force: true } => "kargo.toml (force = true)".to_owned(),
        RequestSource::Dependency { module, version } => format!("{module}:{version}"),
        RequestSource::Relocation { module, version } => format!("{module}:{version} (relocated)"),
        RequestSource::Variant { module, version } => format!("{module}:{version} (Gradle module metadata variant)"),
        RequestSource::Constraint { module, version } => format!("{module}:{version} (dependency constraint)"),
        RequestSource::Platform { platform, platform_version, module, version } => {
            format!("platform {platform}:{platform_version} of {module}:{version}")
        }
        RequestSource::Bom { bom, version } => format!("BOM {bom}:{version}"),
    }
}

/// Why `version` was chosen, after "selected <version>, ".
fn because(reason: &Reason, requests: &[VersionRequest]) -> String {
    match reason {
        Reason::Pinned => "pinned".to_owned(),
        Reason::Forced => "forced in kargo.toml (`force = true`)".to_owned(),
        Reason::Range(ranges) => format!("the highest version that satisfies {}", ranges.join(", ")),
        Reason::Highest => {
            let distinct: HashSet<&str> = requests.iter().map(|r| r.version.as_str()).collect();
            if distinct.len() > 1 { "the highest version requested".to_owned() } else { "the only version requested".to_owned() }
        }
    }
}

/// What a rejected request lost to.
fn lost_to(answer: &Answer) -> &'static str {
    match (&answer.reason, &answer.fresh) {
        (Some(Reason::Highest), _) => "highest wins",
        (Some(Reason::Forced), _) => "force wins",
        (Some(Reason::Range(_)), _) => "range wins",
        _ => "kargo.lock",
    }
}

/// Renders an answer for the terminal. `title` names the project, as the first step of each path.
pub fn render(title: &str, configuration: &str, answer: &Answer) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}:{} ({}, {configuration})", answer.module, answer.version, answer.target);
    let selected = match (&answer.reason, &answer.fresh) {
        (Some(reason), _) => format!("selected {}, {}", answer.version, because(reason, &answer.requests)),
        (None, Some((fresh, reason))) => format!(
            "selected {}, locked in kargo.lock; resolving again picks {fresh}, {} (run `kargo update {}`)",
            answer.version,
            because(reason, &answer.requests),
            answer.module
        ),
        (None, None) => format!("selected {}, locked in kargo.lock (run `kargo update {}`)", answer.version, answer.module),
    };
    let _ = writeln!(out, "  {selected}");
    if !answer.requests.is_empty() {
        let _ = writeln!(out, "  requested:");
        let columns: Vec<(String, bool)> = answer
            .requests
            .iter()
            .map(|r| {
                // A range that the selected version satisfies did not lose anything.
                let plain = !r.version.starts_with(['[', '(']);
                if plain && r.version != answer.version {
                    (format!("{} -> {}", r.version, answer.version), true)
                } else {
                    (r.version.clone(), false)
                }
            })
            .collect();
        let width = columns.iter().map(|(c, _)| c.chars().count()).max().unwrap_or(0);
        for (request, (column, rejected)) in answer.requests.iter().zip(&columns) {
            let suffix = if *rejected { format!(" ({})", lost_to(answer)) } else { String::new() };
            let _ = writeln!(out, "    {column:<width$}  by {}{suffix}", source(&request.source));
        }
    }
    if !answer.paths.is_empty() {
        let _ = writeln!(out, "  {}:", if answer.paths.len() == 1 { "path" } else { "paths" });
        for path in &answer.paths {
            let _ = writeln!(out, "    {title} -> {}", path.join(" -> "));
        }
        if answer.truncated {
            let _ = writeln!(out, "    ... (only the first {MAX_PATHS} paths are listed)");
        }
    }
    out
}

fn reason_json(reason: &Reason) -> serde_json::Value {
    match reason {
        Reason::Pinned => serde_json::json!({ "kind": "pinned" }),
        Reason::Forced => serde_json::json!({ "kind": "forced" }),
        Reason::Range(ranges) => serde_json::json!({ "kind": "range", "ranges": ranges }),
        Reason::Highest => serde_json::json!({ "kind": "highest" }),
    }
}

fn source_json(request: &RequestSource) -> serde_json::Value {
    match request {
        RequestSource::Root { force } => serde_json::json!({ "kind": "project", "force": force }),
        RequestSource::Dependency { module, version } => {
            serde_json::json!({ "kind": "dependency", "module": module.to_string(), "version": version })
        }
        RequestSource::Relocation { module, version } => {
            serde_json::json!({ "kind": "relocation", "module": module.to_string(), "version": version })
        }
        RequestSource::Variant { module, version } => {
            serde_json::json!({ "kind": "variant", "module": module.to_string(), "version": version })
        }
        RequestSource::Constraint { module, version } => {
            serde_json::json!({ "kind": "constraint", "module": module.to_string(), "version": version })
        }
        RequestSource::Platform { platform, platform_version, module, version } => serde_json::json!({
            "kind": "platform",
            "platform": platform.to_string(),
            "platform-version": platform_version,
            "module": module.to_string(),
            "version": version,
        }),
        RequestSource::Bom { bom, version } => serde_json::json!({ "kind": "bom", "module": bom.to_string(), "version": version }),
    }
}

/// Describes an answer for `kargo why --format json`.
pub fn json(answer: &Answer) -> serde_json::Value {
    let reason = match (&answer.reason, &answer.fresh) {
        (Some(reason), _) => reason_json(reason),
        (None, Some((fresh, reason))) => {
            serde_json::json!({ "kind": "locked", "fresh-version": fresh, "fresh-reason": reason_json(reason) })
        }
        (None, None) => serde_json::json!({ "kind": "locked" }),
    };
    let requests: Vec<serde_json::Value> = answer
        .requests
        .iter()
        .map(|r| serde_json::json!({ "version": r.version, "selected": r.version == answer.version, "from": source_json(&r.source) }))
        .collect();
    serde_json::json!({
        "target": answer.target,
        "module": answer.module.to_string(),
        "version": answer.version,
        "reason": reason,
        "requests": requests,
        "paths": answer.paths,
        "truncated": answer.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_maven::resolver::{ModuleExplanation, ResolvedNode};

    fn node(m: &str, v: &str, deps: &[(&str, &str)]) -> ResolvedNode {
        ResolvedNode {
            module: m.parse().unwrap(),
            version: v.into(),
            files: vec![],
            dependencies: deps.iter().map(|(d, r)| (d.parse().unwrap(), Some((*r).to_owned()))).collect(),
        }
    }

    /// `app` and `other` both bring `shared`, in different versions; `app` also reaches it through `mid`.
    fn sample() -> Resolution {
        Resolution {
            roots: vec!["g:app".parse().unwrap(), "g:other".parse().unwrap()],
            nodes: vec![
                node("g:app", "1", &[("g:shared", "1.0"), ("g:mid", "1")]),
                node("g:other", "1", &[("g:shared", "2.0")]),
                node("g:shared", "2.0", &[]),
                node("g:mid", "1", &[("g:shared", "2.0")]),
            ],
        }
    }

    fn explanation(version: &str, reason: Reason) -> Explanation {
        let dependency = |m: &str| RequestSource::Dependency { module: m.parse().unwrap(), version: "1".into() };
        let requests = vec![
            VersionRequest { version: "1.0".into(), source: dependency("g:app") },
            VersionRequest { version: "2.0".into(), source: dependency("g:other") },
            VersionRequest { version: "2.0".into(), source: dependency("g:mid") },
        ];
        let shared = ModuleExplanation { version: version.into(), reason, requests };
        Explanation { modules: HashMap::from([("g:shared".parse().unwrap(), shared)]) }
    }

    #[test]
    fn parses_queries() {
        let q: Query = "g:a".parse().unwrap();
        assert_eq!((q.module.to_string(), q.version), ("g:a".to_owned(), None));
        let q: Query = "g:a:1.0".parse().unwrap();
        assert_eq!(q.version.as_deref(), Some("1.0"));
        assert!("g".parse::<Query>().is_err());
        assert!("g:a:1:2".parse::<Query>().is_err());
    }

    #[test]
    fn matches_selected_and_requested_versions() {
        let res = sample();
        assert!("g:shared".parse::<Query>().unwrap().matches(&res));
        assert!("g:shared:2.0".parse::<Query>().unwrap().matches(&res));
        assert!("g:shared:1.0".parse::<Query>().unwrap().matches(&res), "a rejected version is explained too");
        assert!(!"g:shared:3.0".parse::<Query>().unwrap().matches(&res));
        assert!(!"g:absent".parse::<Query>().unwrap().matches(&res));
    }

    #[test]
    fn explains_the_highest_version_with_the_shortest_path() {
        let query = "g:shared".parse().unwrap();
        let answer = answer("jvm", &sample(), &explanation("2.0", Reason::Highest), &query, false).unwrap();
        let expected = "\
g:shared:2.0 (jvm, runtime)
  selected 2.0, the highest version requested
  requested:
    1.0 -> 2.0  by g:app:1 (highest wins)
    2.0         by g:other:1
    2.0         by g:mid:1
  path:
    demo v0.1.0 -> g:app:1 -> g:shared:2.0
";
        assert_eq!(render("demo v0.1.0", "runtime", &answer), expected);
    }

    #[test]
    fn lists_every_path() {
        let query = "g:shared".parse().unwrap();
        let answer = answer("jvm", &sample(), &explanation("2.0", Reason::Highest), &query, true).unwrap();
        assert_eq!(
            answer.paths,
            [vec!["g:app:1", "g:shared:2.0"], vec!["g:other:1", "g:shared:2.0"], vec!["g:app:1", "g:mid:1", "g:shared:2.0"]]
        );
    }

    #[test]
    fn explains_a_locked_version() {
        let query = "g:shared".parse().unwrap();
        let answer = answer("jvm", &sample(), &explanation("3.0", Reason::Highest), &query, false).unwrap();
        assert_eq!(answer.fresh, Some(("3.0".to_owned(), Reason::Highest)));
        let text = render("demo", "runtime", &answer);
        assert!(text.contains("selected 2.0, locked in kargo.lock; resolving again picks 3.0"), "{text}");
        assert!(text.contains("1.0 -> 2.0  by g:app:1 (kargo.lock)"), "{text}");
        assert_eq!(
            json(&answer)["reason"],
            serde_json::json!({"kind": "locked", "fresh-version": "3.0", "fresh-reason": {"kind": "highest"}})
        );
    }

    #[test]
    fn describes_answers_as_json() {
        let query = "g:shared".parse().unwrap();
        let answer = answer("jvm", &sample(), &explanation("2.0", Reason::Forced), &query, false).unwrap();
        let value = json(&answer);
        assert_eq!(value["version"], "2.0");
        assert_eq!(value["reason"], serde_json::json!({"kind": "forced"}));
        assert_eq!(
            value["requests"][0],
            serde_json::json!({"version": "1.0", "selected": false, "from": {"kind": "dependency", "module": "g:app", "version": "1"}})
        );
        assert_eq!(value["paths"], serde_json::json!([["g:app:1", "g:shared:2.0"]]));
    }

    #[test]
    fn renders_the_inverted_tree() {
        let text = render_inverted("demo v0.1.0 (jvm)", &sample(), &"g:shared".parse().unwrap(), None);
        let expected = "\
g:shared:2.0
├── g:app:1 (requests g:shared:1.0)
│   └── demo v0.1.0 (jvm)
├── g:other:1
│   └── demo v0.1.0 (jvm)
└── g:mid:1
    └── g:app:1 (*)
";
        assert_eq!(text, expected);
        let text = render_inverted("demo", &sample(), &"g:shared".parse().unwrap(), Some(1));
        assert!(!text.contains("demo"), "{text}");
    }
}
