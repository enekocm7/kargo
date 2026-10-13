//! The `kargo.toml` JSON Schema (`website/public/schemas/kargo.json`, compiled in): looking up
//! the schema of a key path for completion and hover, and validating a parsed manifest.
//!
//! Only the keywords the schema uses are supported: `$ref` to local definitions, `type`,
//! `enum`, `const`, `properties`, `additionalProperties`, `required`, `items`, `anyOf`/`oneOf`,
//! `not`, `minLength`, `minimum`, `minItems` and `maxProperties`. `pattern` is not checked;
//! Kargo's own validation reports those errors.

use std::ops::Range;
use std::sync::OnceLock;

use serde_json::Value;

/// The schema document.
pub fn root() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA
        .get_or_init(|| serde_json::from_str(include_str!("../../../website/public/schemas/kargo.json")).expect("the schema is valid JSON"))
}

/// Follows `$ref`s to local definitions.
pub fn resolve(node: &Value) -> &Value {
    let mut node = node;
    while let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        match reference.strip_prefix("#/definitions/").and_then(|name| root()["definitions"].get(name)) {
            Some(target) => node = target,
            None => break,
        }
    }
    node
}

/// The node itself and, recursively, its `anyOf`/`oneOf` alternatives.
fn alternatives(node: &Value) -> Vec<&Value> {
    let node = resolve(node);
    let mut out = vec![node];
    for keyword in ["anyOf", "oneOf"] {
        for alt in node.get(keyword).and_then(Value::as_array).into_iter().flatten() {
            out.extend(alternatives(alt));
        }
    }
    out
}

/// The schemas a key can have under `parents`. Arrays are stepped into, so `bin` and its
/// `[[bin]]` elements share their keys.
pub fn child<'a>(parents: &[&'a Value], key: &str) -> Vec<&'a Value> {
    let mut out: Vec<&Value> = Vec::new();
    for node in parents.iter().flat_map(|p| alternatives(p)).flat_map(elements) {
        let found = match node.get("properties").and_then(|p| p.get(key)) {
            Some(prop) => Some(prop),
            None => node.get("additionalProperties").filter(|a| a.is_object()),
        };
        if let Some(found) = found
            && !out.iter().any(|o| std::ptr::eq(*o, found))
        {
            out.push(found);
        }
    }
    out
}

/// A node, or the schema of its elements if it is an array.
fn elements(node: &Value) -> Vec<&Value> {
    match node.get("items") {
        Some(items) => alternatives(items),
        None => vec![node],
    }
}

/// The schemas of a key path, starting at the root; empty if the schema doesn't know it.
pub fn at_path<S: AsRef<str>>(path: &[S]) -> Vec<&'static Value> {
    let mut nodes = vec![root()];
    for key in path {
        nodes = child(&nodes, key.as_ref());
        if nodes.is_empty() {
            break;
        }
    }
    nodes
}

/// The keys `nodes` declare, each with its schema, in the schema's order.
pub fn keys<'a>(nodes: &[&'a Value]) -> Vec<(&'a str, &'a Value)> {
    let mut out: Vec<(&str, &Value)> = Vec::new();
    for node in nodes.iter().flat_map(|n| alternatives(n)).flat_map(elements) {
        for (key, prop) in node.get("properties").and_then(Value::as_object).into_iter().flatten() {
            if !out.iter().any(|(k, _)| k == key) {
                out.push((key, prop));
            }
        }
    }
    out
}

/// The first `description` of a node or its alternatives.
pub fn description(node: &Value) -> Option<&str> {
    node.get("description").and_then(Value::as_str).or_else(|| alternatives(node).into_iter().find_map(|n| n["description"].as_str()))
}

