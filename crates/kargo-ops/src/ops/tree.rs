//! `kargo tree`: prints the resolved dependency graph.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::Result;

use super::Configuration;
use super::context::BuildContext;
use crate::npm::Origin;
use kargo_manifest::{Scope, TargetKind};
use kargo_maven::{ModuleId, Resolution};

/// A package reached through path dependencies, with the ones it reaches in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNode {
    /// Such as `mathlib v0.1.0 (/home/me/mathlib)`.
    pub label: String,
    pub children: Vec<LocalNode>,
}

/// The packages a configuration of `target` uses through path dependencies, as a tree that
/// follows the path dependencies of each one: the module graph. Their own Maven dependencies are
/// part of the resolution.
pub fn path_dependencies(ctx: &BuildContext, target: TargetKind, configuration: Configuration) -> Result<Vec<LocalNode>> {
    let scopes: &[Scope] = match configuration {
        Configuration::Compile => &[Scope::Compile, Scope::Provided],
        Configuration::Runtime => &[Scope::Compile, Scope::Runtime],
        Configuration::Test => &[Scope::Compile, Scope::Runtime, Scope::Provided],
    };
    let mut deps = ctx.local_dependencies(target, false)?;
    if configuration == Configuration::Test {
        deps.extend(ctx.local_dependencies(target, true)?);
    }
    let included: Vec<usize> = deps.iter().filter(|d| scopes.contains(&d.scope)).map(|d| d.package).collect();
    let project = ctx.project;
    fn node(
        project: &crate::workspace::Project,
        package: usize,
        target: TargetKind,
        included: &[usize],
        seen: &mut Vec<usize>,
    ) -> LocalNode {
        let pkg = project.package(package);
        let mut label = format!("{} v{} ({})", pkg.name(), pkg.manifest().package.version, pkg.root().display());
        let children: Vec<usize> =
            pkg.path_dependencies().iter().filter(|(d, i)| d.applies_to(target, false) && included.contains(i)).map(|(_, i)| *i).collect();
        if seen.contains(&package) && !children.is_empty() {
            label.push_str(" (*)");
            return LocalNode { label, children: Vec::new() };
        }
        seen.push(package);
        let children = children.into_iter().map(|c| node(project, c, target, included, seen)).collect();
        LocalNode { label, children }
    }
    let (mut seen, mut top) = (Vec::new(), Vec::new());
    let mut out = Vec::new();
    for dev in [false, true] {
        if dev && configuration != Configuration::Test {
            break;
        }
        for (dep, index) in ctx.ws.path_dependencies() {
            if dep.applies_to(target, dev) && included.contains(index) && *index != ctx.package && !top.contains(index) {
                top.push(*index);
                out.push(node(project, *index, target, &included, &mut seen));
            }
        }
    }
    Ok(out)
}

/// Renders a resolution as a Cargo-style tree, after the `locals` (path dependency) lines.
/// Modules already printed are marked `(*)`; version conflicts show `requested -> selected`.
pub fn render(title: &str, locals: &[LocalNode], resolution: &Resolution, max_depth: Option<usize>) -> String {
    let versions: HashMap<&ModuleId, &str> = resolution.nodes.iter().map(|n| (&n.module, n.version.as_str())).collect();
    let deps: HashMap<&ModuleId, &Vec<(ModuleId, Option<String>)>> =
        resolution.nodes.iter().map(|n| (&n.module, &n.dependencies)).collect();

    let mut out = String::new();
    let _ = writeln!(out, "{title}");
    let mut expanded: HashSet<ModuleId> = HashSet::new();

    #[allow(clippy::too_many_arguments)]
    fn walk(
        out: &mut String,
        module: &ModuleId,
        requested: Option<&str>,
        prefix: &str,
        last: bool,
        depth: usize,
        max_depth: Option<usize>,
        versions: &HashMap<&ModuleId, &str>,
        deps: &HashMap<&ModuleId, &Vec<(ModuleId, Option<String>)>>,
        expanded: &mut HashSet<ModuleId>,
    ) {
        let selected = versions.get(module).copied().unwrap_or("?");
        let version = match requested {
            Some(r) if r != selected && !r.starts_with('[') => format!("{r} -> {selected}"),
            _ => selected.to_owned(),
        };
        let branch = if last { "└── " } else { "├── " };
        let children = deps.get(module).map(|d| d.as_slice()).unwrap_or(&[]);
        let repeated = !children.is_empty() && expanded.contains(module);
        let _ = writeln!(out, "{prefix}{branch}{module}:{version}{}", if repeated { " (*)" } else { "" });
        if repeated || max_depth.is_some_and(|m| depth >= m) {
            return;
        }
        expanded.insert(module.clone());
        let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
        for (i, (child, req)) in children.iter().enumerate() {
            walk(out, child, req.as_deref(), &child_prefix, i + 1 == children.len(), depth + 1, max_depth, versions, deps, expanded);
        }
    }

    fn walk_local(out: &mut String, node: &LocalNode, prefix: &str, last: bool) {
        let _ = writeln!(out, "{prefix}{}{}", if last { "└── " } else { "├── " }, node.label);
        let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
        for (i, child) in node.children.iter().enumerate() {
            walk_local(out, child, &child_prefix, i + 1 == node.children.len());
        }
    }
    for (i, local) in locals.iter().enumerate() {
        walk_local(&mut out, local, "", i + 1 == locals.len() && resolution.roots.is_empty());
    }
    for (i, root) in resolution.roots.iter().enumerate() {
        walk(&mut out, root, None, "", i + 1 == resolution.roots.len(), 1, max_depth, &versions, &deps, &mut expanded);
    }
    out
}

