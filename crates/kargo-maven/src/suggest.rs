//! Errors for a coordinate or a version that does not exist, with the real ones as suggestions: the artifacts in the
//! group's directory of the repositories ("did you mean `kotlinx-coroutines-core`?") and the versions near the one asked for.

use std::fmt::Write as _;

use anyhow::Error;
use kargo_util::help;

use crate::coord::ModuleId;
use crate::repo::MavenClient;
use crate::version::Version;

/// How many versions an error lists.
const LISTED_VERSIONS: usize = 6;

impl MavenClient {
    /// Modules a mistyped `module` may have been meant to be: the artifacts of its group that are close to its
    /// artifact name, or, when the group does not exist either, those of the closest existing group.
    pub fn similar_modules(&self, module: &ModuleId) -> Vec<ModuleId> {
        let in_group = |group: &str| -> Vec<ModuleId> {
            let artifacts = self.list_directories(&group.replace('.', "/"));
            help::nearest(&module.artifact, &artifacts, 3)
                .into_iter()
                .map(|a| ModuleId { group: group.to_owned(), artifact: a.to_owned() })
                .collect()
        };
        let found = in_group(&module.group);
        if !found.is_empty() {
            return found;
        }
        // The group may be the typo: fix the first segment that is not a directory of its parent.
        let mut fixed: Vec<String> = Vec::new();
        for (i, segment) in module.group.split('.').enumerate() {
            if i == 0 {
                fixed.push(segment.to_owned());
                continue;
            }
            let children = self.list_directories(&fixed.join("/"));
            if children.iter().any(|c| c == segment) {
                fixed.push(segment.to_owned());
            } else if let Some(close) = help::closest(segment, &children) {
                fixed.push(close.to_owned());
            } else {
                return Vec::new();
            }
        }
        let group = fixed.join(".");
        if group == module.group { Vec::new() } else { in_group(&group) }
    }

    /// The error for a `module` no repository knows: with the closest real coordinates, when there are any.
    pub fn module_not_found(&self, module: &ModuleId) -> Error {
        let message = format!("`{module}` was not found in any repository");
        let similar = self.similar_modules(module);
        match similar.first() {
            Some(_) => {
                let list: Vec<String> = similar.iter().map(|m| format!("`{m}`")).collect();
                let help = if similar.len() == 1 {
                    format!("did you mean {}?", list[0])
                } else {
                    format!("did you mean one of {}?", list.join(", "))
                };
                help::helpful(message, help, similar.iter().map(ToString::to_string).collect::<Vec<_>>())
            }
            None => anyhow::anyhow!(message),
        }
    }

    /// The error for `module:version` when no repository has that version: the versions near it, and a note on the
    /// pre-releases among them.
    pub fn version_not_found(&self, module: &ModuleId, version: &str) -> Error {
        self.versions_error(module, version, format!("`{module}:{version}` was not found in any repository"))
    }

    /// The error for a requirement (`[1.5,2)`) that no published version of `module` satisfies.
    pub fn no_version_matches(&self, module: &ModuleId, requirement: &str) -> Error {
        self.versions_error(module, requirement, format!("no version matches `{requirement}` of `{module}`"))
    }

    fn versions_error(&self, module: &ModuleId, wanted: &str, message: String) -> Error {
        let versions = self.versions(module).unwrap_or_default();
        if versions.is_empty() {
            return self.module_not_found(module);
        }
        let nearby = nearby_versions(&versions, wanted);
        let shown: Vec<String> =
            nearby.iter().map(|v| if v.is_prerelease() { format!("{v} (pre-release)") } else { v.to_string() }).collect();
        let note = if nearby.iter().any(|v| v.is_prerelease()) { "; a pre-release needs its exact version" } else { "" };
        let mut help = format!("available: {}{note}", shown.join(", "));
        if nearby.len() < versions.len() {
            let _ = write!(help, " ({} versions in all)", versions.len());
        }
        help::helpful(message, help, nearby.iter().map(ToString::to_string).collect())
    }
}

/// Up to [`LISTED_VERSIONS`] of the sorted `versions`, the ones around where `wanted` would be.
fn nearby_versions<'a>(versions: &'a [Version], wanted: &str) -> Vec<&'a Version> {
    let digits = wanted.trim_start_matches(|c: char| !c.is_ascii_digit());
    let digits = digits.split([',', ']', ')']).next().unwrap_or(digits);
    let at = if digits.is_empty() { versions.len() } else { versions.partition_point(|v| *v < Version::parse(digits)) };
    let start = at.saturating_sub(LISTED_VERSIONS / 2).min(versions.len().saturating_sub(LISTED_VERSIONS));
    versions.iter().skip(start).take(LISTED_VERSIONS).collect()
}

/// The directories an HTML listing links to (`<a href="kotlinx-coroutines-core/">`).
pub(crate) fn directories_in(page: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = page;
    while let Some(at) = rest.find("href=\"") {
        rest = &rest[at + "href=\"".len()..];
        let Some(end) = rest.find('"') else { break };
        let link = &rest[..end];
        rest = &rest[end..];
        if let Some(name) = link.strip_suffix('/')
            && !name.is_empty()
            && !name.starts_with(['.', '/', '?', '#'])
            && !name.contains(['/', ':', '?'])
        {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_directories_of_an_html_listing() {
        let page = r#"<a href="../">../</a><a href="kotlinx-coroutines-core/" title="x">kotlinx-coroutines-core/</a>
            <a href="kotlinx-coroutines-test/">t</a><a href="maven-metadata.xml">m</a><a href="https://example.com/">e</a>"#;
        assert_eq!(directories_in(page), ["kotlinx-coroutines-core", "kotlinx-coroutines-test"]);
    }

    #[test]
    fn lists_the_versions_around_the_one_asked_for() {
        let versions: Vec<Version> =
            ["1.0.0", "1.1.0", "1.2.0", "1.3.0", "1.4.2", "1.5.0-RC1", "2.0.0", "2.1.0"].map(Version::parse).to_vec();
        let near: Vec<String> = nearby_versions(&versions, "1.5").iter().map(ToString::to_string).collect();
        assert_eq!(near, ["1.2.0", "1.3.0", "1.4.2", "1.5.0-RC1", "2.0.0", "2.1.0"]);
        let near: Vec<String> = nearby_versions(&versions, "[1.5,2)").iter().map(ToString::to_string).collect();
        assert_eq!(near.len(), 6);
        let few: Vec<Version> = ["1.0"].map(Version::parse).to_vec();
        assert_eq!(nearby_versions(&few, "9").len(), 1);
    }
}
