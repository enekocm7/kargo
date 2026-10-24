//! Code actions for `kargo.toml`: quick fixes for its diagnostics, and the edits of `kargo add`,
//! `kargo add --framework`, `kargo target add` and `kargo upgrade`.
//!
//! Every edit goes through `kargo-manifest`'s format-preserving editor, as the commands do, so an
//! action and the command it mirrors leave the same `kargo.toml`. Edits that need the network
//! (picking a dependency's version) or write other files (scaffolding a target) are commands the
//! editor sends back with `workspace/executeCommand`; the server runs them (see
//! [`add_dependency`] and [`add_target`]) and applies the result with `workspace/applyEdit`.

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use kargo_manifest::TargetKind;
use kargo_manifest::edit::{DependencySection, ManifestEditor};
use kargo_util::config::Config;

use crate::schema::Fix;
use crate::{deps, manifest, schema, text};

/// Adds dependencies as `kargo add` does. Arguments: `{ uri, coords, section?, line? }`, where
/// `coords` is one coordinate or a list, `section` a dependency table such as `dev-dependencies`
/// (default `dependencies`), and `line` a zero-based line holding a half-typed dependency to replace.
pub const ADD_DEPENDENCY: &str = "kargo/addDependency";
/// Adds a target as `kargo target add` does, scaffolding its files. Arguments: `{ uri, target }`.
pub const ADD_TARGET: &str = "kargo/addTarget";
/// The commands Kargo runs for `workspace/executeCommand`.
pub const COMMANDS: &[&str] = &[ADD_DEPENDENCY, ADD_TARGET];

/// What the editor asked for code actions on.
pub struct Request<'a> {
    pub uri: &'a str,
    pub text: &'a str,
    pub path: &'a Path,
    /// The selection, in bytes.
    pub range: Range<usize>,
    /// `context.only`: the kinds of actions wanted, or empty for all.
    pub only: Vec<String>,
    /// The newer-version hints of the document.
    pub hints: &'a [deps::Hint],
    pub is_builtin_command: &'a dyn Fn(&str) -> bool,
}

impl Request<'_> {
    fn wants(&self, kind: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|o| kind == o || kind.strip_prefix(o.as_str()).is_some_and(|r| r.starts_with('.')))
    }

    /// Whether `span` is on the lines of the selection, where editors show its light bulb.
    fn touches(&self, span: &Range<usize>) -> bool {
        let start = self.range.start.min(self.text.len());
        let end = self.range.end.clamp(start, self.text.len());
        let first = self.text[..start].rfind('\n').map_or(0, |i| i + 1);
        let last = self.text[end..].find('\n').map_or(self.text.len(), |i| end + i);
        span.start <= last && first <= span.end
    }

    /// An action that replaces the document's text with `new`.
    fn edit_action(&self, title: String, kind: &str, new: &str) -> Option<Value> {
        let edit = text::edit(self.text, new)?;
        Some(json!({ "title": title, "kind": kind, "edit": { "changes": { self.uri: [edit] } } }))
    }

    /// The document after `change`, made with the manifest editor.
    fn edited(&self, change: impl FnOnce(&mut ManifestEditor) -> Result<bool>) -> Option<String> {
        let mut editor = ManifestEditor::from_str(self.path, self.text).ok()?;
        change(&mut editor).ok()?.then(|| editor.contents())
    }
}

/// The code actions for a `textDocument/codeAction` request on `kargo.toml`.
pub fn code_actions(req: &Request) -> Vec<Value> {
    let mut out = Vec::new();
    if req.wants("quickfix") {
        quick_fixes(req, &mut out);
        upgrades(req, &mut out);
        add_dependency_action(req, &mut out);
    }
    let parsed = toml_edit::Document::parse(req.text.to_owned()).is_ok();
    if parsed && !kargo_manifest::is_workspace(req.text).unwrap_or(true) {
        // The table the cursor is in, counting a header on its own line.
        let start = req.range.start.min(req.text.len());
        let line_end = req.text[start..].find('\n').map_or(req.text.len(), |i| start + i);
        let table = manifest::current_table(req.text, line_end);
        frameworks(req, &table, &mut out);
        targets(req, &table, &mut out);
    }
    out
}