/// Renders the npm packages of the Wasm or JS target below its tree: each with the version that gets
/// installed and where it comes from (the libraries that ask for it, and `kargo.toml`, which wins). Empty
/// without packages.
pub fn render_npm(origins: &[Origin]) -> String {
    if origins.is_empty() {
        return String::new();
    }
    let mut out = String::from("npm packages\n");
    for (i, origin) in origins.iter().enumerate() {
        let branch = if i + 1 == origins.len() { "└── " } else { "├── " };
        let _ = writeln!(out, "{branch}{} {} ({})", origin.name, origin.version, describe_npm(origin));
    }
    out
}

fn describe_npm(origin: &Origin) -> String {
    let asked: Vec<String> = origin
        .requested_by
        .iter()
        .map(|r| {
            if r.spec == origin.version {
                format!("{}:{}", r.module, r.version)
            } else {
                format!("{}:{} wants {}", r.module, r.version, r.spec)
            }
        })
        .collect();
    let declared = if origin.dev { "kargo.toml, tests only" } else { "kargo.toml" };
    match (&origin.declared, asked.is_empty()) {
        (Some(_), true) => declared.to_owned(),
        (Some(_), false) => format!("{declared}; also asked for by {}", asked.join(", ")),
        (None, _) => format!("asked for by {}", asked.join(", ")),
    }
}

