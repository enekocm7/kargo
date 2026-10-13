//! `kargo.toml` support: diagnostics (TOML syntax, the JSON Schema, then Kargo's own
//! validation and warnings), key and value completion, and hover documentation.
//!
//! Completion and hover read the text around the cursor line by line rather than a parse tree,
//! so they keep working while the file is half typed and not valid TOML.

use std::ops::Range;
use std::path::Path;

use serde_json::{Value, json};

use crate::schema;

/// A problem in a manifest, at a byte range.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub span: Range<usize>,
    pub message: String,
    pub warning: bool,
    /// How to fix it, for a code action.
    pub fix: Option<schema::Fix>,
}

/// Every problem in the manifest at `path` with contents `text`. `is_builtin_command` names
/// Kargo's commands, for the warning about tasks they hide.
pub fn problems(text: &str, path: &Path, is_builtin_command: &dyn Fn(&str) -> bool) -> Vec<Problem> {
    let doc = match toml_edit::Document::parse(text.to_owned()) {
        Ok(doc) => doc,
        Err(e) => {
            let span = e.span().unwrap_or(0..0);
            return vec![Problem { span, message: e.message().trim().to_owned(), warning: false, fix: None }];
        }
    };
    let violations = schema::validate(&schema::from_document(&doc));
    if !violations.is_empty() {
        return violations.into_iter().map(|v| Problem { span: v.span, message: v.message, warning: false, fix: v.fix }).collect();
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let result = match kargo_manifest::is_workspace(text) {
        Ok(true) => kargo_manifest::WorkspaceManifest::parse(text, dir).map(|_| Vec::new()),
        Ok(false) => {
            let workspace = enclosing_workspace(dir);
            kargo_manifest::Manifest::parse_in(text, workspace.as_ref()).map(|m| kargo_ops::warnings::manifest(&m, is_builtin_command))
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(warnings) => {
            warnings.into_iter().map(|message| Problem { span: locate(text, &message), message, warning: true, fix: None }).collect()
        }
        Err(e) => {
            let span = e.downcast_ref::<toml::de::Error>().and_then(toml::de::Error::span);
            let message = match e.downcast_ref::<toml::de::Error>() {
                Some(de) => de.message().to_owned(),
                None => format!("{e:#}"),
            };
            let span = span.unwrap_or_else(|| locate(text, &message));
            vec![Problem { span, message, warning: false, fix: None }]
        }
    }
}

/// The LSP diagnostic of a problem.
pub fn diagnostic(text: &str, problem: &Problem) -> Value {
    json!({ "range": crate::text::range(text, problem.span.clone()), "severity": if problem.warning { 2 } else { 1 }, "source": "kargo", "message": problem.message })
}

/// The workspace manifest a package at `dir` inherits from, if one of its parents has one.
pub(crate) fn enclosing_workspace(dir: &Path) -> Option<kargo_manifest::WorkspaceManifest> {
    dir.ancestors().skip(1).find_map(|parent| {
        let path = parent.join(kargo_manifest::MANIFEST_NAME);
        match kargo_manifest::is_workspace_manifest(&path) {
            Ok(true) => kargo_manifest::WorkspaceManifest::load(&path).ok(),
            _ => None,
        }
    })
}

/// Where a message points: the first `quoted` name it mentions that appears in the text as a
/// key or a string, else the first line.
fn locate(text: &str, message: &str) -> Range<usize> {
    for quoted in message.split('`').skip(1).step_by(2) {
        let name = quoted.trim_matches(|c| c == '[' || c == ']');
        if name.is_empty() {
            continue;
        }
        for candidate in [format!("\"{name}\""), name.to_owned()] {
            let mut from = 0;
            while let Some(i) = text[from..].find(&candidate) {
                let start = from + i;
                let end = start + candidate.len();
                let word = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
                let before_ok = !text[..start].chars().next_back().is_some_and(word);
                let after_ok = !text[end..].chars().next().is_some_and(word);
                if before_ok && after_ok {
                    return start..end;
                }
                from = end;
            }
        }
    }
    0..text.find('\n').unwrap_or(text.len())
}

/// Where the cursor is, as far as the schema is concerned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Context {
    /// Inside a `[header]`: completing a table name under `parent`.
    Header { parent: Vec<String>, partial: String },
    /// Typing a key in the table at `table`.
    Key { table: Vec<String>, partial: String },
    /// Typing the value of the key at `key`; `quoted` if inside an open string.
    Value { key: Vec<String>, quoted: bool },
}

/// The key path of the last `[table]` or `[[array]]` header on the lines before `line_start`.
pub(crate) fn current_table(text: &str, line_start: usize) -> Vec<String> {
    text[..line_start]
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim_start();
            line.starts_with('[').then(|| header_path(line))
        })
        .unwrap_or_default()
}