/// A string, number or boolean as a TOML value.
fn toml_value(value: &Value) -> String {
    match value {
        Value::String(s) => toml_edit::Value::from(s.as_str()).to_string(),
        other => other.to_string(),
    }
}

fn quick_fixes(req: &Request, out: &mut Vec<Value>) {
    for problem in manifest::problems(req.text, req.path, req.is_builtin_command) {
        let Some(fix) = &problem.fix else { continue };
        if !req.touches(&problem.span) {
            continue;
        }
        let diagnostic = manifest::diagnostic(req.text, &problem);
        let span = problem.span.clone();
        let mut push = |title: String, start: usize, end: usize, new_text: String, preferred: bool| {
            let mut new = req.text.to_owned();
            new.replace_range(start..end, &new_text);
            if let Some(mut action) = req.edit_action(title, "quickfix", &new) {
                action["diagnostics"] = json!([diagnostic]);
                action["isPreferred"] = json!(preferred);
                out.push(action);
            }
        };
        match fix {
            Fix::RenameKey(key) => push(format!("Rename to `{key}`"), span.start, span.end, manifest::quote_key(key), true),
            Fix::ReplaceValue(values) => {
                let written = &req.text[span.clone()];
                let current = written.trim_matches(['"', '\'']);
                let closest = values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| (schema::distance(current, v), v))
                    .filter(|(d, _)| *d <= 3)
                    .min_by_key(|(d, _)| *d)
                    .map(|(_, v)| v);
                for value in values {
                    let literal = match value {
                        // Keep a literal string literal.
                        Value::String(s) if written.starts_with('\'') && !s.contains('\'') => format!("'{s}'"),
                        other => toml_value(other),
                    };
                    let shown = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
                    push(format!("Change to `{shown}`"), span.start, span.end, literal, value.as_str() == closest);
                }
            }
            Fix::AddKey { table, key, header_end, example } => {
                let dir_name = || {
                    req.path
                        .parent()
                        .and_then(Path::file_name)
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "app".to_owned())
                };
                if table.is_empty() && key == "package" {
                    let text = format!("[package]\nname = {}\nversion = \"0.1.0\"\n\n", toml_value(&json!(dir_name())));
                    push("Add a [package] table".to_owned(), 0, 0, text, true);
                    continue;
                }
                // Only under a `[header]`, not in an inline table.
                let Some(header_end) = *header_end else { continue };
                let header_start = req.text[..header_end].rfind('\n').map_or(0, |i| i + 1);
                if !req.text[header_start..].trim_start().starts_with('[') {
                    continue;
                }
                let value = match example {
                    Some(v) => v.clone(),
                    None if key == "name" && table.len() == 1 && table[0] == "package" => json!(dir_name()),
                    None => json!(""),
                };
                let line = format!("{} = {}", manifest::quote_key(key), toml_value(&value));
                let (at, new_text) = match req.text[header_end..].find('\n') {
                    Some(i) => (header_end + i + 1, format!("{line}\n")),
                    None => (req.text.len(), format!("\n{line}\n")),
                };
                push(format!("Add `{line}`"), at, at, new_text, true);
            }
        }
    }
}

fn upgrades(req: &Request, out: &mut Vec<Value>) {
    for hint in req.hints {
        let Some(span) = deps::locate(req.text, hint) else { continue };
        if !req.touches(&span) {
            continue;
        }
        let diagnostic = deps::diagnostic(req.text, hint);
        let mut versions: Vec<(&String, bool)> = Vec::new();
        if let Some(compatible) = &hint.compatible {
            versions.push((compatible, true));
        }
        if let Some(latest) = hint.latest.as_ref().filter(|l| hint.compatible.as_ref() != Some(*l)) {
            versions.push((latest, hint.compatible.is_none()));
        }
        for (version, preferred) in versions {
            let new = req.edited(|editor| {
                if editor.is_inherited(hint.section, &hint.name) {
                    return Ok(false);
                }
                editor.set_dependency_version(hint.section, &hint.module, version).map(|_| true)
            });
            let title = format!("Upgrade `{}` to {version}", hint.module);
            if let Some(mut action) = new.and_then(|new| req.edit_action(title, "quickfix", &new)) {
                action["diagnostics"] = json!([diagnostic]);
                action["isPreferred"] = json!(preferred);
                out.push(action);
            }
        }
    }
}