/// The JSON types a node allows (`string`, `object`, ...), from `type`, `enum` and alternatives.
pub fn types(node: &Value) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for alt in alternatives(node) {
        let mut add = |t: &'static str| {
            if !out.contains(&t) {
                out.push(t);
            }
        };
        match &alt["type"] {
            Value::String(t) => add(type_name(t)),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).for_each(|t| add(type_name(t))),
            _ => {}
        }
        for value in alt.get("enum").and_then(Value::as_array).into_iter().flatten() {
            add(json_type(value));
        }
    }
    out
}

fn type_name(t: &str) -> &'static str {
    match t {
        "string" => "string",
        "integer" => "integer",
        "number" => "number",
        "boolean" => "boolean",
        "array" => "array",
        "object" => "object",
        _ => "null",
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

/// The values a node lists in `enum`, `const`, `default` and `examples`, enum values first.
pub fn values(node: &Value) -> Vec<(&Value, &'static str)> {
    let mut out: Vec<(&Value, &str)> = Vec::new();
    for alt in alternatives(node).into_iter().flat_map(elements) {
        let lists = [("enum", alt.get("enum")), ("examples", alt.get("examples"))];
        for (what, list) in lists {
            for v in list.and_then(Value::as_array).into_iter().flatten() {
                if !out.iter().any(|(o, _)| *o == v) {
                    out.push((v, what));
                }
            }
        }
        for (what, v) in [("const", alt.get("const")), ("default", alt.get("default"))] {
            if let Some(v) = v
                && !v.is_object()
                && !v.is_array()
                && !out.iter().any(|(o, _)| *o == v)
            {
                out.push((v, what));
            }
        }
    }
    out
}

/// A TOML value with the byte range it was written at.
#[derive(Debug, Clone)]
pub struct Spanned {
    pub value: Tv,
    pub span: Range<usize>,
}

/// A TOML value, as the validator sees it.
#[derive(Debug, Clone)]
pub enum Tv {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Datetime,
    Array(Vec<Spanned>),
    /// Entries with the range of their key.
    Table(Vec<(String, Range<usize>, Spanned)>),
}

impl Tv {
    fn type_name(&self) -> &'static str {
        match self {
            Tv::String(_) => "string",
            Tv::Integer(_) => "integer",
            Tv::Float(_) => "number",
            Tv::Boolean(_) => "boolean",
            Tv::Datetime => "datetime",
            Tv::Array(_) => "array",
            Tv::Table(_) => "object",
        }
    }

    fn matches(&self, ty: &str) -> bool {
        match (self, ty) {
            (Tv::Integer(_), "number") => true,
            _ => self.type_name() == ty,
        }
    }

    fn equals(&self, json: &Value) -> bool {
        match (self, json) {
            (Tv::String(a), Value::String(b)) => a == b,
            (Tv::Integer(a), Value::Number(b)) => b.as_i64() == Some(*a),
            (Tv::Float(a), Value::Number(b)) => b.as_f64() == Some(*a),
            (Tv::Boolean(a), Value::Bool(b)) => a == b,
            _ => false,
        }
    }
}

/// Converts a parsed `toml_edit` document into [`Spanned`] values.
pub fn from_document(doc: &toml_edit::Document<String>) -> Spanned {
    table(doc.as_table(), 0..0)
}

fn table(table: &toml_edit::Table, fallback: Range<usize>) -> Spanned {
    let span = table.span().unwrap_or(fallback);
    let entries = table_entries(table.iter().map(|(k, _)| k).filter_map(|k| table.get_key_value(k)), &span);
    Spanned { value: Tv::Table(entries), span }
}

fn table_entries<'a>(
    pairs: impl Iterator<Item = (&'a toml_edit::Key, &'a toml_edit::Item)>,
    span: &Range<usize>,
) -> Vec<(String, Range<usize>, Spanned)> {
    pairs
        .filter_map(|(key, item)| {
            let key_span = key.span().unwrap_or_else(|| span.clone());
            item_value(item, key_span.clone()).map(|v| (key.get().to_owned(), key_span, v))
        })
        .collect()
}