/// The key path of a header line such as `[target.jvm]` or `[[bin]]`.
pub(crate) fn header_path(line: &str) -> Vec<String> {
    let inner = line.trim_start_matches('[');
    let end = inner.find(']').unwrap_or(inner.len());
    split_key(&inner[..end])
}

/// Splits a dotted key, honoring quoted parts.
fn split_key(key: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in key.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, '.') => parts.push(std::mem::take(&mut current)),
            (None, c) if c.is_whitespace() => {}
            (None, c) => current.push(c),
        }
    }
    parts.push(current);
    parts
}

/// What the cursor at byte `offset` is on.
pub(crate) fn context(text: &str, offset: usize) -> Context {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..offset];
    let table = current_table(text, line_start);
    if line.trim_start().starts_with('[') {
        let inner = line.trim_start().trim_start_matches('[');
        let mut path = split_key(inner);
        let partial = path.pop().unwrap_or_default();
        return Context::Header { parent: path, partial };
    }
    // Walk the line, tracking strings and inline tables, to find what the cursor is in.
    let mut stack: Vec<Vec<String>> = vec![table];
    let mut segment_start = 0;
    let mut key: Option<Vec<String>> = None;
    let mut quote: Option<char> = None;
    let mut array_depth = 0;
    for (i, c) in line.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '=' if key.is_none() => {
                let mut path = stack.last().cloned().unwrap_or_default();
                path.extend(split_key(&line[segment_start..i]));
                key = Some(path);
            }
            '[' if key.is_some() => array_depth += 1,
            ']' if key.is_some() => array_depth -= 1,
            '{' if key.is_some() => {
                stack.push(key.take().unwrap_or_default());
                segment_start = i + 1;
            }
            '}' => {
                stack.pop();
                key = None;
                segment_start = i + 1;
            }
            ',' if array_depth == 0 && stack.len() > 1 => {
                key = None;
                segment_start = i + 1;
            }
            _ => {}
        }
    }
    match key {
        Some(key) => Context::Value { key, quoted: quote.is_some() },
        None => {
            let mut path = split_key(&line[segment_start..]);
            let partial = path.pop().unwrap_or_default();
            let mut table = stack.pop().unwrap_or_default();
            table.extend(path);
            Context::Key { table, partial }
        }
    }
}

/// LSP `CompletionItemKind`s.
const KIND_PROPERTY: u32 = 10;
const KIND_MODULE: u32 = 9;
const KIND_VALUE: u32 = 12;

/// Completion items for the cursor at byte `offset`.
pub fn completion(text: &str, offset: usize) -> Vec<Value> {
    match context(text, offset) {
        Context::Header { parent, partial: _ } => schema::keys(&schema::at_path(&parent))
            .into_iter()
            .filter(|(_, node)| schema::types(node).iter().any(|t| *t == "object" || *t == "array"))
            .map(|(key, node)| item(key, KIND_MODULE, node, quote_key(key)))
            .collect(),
        Context::Key { table, partial: _ } => {
            let present = keys_in_table(text, offset, &table);
            schema::keys(&schema::at_path(&table))
                .into_iter()
                .filter(|(key, _)| !present.iter().any(|p| p == key))
                .map(|(key, node)| {
                    let insert = if schema::types(node) == ["object"] { quote_key(key) } else { format!("{} = ", quote_key(key)) };
                    item(key, KIND_PROPERTY, node, insert)
                })
                .collect()
        }
        Context::Value { key, quoted } => {
            let nodes = schema::at_path(&key);
            let mut out: Vec<Value> = Vec::new();
            for node in &nodes {
                for (value, what) in schema::values(node) {
                    let label = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if out.iter().any(|o| o["label"] == label.as_str()) {
                        continue;
                    }
                    let insert = match value {
                        Value::String(s) if !quoted => format!("\"{s}\""),
                        _ => label.clone(),
                    };
                    let detail = match what {
                        "enum" | "const" => None,
                        other => Some(other),
                    };
                    out.push(json!({ "label": label, "kind": KIND_VALUE, "insertText": insert, "detail": detail }));
                }
                if !quoted && schema::types(node).contains(&"boolean") {
                    for b in ["true", "false"] {
                        if !out.iter().any(|o| o["label"] == b) {
                            out.push(json!({ "label": b, "kind": KIND_VALUE, "insertText": b }));
                        }
                    }
                }
            }
            out
        }
    }
}