/// The dependency table a key path starts with, as `kargo add` writes to.
fn section_of(table: &[String]) -> Option<DependencySection> {
    match table {
        [d] if d == "dependencies" => Some(DependencySection::Main),
        [d] if d == "dev-dependencies" => Some(DependencySection::Dev),
        [t, kind, d] if t == "target" && d == "dependencies" => kind.parse().ok().map(DependencySection::Target),
        _ => None,
    }
}

fn parse_section(name: &str) -> Result<DependencySection> {
    let parts: Vec<String> = name.split('.').map(str::to_owned).collect();
    section_of(&parts).with_context(|| {
        format!("`{name}` is not a dependency table; use `dependencies`, `dev-dependencies` or `target.<target>.dependencies`")
    })
}

/// On a line that only holds a `group:artifact` in a dependency table (typed, or completed,
/// without a version): add it at its latest version, as `kargo add` does.
fn add_dependency_action(req: &Request, out: &mut Vec<Value>) {
    let start = req.range.start.min(req.text.len());
    let line_start = req.text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = req.text[start..].find('\n').map_or(req.text.len(), |i| start + i);
    let line = req.text[line_start..line_end].trim();
    let line = line.strip_suffix('=').map_or(line, str::trim_end);
    let coords = line.trim_matches(['"', '\'']);
    if coords.parse::<kargo_maven::ModuleId>().is_err() || coords.len() + 2 < line.len() {
        return;
    }
    let Some(section) = section_of(&manifest::current_table(req.text, line_start)) else { return };
    let line_number = req.text[..line_start].matches('\n').count();
    let arguments = json!([{ "uri": req.uri, "coords": coords, "section": section.display(), "line": line_number }]);
    out.push(json!({
        "title": format!("Add `{coords}` at its latest version"),
        "kind": "quickfix",
        "isPreferred": true,
        "command": { "title": "Add dependency", "command": ADD_DEPENDENCY, "arguments": arguments },
    }));
}

/// `refactor` in the table the action is about, `source` (out of the way) everywhere else.
fn kind_in(table: &[String], name: &str) -> &'static str {
    if table.first().is_some_and(|t| t == name) { "refactor" } else { "source" }
}

/// Enables each built-in framework that isn't, as `kargo add --framework` does. Ktor needs an
/// engine, so it isn't offered.
fn frameworks(req: &Request, table: &[String], out: &mut Vec<Value>) {
    let kind = kind_in(table, "frameworks");
    if !req.wants(kind) {
        return;
    }
    for name in kargo_ops::builtins::FRAMEWORK_NAMES.iter().filter(|n| **n != "ktor") {
        if let Some(new) = req.edited(|editor| editor.enable_framework(name))
            && let Some(action) = req.edit_action(format!("Enable the `{name}` framework"), kind, &new)
        {
            out.push(action);
        }
    }
}

/// Adds each target that isn't enabled, as `kargo target add` does. It scaffolds files, so the
/// server runs it as a command.
fn targets(req: &Request, table: &[String], out: &mut Vec<Value>) {
    let kind = kind_in(table, "target");
    if !req.wants(kind) {
        return;
    }
    let workspace = manifest::enclosing_workspace(req.path.parent().unwrap_or(Path::new(".")));
    let Ok(parsed) = kargo_manifest::Manifest::parse_in(req.text, workspace.as_ref()) else { return };
    for target in TargetKind::ALL.into_iter().filter(|t| !parsed.targets.is_enabled(*t)) {
        let arguments = json!([{ "uri": req.uri, "target": target.as_str() }]);
        out.push(json!({
            "title": format!("Add the `{target}` target"),
            "kind": kind,
            "command": { "title": "Add target", "command": ADD_TARGET, "arguments": arguments },
        }));
    }
}

/// The arguments of a command: the first element of `arguments`.
fn argument<'a>(params: &'a Value, name: &str) -> Option<&'a Value> {
    params["arguments"][0].get(name).filter(|v| !v.is_null())
}

/// The document a command is about.
pub fn command_uri(params: &Value) -> Result<String> {
    argument(params, "uri").and_then(Value::as_str).map(str::to_owned).context("the command needs the `uri` of a kargo.toml")
}