fn item_value(item: &toml_edit::Item, fallback: Range<usize>) -> Option<Spanned> {
    match item {
        toml_edit::Item::None => None,
        toml_edit::Item::Value(v) => Some(value(v, fallback)),
        toml_edit::Item::Table(t) => Some(table(t, fallback)),
        toml_edit::Item::ArrayOfTables(a) => {
            let span = a.span().unwrap_or_else(|| fallback.clone());
            Some(Spanned { value: Tv::Array(a.iter().map(|t| table(t, span.clone())).collect()), span })
        }
    }
}

fn value(v: &toml_edit::Value, fallback: Range<usize>) -> Spanned {
    let span = v.span().unwrap_or(fallback);
    let value = match v {
        toml_edit::Value::String(s) => Tv::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Tv::Integer(*i.value()),
        toml_edit::Value::Float(f) => Tv::Float(*f.value()),
        toml_edit::Value::Boolean(b) => Tv::Boolean(*b.value()),
        toml_edit::Value::Datetime(_) => Tv::Datetime,
        toml_edit::Value::Array(a) => Tv::Array(a.iter().map(|v| value(v, span.clone())).collect()),
        toml_edit::Value::InlineTable(t) => Tv::Table(table_entries(t.iter().map(|(k, _)| k).filter_map(|k| t.get_key_value(k)), &span)),
    };
    Spanned { value, span }
}

/// A schema violation.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub span: Range<usize>,
    pub message: String,
    /// How to fix it, when the schema knows.
    pub fix: Option<Fix>,
}

/// A fix for a [`Violation`].
#[derive(Debug, Clone, PartialEq)]
pub enum Fix {
    /// Rename the unknown key at the violation's span to this one.
    RenameKey(String),
    /// Replace the value at the violation's span with one of these, in the schema's order.
    ReplaceValue(Vec<Value>),
    /// Add the missing `key` to the table at `table`, whose header ends at byte `header_end` (the
    /// root table has none). `example` is a value for it, from the schema or the table's other keys.
    AddKey { table: Vec<String>, key: String, header_end: Option<usize>, example: Option<Value> },
}

/// Validates a manifest against the schema.
pub fn validate(doc: &Spanned) -> Vec<Violation> {
    let mut out = Vec::new();
    check(root(), doc, &[], &doc.span, &mut out);
    out
}

/// `[a.b]` / `a.b` for messages.
fn describe(path: &[&str]) -> String {
    path.iter()
        .map(|k| if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { (*k).to_owned() } else { format!("\"{k}\"") })
        .collect::<Vec<_>>()
        .join(".")
}

fn check(node: &Value, value: &Spanned, path: &[&str], key_span: &Range<usize>, out: &mut Vec<Violation>) {
    let node = resolve(node);
    check_keywords(node, value, path, key_span, out);
    let alts: Vec<&Value> = ["anyOf", "oneOf"].iter().filter_map(|k| node.get(*k)).filter_map(Value::as_array).flatten().collect();
    if alts.is_empty() {
        return;
    }
    let mut results: Vec<(bool, Vec<Violation>)> = alts
        .iter()
        .map(|alt| {
            let alt = resolve(alt);
            let type_ok = accepts_type(alt, &value.value);
            let mut errors = Vec::new();
            check(alt, value, path, key_span, &mut errors);
            (type_ok, errors)
        })
        .collect();
    if results.iter().any(|(_, errors)| errors.is_empty()) {
        return;
    }
    let typed: Vec<usize> = (0..results.len()).filter(|&i| results[i].0).collect();
    if typed.is_empty() {
        let mut expected: Vec<&str> = alts.iter().flat_map(|a| types(a)).collect();
        expected.dedup();
        out.push(Violation { span: value.span.clone(), message: type_message(path, &expected, &value.value), fix: None });
        return;
    }
    let best = typed.into_iter().min_by_key(|&i| results[i].1.len()).expect("one alternative matches the type");
    out.append(&mut results[best].1);
}