fn item(key: &str, kind: u32, node: &Value, insert: String) -> Value {
    let mut item = json!({ "label": key, "kind": kind, "insertText": insert });
    if let Some(doc) = documentation(node) {
        item["documentation"] = json!({ "kind": "markdown", "value": doc });
    }
    let types = schema::types(node);
    if !types.is_empty() {
        item["detail"] = json!(types.join(" | "));
    }
    item
}

pub(crate) fn quote_key(key: &str) -> String {
    if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { key.to_owned() } else { format!("\"{key}\"") }
}

/// Keys already written directly in the table being edited, so completion doesn't offer them again.
fn keys_in_table(text: &str, offset: usize, table: &[String]) -> Vec<String> {
    let Ok(doc) = toml_edit::Document::parse(text.to_owned()) else { return Vec::new() };
    let mut item: &toml_edit::Item = doc.as_item();
    for key in table {
        item = match item.get(key.as_str()) {
            Some(toml_edit::Item::ArrayOfTables(a)) => {
                // The element of an array of tables the cursor is in.
                match a.iter().filter(|t| t.span().is_some_and(|s| s.start <= offset)).last() {
                    Some(t) => return t.iter().map(|(k, _)| k.to_owned()).collect(),
                    None => return Vec::new(),
                }
            }
            Some(next) => next,
            None => return Vec::new(),
        };
    }
    item.as_table_like().map(|t| t.iter().map(|(k, _)| k.to_owned()).collect()).unwrap_or_default()
}

