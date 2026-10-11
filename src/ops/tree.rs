//! `kargo tree`: prints the resolved dependency graph.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::Result;

use crate::maven::{ModuleId, Resolution};

/// Renders a resolution as a Cargo-style tree. Modules already printed are
/// marked `(*)`; version conflicts show `requested -> selected`.
pub fn render(title: &str, resolution: &Resolution, max_depth: Option<usize>) -> String {
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

    for (i, root) in resolution.roots.iter().enumerate() {
        walk(&mut out, root, None, "", i + 1 == resolution.roots.len(), 1, max_depth, &versions, &deps, &mut expanded);
    }
    out
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
    use crate::maven::resolver::ResolvedNode;

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
        let text = render("demo (jvm)", &sample(), None);
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
    fn depth_limit() {
        let text = render("t", &sample(), Some(1));
        assert!(!text.contains("g:shared"), "{text}");
        assert!(text.contains("g:other:1"));
    }

    #[test]
    fn lists_conflicts() {
        assert_eq!(conflicts(&sample()).unwrap(), ["g:shared: requested 1.0, 2.0 -> selected 2.0"]);
    }
}