/// Whether a node's own `type`/`enum` allows a value's type (true when it doesn't say).
fn accepts_type(node: &Value, value: &Tv) -> bool {
    let declared: Vec<&str> = match &node["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => match node.get("enum").and_then(Value::as_array) {
            Some(values) => values.iter().map(json_type).collect(),
            None => return true,
        },
    };
    declared.iter().any(|t| value.matches(t))
}

fn type_message(path: &[&str], expected: &[&str], value: &Tv) -> String {
    let names: Vec<&str> = expected
        .iter()
        .map(|t| match *t {
            "string" => "a string",
            "integer" => "an integer",
            "number" => "a number",
            "boolean" => "a boolean",
            "array" => "an array",
            "object" => "a table",
            _ => "null",
        })
        .collect();
    let what = match names.as_slice() {
        [] => "something else".to_owned(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    };
    let found = match value {
        Tv::Table(_) => "a table".to_owned(),
        Tv::Array(_) => "an array".to_owned(),
        other => format!("{} `{}`", other.type_name(), short(other)),
    };
    format!("`{}` must be {what}, found {found}", describe(path))
}

fn short(value: &Tv) -> String {
    match value {
        Tv::String(s) => s.clone(),
        Tv::Integer(i) => i.to_string(),
        Tv::Float(f) => f.to_string(),
        Tv::Boolean(b) => b.to_string(),
        _ => String::new(),
    }
}

fn json_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn check_keywords(node: &Value, value: &Spanned, path: &[&str], key_span: &Range<usize>, out: &mut Vec<Violation>) {
    if !accepts_type(node, &value.value) && node.get("type").is_some() {
        out.push(Violation { span: value.span.clone(), message: type_message(path, &types(node), &value.value), fix: None });
        return;
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array)
        && !values.iter().any(|v| value.value.equals(v))
    {
        let list: Vec<String> = values.iter().map(|v| format!("`{}`", json_text(v))).collect();
        let message = format!("`{}` must be one of {}, found `{}`", describe(path), list.join(", "), short(&value.value));
        out.push(Violation { span: value.span.clone(), message, fix: Some(Fix::ReplaceValue(values.clone())) });
        return;
    }
    if let Some(constant) = node.get("const")
        && !value.value.equals(constant)
    {
        let message = format!("`{}` must be `{}`", describe(path), json_text(constant));
        out.push(Violation { span: value.span.clone(), message, fix: None });
    }
    match &value.value {
        Tv::String(s) => {
            if let Some(min) = node.get("minLength").and_then(Value::as_u64)
                && (s.chars().count() as u64) < min
            {
                out.push(Violation { span: value.span.clone(), message: format!("`{}` must not be empty", describe(path)), fix: None });
            }
        }
        Tv::Integer(i) => {
            if let Some(min) = node.get("minimum").and_then(Value::as_i64)
                && *i < min
            {
                out.push(Violation {
                    span: value.span.clone(),
                    message: format!("`{}` must be at least {min}", describe(path)),
                    fix: None,
                });
            }
        }
        Tv::Array(items) => {
            if let Some(min) = node.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < min
            {
                let message = format!("`{}` needs at least {min} element{}", describe(path), if min == 1 { "" } else { "s" });
                out.push(Violation { span: value.span.clone(), message, fix: None });
            }
            if let Some(schema) = node.get("items") {
                for item in items {
                    let span = item.span.clone();
                    check(schema, item, path, &span, out);
                }
            }
        }
        Tv::Table(entries) => check_table(node, entries, value, path, key_span, out),
        Tv::Float(_) | Tv::Boolean(_) | Tv::Datetime => {}
    }
}

fn check_table(
    node: &Value,
    entries: &[(String, Range<usize>, Spanned)],
    value: &Spanned,
    path: &[&str],
    key_span: &Range<usize>,
    out: &mut Vec<Violation>,
) {
    let properties = node.get("properties").and_then(Value::as_object);
    for (key, span, child) in entries {
        let mut child_path = path.to_vec();
        child_path.push(key);
        if let Some(schema) = properties.and_then(|p| p.get(key)) {
            check(schema, child, &child_path, span, out);
            continue;
        }
        match node.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                let message = match path {
                    [] => format!("unknown key `{}`", describe(&child_path)),
                    _ => format!("unknown key `{key}` in `{}`", describe(path)),
                };
                let known: Vec<&String> = properties.into_iter().flat_map(|p| p.keys()).collect();
                let closest = known.iter().map(|k| (distance(key, k), k)).filter(|(d, _)| *d <= 2).min_by_key(|(d, _)| *d);
                let message = match (closest, known.len()) {
                    (Some((_, k)), _) => format!("{message}; did you mean `{k}`?"),
                    (None, 1..=12) => {
                        let list: Vec<String> = known.iter().map(|k| format!("`{k}`")).collect();
                        format!("{message}; expected one of {}", list.join(", "))
                    }
                    (None, _) => message,
                };
                // Not to a key the table already has: that would be a duplicate.
                let fix = closest.filter(|(_, k)| !entries.iter().any(|(e, _, _)| e == **k)).map(|(_, k)| Fix::RenameKey((*k).clone()));
                out.push(Violation { span: span.clone(), message, fix });
            }
            Some(schema @ Value::Object(_)) => check(schema, child, &child_path, span, out),
            _ => {}
        }
    }
    let has = |key: &str| entries.iter().any(|(k, _, _)| k == key);
    // A table without its own span (the root, or a table only made by dotted headers) is
    // reported where its key is written.
    let at = if value.span.is_empty() { key_span.clone() } else { value.span.clone() };
    for required in node.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !has(required) {
            let message = match path {
                [] => format!("missing `{required}`"),
                _ => format!("missing `{required}` in `{}`", describe(path)),
            };
            let schema = properties.and_then(|p| p.get(required));
            let mut example = schema.and_then(|s| values(s).into_iter().find(|(_, what)| *what != "const").map(|(v, _)| v.clone()));
            // A `[[bin]]` without a name: the one its `path` suggests.
            if example.is_none()
                && required == "name"
                && let Some((_, _, Spanned { value: Tv::String(file), .. })) = entries.iter().find(|(k, _, _)| k == "path")
            {
                let stem = std::path::Path::new(file).file_stem().map(|s| s.to_string_lossy().to_lowercase());
                example = stem.filter(|s| !s.is_empty()).map(Value::String);
            }
            let header_end = (!value.span.is_empty()).then_some(value.span.end);
            let table = path.iter().map(|k| (*k).to_owned()).collect();
            let fix = Fix::AddKey { table, key: required.to_owned(), header_end, example };
            out.push(Violation { span: at.clone(), message, fix: Some(fix) });
        }
    }
    if let Some(max) = node.get("maxProperties").and_then(Value::as_u64)
        && entries.len() as u64 > max
    {
        let message = match path {
            [] => format!("this table can only have {max} key{}", if max == 1 { "" } else { "s" }),
            _ => format!("`{}` can only have {max} key{}", describe(path), if max == 1 { "" } else { "s" }),
        };
        out.push(Violation { span: at.clone(), message, fix: None });
    }
    if let Some(forbidden) = node.get("not").and_then(|n| n.get("required")).and_then(Value::as_array) {
        let names: Vec<&str> = forbidden.iter().filter_map(Value::as_str).collect();
        if names.iter().all(|k| has(k)) {
            for name in names {
                let span = entries.iter().find(|(k, _, _)| k == name).map_or(at.clone(), |(_, s, _)| s.clone());
                out.push(Violation { span, message: format!("`{name}` is not allowed here"), fix: None });
            }
        }
    }
}