/// Markdown for a schema node: its description, type, default and allowed values.
fn documentation(node: &Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(description) = schema::description(node) {
        parts.push(description.to_owned());
    }
    let mut facts: Vec<String> = Vec::new();
    let types = schema::types(node);
    if !types.is_empty() {
        facts.push(format!("Type: `{}`", types.join(" | ")));
    }
    let values = schema::values(node);
    let render = |v: &Value| match v {
        Value::String(s) => format!("`\"{s}\"`"),
        other => format!("`{other}`"),
    };
    if let Some((default, _)) = values.iter().find(|(_, what)| *what == "default") {
        facts.push(format!("Default: {}", render(default)));
    }
    let allowed: Vec<String> = values.iter().filter(|(_, what)| *what == "enum").map(|(v, _)| render(v)).collect();
    if !allowed.is_empty() {
        facts.push(format!("One of: {}", allowed.join(", ")));
    }
    if !facts.is_empty() {
        parts.push(facts.join(" · "));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Hover documentation for the key under byte `offset`.
pub fn hover(text: &str, offset: usize) -> Option<Value> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.';
    let start = text[..offset].char_indices().rev().take_while(|(_, c)| word(*c) && *c != '.').last().map_or(offset, |(i, _)| i);
    let end = text[offset..].char_indices().find(|(_, c)| !word(*c) || *c == '.').map_or(text.len(), |(i, _)| offset + i);
    if start == end {
        return None;
    }
    // The path up to and including the hovered word, read as if completing right after it.
    let path = match context(text, end) {
        Context::Header { mut parent, partial } => {
            parent.push(partial);
            parent
        }
        Context::Key { mut table, partial } => {
            table.push(partial);
            table
        }
        Context::Value { key, .. } => key,
    };
    let nodes = schema::at_path(&path);
    let node = nodes.first()?;
    let doc = documentation(node)?;
    Some(json!({
        "contents": { "kind": "markdown", "value": format!("**`{}`**\n\n{doc}", path.join(".")) },
        "range": crate::text::range(text, start..end),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits `|` out of `text`, returning the text and the cursor offset.
    fn cursor(text: &str) -> (String, usize) {
        let offset = text.find('|').unwrap();
        (text.replacen('|', "", 1), offset)
    }

    fn labels(text: &str) -> Vec<String> {
        let (text, offset) = cursor(text);
        completion(&text, offset).iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn reads_the_cursor_context() {
        let ctx = |t: &str| {
            let (text, offset) = cursor(t);
            context(&text, offset)
        };
        assert_eq!(ctx("[package]\nna|"), Context::Key { table: vec!["package".into()], partial: "na".into() });
        assert_eq!(ctx("[target.jv|"), Context::Header { parent: vec!["target".into()], partial: "jv".into() });
        assert_eq!(
            ctx("[target.wasm]\nenvironment = \"|"),
            Context::Value { key: vec!["target".into(), "wasm".into(), "environment".into()], quoted: true }
        );
        assert_eq!(
            ctx("[dependencies]\n\"g:a\" = { version = \"1\", sc|"),
            Context::Key { table: vec!["dependencies".into(), "g:a".into()], partial: "sc".into() }
        );
        assert_eq!(
            ctx("[dependencies]\n\"g:a\" = { scope = |"),
            Context::Value { key: vec!["dependencies".into(), "g:a".into(), "scope".into()], quoted: false }
        );
        assert_eq!(ctx("target.jvm.main|"), Context::Key { table: vec!["target".into(), "jvm".into()], partial: "main".into() });
        assert_eq!(ctx("[[bin]]\nname = \"a\"\n|"), Context::Key { table: vec!["bin".into()], partial: String::new() });
    }

    #[test]
    fn completes_keys_tables_and_values() {
        let keys = labels("[package]\nname = \"a\"\n|");
        assert!(keys.contains(&"version".to_owned()) && !keys.contains(&"name".to_owned()), "{keys:?}");
        assert!(labels("[target.|").contains(&"android".to_owned()));
        assert!(labels("[|").contains(&"frameworks".to_owned()));
        assert_eq!(labels("[target.wasm]\nenvironment = |"), ["js", "browser", "wasi"]);
        let (text, offset) = cursor("[target.wasm]\nenvironment = \"|");
        assert_eq!(completion(&text, offset)[0]["insertText"], "js");
        let (text, offset) = cursor("[target.wasm]\nenvironment = |");
        assert_eq!(completion(&text, offset)[0]["insertText"], "\"js\"");
        assert!(labels("[kotlin]\nwarnings-as-errors = |").contains(&"true".to_owned()));
        assert!(labels("[dependencies]\n\"g:a\" = { scope = |").contains(&"runtime".to_owned()));
        let (text, offset) = cursor("[package]\n|");
        let version = completion(&text, offset).into_iter().find(|i| i["label"] == "version").unwrap();
        assert_eq!(version["insertText"], "version = ");
        assert!(version["documentation"]["value"].as_str().unwrap().contains("package version"));
    }

    #[test]
    fn hovers_keys() {
        let text = "[target.jvm]\nmain-class = \"a.MainKt\"\n";
        let offset = text.find("class").unwrap();
        let found = hover(text, offset).unwrap();
        let value = found["contents"]["value"].as_str().unwrap();
        assert!(value.starts_with("**`target.jvm.main-class`**") && value.contains("main class"), "{value}");
        let offset = text.find("jvm").unwrap();
        assert!(hover_text(text, offset).starts_with("**`target.jvm`**"));
        assert!(hover(text, text.find("MainKt").unwrap()).unwrap()["contents"]["value"].as_str().unwrap().contains("main-class"));
        assert!(hover("[package]\nnope = 1\n", 12).is_none());
    }

    fn hover_text(text: &str, offset: usize) -> String {
        hover(text, offset).unwrap()["contents"]["value"].as_str().unwrap().to_owned()
    }

    #[test]
    fn reports_problems_in_order_of_precedence() {
        let path = Path::new("/nowhere/kargo.toml");
        let none = |_: &str| false;
        let syntax = problems("[package\n", path, &none);
        assert_eq!(syntax.len(), 1);
        assert!(!syntax[0].warning);

        let schema = problems("[package]\nname = \"a\"\nversion = \"1\"\nnmae = 1\n", path, &none);
        assert_eq!(schema.len(), 1);
        assert!(schema[0].message.starts_with("unknown key `nmae`"), "{schema:?}");

        // Valid for the schema but not for Kargo.
        let text = "[package]\nname = \"1abc\"\nversion = \"1\"\n";
        let kargo = problems(text, path, &none);
        assert_eq!(kargo.len(), 1, "{kargo:?}");
        assert!(kargo[0].message.contains("must start with a letter"), "{kargo:?}");
        assert_eq!(&text[kargo[0].span.clone()], "\"1abc\"");

        assert_eq!(problems("[package]\nname = \"a\"\nversion = \"1\"\n", path, &none), []);
        assert_eq!(problems("[workspace]\nmembers = [\"a\"]\n", path, &none), []);
    }
}