/// [`ADD_DEPENDENCY`] on `text`, the contents of the manifest at `path`: the new contents and a
/// summary for the reply. Picking versions may use the network.
pub fn add_dependency(config: &Config, path: &Path, text: &str, params: &Value) -> Result<(String, Value)> {
    let specs: Vec<String> = match argument(params, "coords") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => bail!("`{ADD_DEPENDENCY}` needs `coords`, such as `io.ktor:ktor-server-core`"),
    };
    let section = match argument(params, "section").and_then(Value::as_str) {
        Some(name) => parse_section(name)?,
        None => DependencySection::Main,
    };
    // The half-typed dependency the action was offered on goes, so the rest parses.
    let mut base = text.to_owned();
    if let Some(line) = argument(params, "line").and_then(Value::as_u64) {
        let start = text::offset(text, &json!({ "line": line, "character": 0 }));
        let end = text::offset(text, &json!({ "line": line + 1, "character": 0 }));
        base.replace_range(start..end, "");
    }
    let mut editor = ManifestEditor::from_str(path, &base)?;
    let opts = kargo_ops::ops::add::AddOptions { specs, section, scope: None, bom: false, prerelease: false, no_verify: false };
    let added = kargo_ops::ops::add::add_to(config, deps::repositories(&base, path), &mut editor, &opts)?;
    let workspace = manifest::enclosing_workspace(path.parent().unwrap_or(Path::new(".")));
    kargo_manifest::Manifest::parse_in(&editor.contents(), workspace.as_ref())
        .context("the edited manifest is invalid; nothing was changed")?;
    let added: Vec<&str> = added.iter().map(|(coords, _)| coords.as_str()).collect();
    Ok((editor.contents(), json!({ "added": added, "section": section.display() })))
}

/// [`ADD_TARGET`] on `text`, the contents of the manifest at `path`: the new contents, and a
/// function that scaffolds the target's files once they are applied and returns the reply.
pub fn add_target(path: &Path, text: &str, params: &Value) -> Result<(String, impl FnOnce() -> Result<Value> + use<>)> {
    let name = argument(params, "target").and_then(Value::as_str).with_context(|| format!("`{ADD_TARGET}` needs a `target`"))?;
    let target: TargetKind = name.parse()?;
    let project = kargo_ops::workspace::Project::load(path)?;
    let index = project.current().context("kargo.toml is not a package")?;
    let ws = project.package(index).clone();
    let current = ws.check_manifest(text)?;
    let mut editor = ManifestEditor::from_str(path, text)?;
    let Some(edit) = kargo_ops::ops::target::edit(&ws, &current, &mut editor, &[target], None)? else {
        bail!("the {target} target is already enabled");
    };
    let scaffold = move || {
        let created = kargo_ops::ops::target::scaffold_targets(&ws, &edit.manifest, &edit.targets, edit.lib)?;
        let created: Vec<String> = created.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        let targets: Vec<&str> = edit.targets.iter().map(|t| t.as_str()).collect();
        Ok(json!({ "targets": targets, "jvmMaterialized": edit.materialized_jvm, "created": created }))
    };
    Ok((editor.contents(), scaffold))
}