/// The Levenshtein distance between two keys.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous + usize::from(ca != *cb);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(previous + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violations(text: &str) -> Vec<(String, String)> {
        let doc = toml_edit::Document::parse(text.to_owned()).unwrap();
        validate(&from_document(&doc)).into_iter().map(|v| (text[v.span].to_owned(), v.message)).collect()
    }

    #[test]
    fn finds_keys_and_their_docs() {
        let jvm = at_path(&["target", "jvm"]);
        assert!(keys(&jvm).iter().any(|(k, _)| *k == "main-class"));
        let dep = at_path(&["dependencies", "io.ktor:ktor-server-core"]);
        assert!(keys(&dep).iter().any(|(k, _)| *k == "version"), "dependency tables come from additionalProperties");
        let bin = at_path(&["bin", "name"]);
        assert!(description(bin[0]).unwrap().contains("Letters"));
        let env = at_path(&["target", "wasm", "environment"]);
        let values: Vec<String> = values(env[0]).iter().map(|(v, _)| json_text(v)).collect();
        assert_eq!(values, ["js", "browser", "wasi"]);
        assert_eq!(types(at_path(&["toolchain", "jdk"])[0]), ["string", "integer"]);
        assert!(at_path(&["nope"]).is_empty());
    }

    #[test]
    fn accepts_valid_manifests() {
        let text = r#"
[package]
name = "app"
version = "0.1.0"

[toolchain]
jdk = 21

[target.jvm]
main-class = "a.MainKt"

[target.wasm]
environment = "browser"

[dependencies]
"io.ktor:ktor-server-core" = "3.0.0"
"g:x" = { version = "1", scope = "runtime" }

[[bin]]
name = "tool"

[tasks]
hello = "echo hi"
build-docs = { run = ["a", "b"], description = "x" }
"#;
        assert_eq!(violations(text), []);
        assert_eq!(violations("[workspace]\nmembers = [\"a\"]\n"), []);
    }

    #[test]
    fn reports_violations_where_they_are() {
        let text = "[package]\nname = \"app\"\nversion = \"1\"\nnmae = \"x\"\n[target.wasm]\nenvironment = \"web\"\n[target.jvm]\nmain-class = 1\n";
        let found = violations(text);
        assert_eq!(found[0].0, "nmae");
        assert_eq!(found[0].1, "unknown key `nmae` in `package`; did you mean `name`?");
        assert_eq!(found[1].0, "\"web\"");
        assert_eq!(found[1].1, "`target.wasm.environment` must be one of `js`, `browser`, `wasi`, found `web`");
        assert_eq!(found[2].0, "1");
        assert_eq!(found[2].1, "`target.jvm.main-class` must be a string, found integer `1`");
        assert_eq!(found.len(), 3);

        let found = violations("[package]\nname = \"app\"\n[dependencies]\n\"g:a\" = { version = \"1\", scop = \"x\" }\n\"g:b\" = 1\n");
        let messages: Vec<&str> = found.iter().map(|(_, m)| m.as_str()).collect();
        assert!(messages[0].starts_with("missing `version` in `package`"), "{messages:?}");
        assert_eq!(messages[1], "unknown key `scop` in `dependencies.\"g:a\"`; did you mean `scope`?");
        assert_eq!(distance("kotlin", "kotlin"), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("jvm-arg", "jvm-args"), 1);
        assert_eq!(messages[2], "`dependencies.\"g:b\"` must be a string or a table, found integer `1`");

        let found = violations("[[bin]]\npath = \"x.kt\"\n[package]\nname = \"a\"\nversion = \"1\"\n");
        assert_eq!(found.len(), 1);
        assert!(found[0].1.starts_with("missing `name` in `bin`"), "{found:?}");
        assert_eq!(violations("")[0].1, "missing `package`");
    }
}