/// The npm packages of a tree for `kargo tree --format json` and `kargo metadata`: the version installed,
/// the one `kargo.toml` declares for it and the libraries that ask for it.
pub fn json_npm(origins: &[Origin]) -> serde_json::Value {
    origins
        .iter()
        .map(|o| {
            serde_json::json!({
                "name": o.name,
                "version": o.version,
                "declared": o.declared,
                "dev": o.dev,
                "requested-by": o.requested_by.iter().map(|r| serde_json::json!({ "module": r.module, "version": r.version, "spec": r.spec })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// Describes a resolution for `kargo tree --format json`: the roots plus every
/// resolved module once, each listing its direct dependencies (a graph, not a tree,
/// so shared modules are not repeated).
pub fn json(target: &str, resolution: &Resolution) -> serde_json::Value {
    let versions: HashMap<&ModuleId, &str> = resolution.nodes.iter().map(|n| (&n.module, n.version.as_str())).collect();
    let nodes: Vec<serde_json::Value> = resolution
        .nodes
        .iter()
        .map(|n| {
            let dependencies: Vec<serde_json::Value> = n
                .dependencies
                .iter()
                .map(|(m, requested)| {
                    serde_json::json!({
                        "id": m.to_string(),
                        "requested": requested,
                        "version": versions.get(m).copied(),
                    })
                })
                .collect();
            serde_json::json!({
                "id": n.module.to_string(),
                "group": n.module.group,
                "artifact": n.module.artifact,
                "version": n.version,
                "files": n.files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
                "dependencies": dependencies,
            })
        })
        .collect();
    serde_json::json!({
        "target": target,
        "roots": resolution.roots.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "modules": nodes,
    })
}

/// Renders `resolution` with duplicates suppressed (for `--duplicates`): lists
/// modules that were requested in more than one version.
pub fn conflicts(resolution: &Resolution) -> Result<Vec<String>> {
    let mut requested: HashMap<&ModuleId, HashSet<&str>> = HashMap::new();
    for node in &resolution.nodes {
        for (dep, req) in &node.dependencies {
            if let Some(r) = req {
                requested.entry(dep).or_default().insert(r.as_str());
            }
        }
    }
    let mut out: Vec<String> = requested
        .into_iter()
        .filter(|(_, reqs)| reqs.len() > 1)
        .map(|(m, reqs)| {
            let mut reqs: Vec<&str> = reqs.into_iter().collect();
            reqs.sort();
            let selected = resolution.node(m).map(|n| n.version.as_str()).unwrap_or("?");
            format!("{m}: requested {} -> selected {selected}", reqs.join(", "))
        })
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_maven::resolver::ResolvedNode;

    fn node(m: &str, v: &str, deps: &[(&str, &str)]) -> ResolvedNode {
        ResolvedNode {
            module: m.parse().unwrap(),
            version: v.into(),
            files: vec![],
            dependencies: deps.iter().map(|(d, r)| (d.parse().unwrap(), Some((*r).to_owned()))).collect(),
        }
    }

    fn sample() -> Resolution {
        Resolution {
            roots: vec!["g:app".parse().unwrap(), "g:other".parse().unwrap()],
            nodes: vec![
                node("g:app", "1", &[("g:shared", "1.0"), ("g:leaf", "1")]),
                node("g:other", "1", &[("g:shared", "2.0")]),
                node("g:shared", "2.0", &[("g:leaf", "1")]),
                node("g:leaf", "1", &[]),
            ],
        }
    }

    #[test]
    fn renders_tree() {
        let text = render("demo (jvm)", &[], &sample(), None);
        let expected = "\
demo (jvm)
├── g:app:1
│   ├── g:shared:1.0 -> 2.0
│   │   └── g:leaf:1
│   └── g:leaf:1
└── g:other:1
    └── g:shared:2.0 (*)
";
        assert_eq!(text, expected);
    }

    #[test]
    fn renders_the_module_graph_first() {
        let leaf = |label: &str| LocalNode { label: label.to_owned(), children: Vec::new() };
        let locals = vec![
            LocalNode { label: "core v0.1.0 (/ws/core)".to_owned(), children: vec![leaf("util v0.1.0 (/ws/util)")] },
            leaf("util v0.1.0 (/ws/util)"),
        ];
        let text = render("server v0.1.0 (jvm)", &locals, &sample(), Some(0));
        let expected = "\
server v0.1.0 (jvm)
├── core v0.1.0 (/ws/core)
│   └── util v0.1.0 (/ws/util)
├── util v0.1.0 (/ws/util)
├── g:app:1
└── g:other:1
";
        assert_eq!(text, expected);
    }

    #[test]
    fn depth_limit() {
        let text = render("t", &[], &sample(), Some(1));
        assert!(!text.contains("g:shared"), "{text}");
        assert!(text.contains("g:other:1"));
    }

    #[test]
    fn describes_graph_as_json() {
        let value = json("jvm", &sample());
        assert_eq!(value["target"], "jvm");
        assert_eq!(value["roots"], serde_json::json!(["g:app", "g:other"]));
        let modules = value["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 4);
        assert_eq!(modules[0]["id"], "g:app");
        assert_eq!(modules[0]["group"], "g");
        assert_eq!(modules[0]["dependencies"][0], serde_json::json!({"id": "g:shared", "requested": "1.0", "version": "2.0"}));
    }

    #[test]
    fn renders_where_npm_packages_come_from() {
        use crate::npm::LibraryRequest;
        let ask =
            |module: &str, version: &str, spec: &str| LibraryRequest { module: module.into(), version: version.into(), spec: spec.into() };
        let origins = vec![
            Origin {
                name: "@js-joda/core".into(),
                version: "^3.2.0".into(),
                declared: None,
                dev: false,
                requested_by: vec![ask("org.acme:datetime", "0.7.1", "^3.2.0"), ask("org.acme:other", "1.0", "^3.1.0")],
            },
            Origin { name: "left-pad".into(), version: "^1.3.0".into(), declared: Some("^1.3.0".into()), dev: false, requested_by: vec![] },
            Origin {
                name: "mocha".into(),
                version: "^10.0.0".into(),
                declared: Some("^10.0.0".into()),
                dev: true,
                requested_by: vec![ask("org.acme:test", "2", "^9.0.0")],
            },
        ];
        let expected = "\
npm packages
├── @js-joda/core ^3.2.0 (asked for by org.acme:datetime:0.7.1, org.acme:other:1.0 wants ^3.1.0)
├── left-pad ^1.3.0 (kargo.toml)
└── mocha ^10.0.0 (kargo.toml, tests only; also asked for by org.acme:test:2 wants ^9.0.0)
";
        assert_eq!(render_npm(&origins), expected);
        assert_eq!(render_npm(&[]), "");
        let json = json_npm(&origins);
        assert_eq!(json[0]["requested-by"][1], serde_json::json!({ "module": "org.acme:other", "version": "1.0", "spec": "^3.1.0" }));
        assert_eq!(json[1]["declared"], "^1.3.0");
        assert_eq!(json[0]["declared"], serde_json::Value::Null);
        assert_eq!(json[2]["dev"], true);
    }

    #[test]
    fn lists_conflicts() {
        assert_eq!(conflicts(&sample()).unwrap(), ["g:shared: requested 1.0, 2.0 -> selected 2.0"]);
    }
}