/// The path of a `file:` URI, or a clear error.
pub fn command_path(uri: &str) -> Result<PathBuf> {
    text::uri_to_path(uri).with_context(|| format!("`{uri}` is not a file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn request<'a>(uri: &'a str, text: &'a str, path: &'a Path, range: Range<usize>, hints: &'a [deps::Hint]) -> Request<'a> {
        Request { uri, text, path, range, only: Vec::new(), hints, is_builtin_command: &|_| false }
    }

    /// Applies an action's edit to `text`.
    fn apply(text: &str, action: &Value) -> String {
        let edit = &action["edit"]["changes"]["file:///p/kargo.toml"][0];
        let mut out = text.to_owned();
        text::apply_changes(&mut out, &[json!({ "range": edit["range"], "text": edit["newText"] })]);
        out
    }

    fn actions(text: &str, at: &str) -> Vec<Value> {
        let offset = text.find(at).unwrap();
        code_actions(&request("file:///p/kargo.toml", text, Path::new("/p/kargo.toml"), offset..offset, &[]))
    }

    fn titled<'a>(actions: &'a [Value], title: &str) -> &'a Value {
        actions.iter().find(|a| a["title"] == title).unwrap_or_else(|| panic!("no `{title}` in {actions:#?}"))
    }

    #[test]
    fn renames_unknown_keys() {
        let text =
            "[package]\nname = \"app\"   # the name\nversion = \"0.1.0\"\nauthor = [\"me\"]\n\n[target.jvm]\nmian-class = \"a.MainKt\"\n";
        let found = actions(text, "mian");
        let fix = titled(&found, "Rename to `main-class`");
        assert_eq!(fix["kind"], "quickfix");
        assert_eq!(fix["isPreferred"], true);
        assert!(fix["diagnostics"][0]["message"].as_str().unwrap().contains("did you mean `main-class`"));
        let fixed = apply(text, fix);
        assert_eq!(fixed, text.replace("mian-class", "main-class"));
        let fixed = apply(&fixed, titled(&actions(&fixed, "author"), "Rename to `authors`"));
        assert!(fixed.contains("authors = [\"me\"]") && fixed.contains("# the name"), "{fixed}");
        assert!(manifest::problems(&fixed, Path::new("/p/kargo.toml"), &|_| false).is_empty());
    }

    #[test]
    fn replaces_invalid_values() {
        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.wasm]\nenvironment = \"browsr\"\n";
        let found = actions(text, "browsr");
        let titles: Vec<&str> = found.iter().filter(|a| a["kind"] == "quickfix").map(|a| a["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Change to `js`", "Change to `browser`", "Change to `wasi`"]);
        let preferred: Vec<&Value> = found.iter().filter(|a| a["isPreferred"] == true).collect();
        assert_eq!(preferred.len(), 1);
        assert_eq!(apply(text, preferred[0]), text.replace("browsr", "browser"));
    }

    #[test]
    fn adds_missing_keys() {
        let text = "[package]\nname = \"app\"\n\n[[bin]]\npath = \"src/Tool.kt\"\n";
        let fixed = apply(text, titled(&actions(text, "package"), "Add `version = \"0.1.0\"`"));
        assert_eq!(fixed, "[package]\nversion = \"0.1.0\"\nname = \"app\"\n\n[[bin]]\npath = \"src/Tool.kt\"\n");
        let fixed = apply(&fixed, titled(&actions(&fixed, "[[bin]]"), "Add `name = \"tool\"`"));
        assert!(fixed.ends_with("[[bin]]\nname = \"tool\"\npath = \"src/Tool.kt\"\n"), "{fixed}");
        assert!(manifest::problems(&fixed, Path::new("/p/kargo.toml"), &|_| false).is_empty(), "{fixed}");

        let fixed = apply("", titled(&actions("", ""), "Add a [package] table"));
        assert_eq!(fixed, "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n");
    }

    #[test]
    fn enables_frameworks_like_kargo_add() {
        let text = "# app\n[package]\nname = \"app\"   # keep\nversion = \"0.1.0\"\n\n[frameworks]\ncoroutines = true\n";
        let found = actions(text, "coroutines");
        let titles: Vec<&str> = found.iter().filter(|a| a["kind"] == "refactor").map(|a| a["title"].as_str().unwrap()).collect();
        assert!(titles.contains(&"Enable the `serialization` framework"), "{titles:?}");
        assert!(!titles.iter().any(|t| t.contains("coroutines") || t.contains("ktor")), "{titles:?}");
        let fixed = apply(text, titled(&found, "Enable the `serialization` framework"));
        let mut cli = ManifestEditor::from_str(Path::new("kargo.toml"), text).unwrap();
        cli.enable_framework("serialization").unwrap();
        assert_eq!(fixed, cli.contents(), "the same file as `kargo add --framework`");
        // Outside [frameworks] they are source actions, which editors keep out of the light bulb.
        let elsewhere = actions(text, "name");
        assert_eq!(titled(&elsewhere, "Enable the `serialization` framework")["kind"], "source");
        let only =
            Request { only: vec!["quickfix".into()], ..request("file:///p/kargo.toml", text, Path::new("/p/kargo.toml"), 8..8, &[]) };
        assert!(code_actions(&only).is_empty());
    }

    #[test]
    fn offers_targets_and_dependencies_as_commands() {
        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[target.jvm]\n\n[dependencies]\n\"io.ktor:ktor-server-core\"\n";
        let valid = text.replace(
            "\"io.ktor:ktor-server-core\"
",
            "",
        );
        let found = actions(&valid, "[target.jvm]");
        let titles: Vec<&str> = found.iter().filter(|a| a["kind"] == "refactor").map(|a| a["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Add the `wasm` target", "Add the `js` target", "Add the `native` target", "Add the `android` target"]);
        assert_eq!(found.iter().find(|a| a["title"] == "Add the `wasm` target").unwrap()["command"]["arguments"][0]["target"], "wasm");

        let found = actions(text, "\"io.ktor");
        let add = titled(&found, "Add `io.ktor:ktor-server-core` at its latest version");
        assert_eq!(
            add["command"]["arguments"],
            json!([{ "uri": "file:///p/kargo.toml", "coords": "io.ktor:ktor-server-core", "section": "dependencies", "line": 7 }])
        );
        assert!(actions("[package]\nname = \"g:a\"\n", "name").iter().all(|a| a["command"]["command"] != ADD_DEPENDENCY));
    }

    #[test]
    fn upgrades_like_kargo_upgrade() {
        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"com.acme:widget\" = \"1.0.0\" # ui\n";
        let hint = deps::Hint {
            section: DependencySection::Main,
            name: "com.acme:widget".into(),
            module: "com.acme:widget".parse().unwrap(),
            compatible: Some("1.2.0".into()),
            latest: Some("2.0.0".into()),
            message: "`com.acme:widget` 1.2.0 is available (latest: 2.0.0)".into(),
        };
        let offset = text.find("widget").unwrap();
        let hints = [hint];
        let found = code_actions(&request("file:///p/kargo.toml", text, Path::new("/p/kargo.toml"), offset..offset, &hints));
        let compatible = titled(&found, "Upgrade `com.acme:widget` to 1.2.0");
        assert_eq!(compatible["isPreferred"], true);
        assert_eq!(compatible["diagnostics"][0]["severity"], 3);
        assert_eq!(apply(text, compatible), text.replace("1.0.0\" # ui", "1.2.0\" # ui"));
        assert_eq!(titled(&found, "Upgrade `com.acme:widget` to 2.0.0")["isPreferred"], false);
    }

    #[test]
    fn runs_the_commands() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo/com/acme/widget");
        for v in ["1.0", "1.1"] {
            std::fs::create_dir_all(repo.join(v)).unwrap();
            std::fs::write(repo.join(v).join(format!("widget-{v}.pom")), "<project><artifactId>widget</artifactId></project>").unwrap();
        }
        std::fs::write(
            repo.join("maven-metadata.xml"),
            "<metadata><versioning><versions><version>1.0</version><version>1.1</version></versions></versioning></metadata>",
        )
        .unwrap();
        let url = format!("file://{}", dir.path().join("repo").display()).replace('\\', "/");
        let text = format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[repositories]\nlocal = \"{url}\"\n\n[dependencies]\n\"com.acme:widget\"\n"
        );
        let path = dir.path().join("kargo.toml");
        std::fs::write(&path, &text).unwrap();
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);

        let params = json!({ "arguments": [{ "uri": "u", "coords": "com.acme:widget", "line": 8 }] });
        let (new, reply) = add_dependency(&config, &path, &text, &params).unwrap();
        assert!(new.ends_with("[dependencies]\n\"com.acme:widget\" = \"1.1\"\n"), "{new}");
        assert_eq!(reply, json!({ "added": ["com.acme:widget:1.1"], "section": "dependencies" }));
        let params = json!({ "arguments": [{ "uri": "u", "coords": "com.acme:widget", "section": "nope" }] });
        assert!(add_dependency(&config, &path, &new, &params).is_err());

        std::fs::write(&path, &new).unwrap();
        let (with_wasm, scaffold) = add_target(&path, &new, &json!({ "arguments": [{ "uri": "u", "target": "wasm" }] })).unwrap();
        assert!(with_wasm.contains("\n[target.jvm]\n") && with_wasm.contains("[target.wasm]\n"), "{with_wasm}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), new, "the manifest is the editor's to change");
        let reply = scaffold().unwrap();
        assert_eq!(reply, json!({ "targets": ["wasm"], "jvmMaterialized": true, "created": ["src-wasm/.gitkeep"] }));
        assert!(dir.path().join("src-wasm/.gitkeep").is_file());
        assert!(
            add_target(&path, &with_wasm, &json!({ "arguments": [{ "target": "wasm" }] })).err().unwrap().to_string().contains("already")
        );
    }
}
