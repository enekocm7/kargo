//! Code actions for Kotlin files: importing an unresolved name, organizing the imports, adding
//! the dependency an unresolved import needs, implementing the abstract members a class lacks,
//! adding the `actual` declarations an `expect` one is missing, and quick fixes for the compiler
//! messages of `kargo check` it recognises (a missing `override`, an unused variable).
//!
//! A dependency is added by Kargo's [`crate::actions::ADD_DEPENDENCY`] command, which `kargo lsp`
//! runs as `kargo add` does, so the action and the command take the same code path. Every
//! other action is a workspace edit computed from the snapshot the request sees.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::features::{display_ty, escape_name, fully_known, targets_at};
use super::index::{DEFAULT_IMPORTS, Origin, SourceSet, SymId, Unit, UnitId, is_internal_package};
use super::lexer::{self, HARD_KEYWORDS, Tok};
use super::resolve::{Analyzer, Ctx, Subst, Ty, chain_start};
use super::syntax::{Decl, DeclId, File, InitKind, Kind, TypeRef};
use crate::text;

/// The kinds of code actions the server offers.
pub const KINDS: &[&str] = &["quickfix", "source.organizeImports"];

/// The body of the stubs the actions write.
const TODO: &str = "TODO(\"Not yet implemented\")";

/// What the editor asked for code actions on.
pub struct Request<'a> {
    pub unit: UnitId,
    /// The selection, in bytes.
    pub range: Range<usize>,
    /// `context.only`: the kinds of actions wanted, or empty for all.
    pub only: Vec<String>,
    /// `context.diagnostics`: the diagnostics at the selection, `kargo check`'s among them.
    pub diagnostics: Vec<Value>,
    /// The project's source roots with their source sets, where `actual` declarations go.
    pub roots: &'a [(PathBuf, SourceSet)],
    /// The `kargo.toml` of each package, by package index.
    pub manifests: &'a [PathBuf],
    /// Whether the editor takes `documentChanges` in a workspace edit, and can create files there.
    pub document_changes: bool,
    pub create_files: bool,
}

impl Request<'_> {
    fn wants(&self, kind: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|o| kind == o || kind.strip_prefix(o.as_str()).is_some_and(|r| r.starts_with('.')))
    }
}

/// The code actions for a `textDocument/codeAction` request on a Kotlin file.
pub fn code_actions(an: &Analyzer, req: &Request) -> Vec<Value> {
    let unit = an.unit(req.unit);
    let lines = lines_of(&unit.text, &req.range);
    let mut out = Vec::new();
    if req.wants("quickfix") {
        import_actions(an, req, &lines, &mut out);
        dependency_actions(an, req, &lines, &mut out);
        implement_actions(an, req, &lines, &mut out);
        actual_actions(an, req, &lines, &mut out);
        compiler_fixes(an, req, &mut out);
    }
    if req.wants("source.organizeImports") {
        organize_imports(an, req, &mut out);
    }
    out
}

// ----- helpers -----

/// From the start of the selection's first line to the end of its last one, where editors show
/// its light bulb.
fn lines_of(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = range.start.min(text.len());
    let end = range.end.clamp(start, text.len());
    let first = line_start(text, start);
    let last = text[end..].find('\n').map_or(text.len(), |i| end + i);
    first..last
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

fn touches(lines: &Range<usize>, span: &Range<usize>) -> bool {
    span.start <= lines.end && lines.start <= span.end
}

fn text_edit(text: &str, span: Range<usize>, new_text: impl Into<String>) -> Value {
    json!({ "range": text::range(text, span), "newText": new_text.into() })
}

/// The byte range of a diagnostic in `text`.
fn diagnostic_span(text: &str, diagnostic: &Value) -> Range<usize> {
    let start = text::offset(text, &diagnostic["range"]["start"]);
    let end = text::offset(text, &diagnostic["range"]["end"]).max(start);
    start..end
}

/// An action that edits the document `uri`.
fn edit_action(title: String, uri: &str, edits: Vec<Value>, diagnostic: Option<&Value>, preferred: bool) -> Value {
    let mut action = json!({ "title": title, "kind": "quickfix", "edit": { "changes": { uri: edits } } });
    if let Some(d) = diagnostic {
        action["diagnostics"] = json!([d]);
    }
    if preferred {
        action["isPreferred"] = json!(true);
    }
    action
}

/// The first quoted name in a compiler message: `x` in `Variable 'x' is never used.`
fn quoted(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once('\'')?;
    let (name, _) = rest.split_once('\'')?;
    Some(name)
}

/// The indentation of the line `offset` is on.
fn indent_at(text: &str, offset: usize) -> &str {
    let start = line_start(text, offset);
    let line = &text[start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// The `<T, R : Any>` of a declaration, with a space after it, or nothing.
fn type_params_text(src: &str, d: &Decl) -> String {
    if d.type_params.is_empty() {
        return String::new();
    }
    let params: Vec<&str> = d.type_params.iter().map(|t| src.get(t.span.clone()).unwrap_or(&t.name)).collect();
    format!("<{}> ", params.join(", "))
}

// ----- imports -----

/// Words that are never a reference to import.
const SOFT_KEYWORDS: &[&str] = &[
    "abstract",
    "actual",
    "annotation",
    "by",
    "catch",
    "companion",
    "const",
    "constructor",
    "crossinline",
    "data",
    "delegate",
    "dynamic",
    "enum",
    "expect",
    "external",
    "field",
    "file",
    "finally",
    "get",
    "import",
    "infix",
    "init",
    "inline",
    "inner",
    "internal",
    "it",
    "lateinit",
    "noinline",
    "open",
    "operator",
    "out",
    "override",
    "package",
    "param",
    "private",
    "property",
    "protected",
    "public",
    "receiver",
    "reified",
    "sealed",
    "set",
    "setparam",
    "suspend",
    "tailrec",
    "value",
    "vararg",
    "where",
];

/// The name of an `Unresolved reference 'x'.` (or K1's `Unresolved reference: x`) message.
fn unresolved_name(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("Unresolved reference")?;
    let name = match quoted(rest) {
        Some(name) => name,
        None => rest.trim_start_matches([':', ' ']).split(|c: char| !lexer::is_ident_char(c)).next()?,
    };
    (!name.is_empty()).then_some(name)
}

/// Offers to import each unresolved name on the selected lines, and each name a diagnostic
/// there says is unresolved.
fn import_actions(an: &Analyzer, req: &Request, lines: &Range<usize>, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (file, src) = (&unit.file, &unit.text);
    let lx = &file.lexed;
    let header_end = file.imports.last().map(|i| i.span.end).or_else(|| file.package_tokens.last().map(|&t| lx.tokens[t].end)).unwrap_or(0);
    let mut wanted: Vec<(usize, Option<&Value>)> = Vec::new();
    for d in &req.diagnostics {
        let Some(name) = d["message"].as_str().and_then(unresolved_name) else { continue };
        let span = diagnostic_span(src, d);
        let found = (lx.at_or_after(span.start)..lx.tokens.len())
            .take_while(|&k| lx.tokens[k].start <= span.end)
            .find(|&k| lx.tokens[k].kind == Tok::Ident && lx.text(src, k) == name);
        if let Some(tok) = found
            && lx.tokens[tok].start >= header_end
        {
            wanted.push((tok, Some(d)));
        }
    }
    for tok in lx.at_or_after(lines.start)..lx.tokens.len() {
        let t = lx.tokens[tok];
        if t.start > lines.end {
            break;
        }
        if t.start >= header_end && !wanted.iter().any(|(k, _)| *k == tok) && looks_unresolved(an, u, tok) {
            wanted.push((tok, None));
        }
    }
    let mut seen = HashSet::new();
    for (tok, diagnostic) in wanted {
        let name = lx.text(src, tok);
        if !seen.insert(name) {
            continue;
        }
        let candidates = import_candidates(an, u, tok);
        let single = candidates.len() == 1;
        for fq in candidates {
            let edit = import_edit(unit, &fq);
            out.push(edit_action(format!("Import `{fq}`"), &text::path_to_uri(&unit.path), vec![edit], diagnostic, single));
        }
    }
}

/// Whether the identifier `tok` is a reference that doesn't resolve. Without a diagnostic to go
/// on, only capitalized names and calls count, since a name the resolver doesn't follow (a
/// member of a receiver it can't infer) isn't necessarily missing an import.
fn looks_unresolved(an: &Analyzer, u: UnitId, tok: usize) -> bool {
    let unit = an.unit(u);
    let (file, src) = (&unit.file, &unit.text);
    let lx = &file.lexed;
    let t = lx.tokens[tok];
    if t.kind != Tok::Ident {
        return false;
    }
    let name = lx.text(src, tok);
    let raw = &src[t.span()];
    if HARD_KEYWORDS.contains(&raw) || SOFT_KEYWORDS.contains(&raw) || name == "this" || name == "super" {
        return false;
    }
    let next = lx.kind(tok + 1);
    let call = matches!(next, Some(Tok::LParen | Tok::LBrace | Tok::Lt)) && !lx.tokens.get(tok + 1).is_some_and(|n| n.nl);
    if !(call || name.starts_with(|c: char| c.is_uppercase())) {
        return false;
    }
    let prev = tok.checked_sub(1).and_then(|p| lx.kind(p));
    // A named argument.
    if next == Some(Tok::Eq) && matches!(prev, Some(Tok::LParen | Tok::Comma)) {
        return false;
    }
    if file.decls.iter().any(|d| d.name_span == t.span()) {
        return false;
    }
    // A type parameter in scope.
    if file.decls.iter().any(|d| d.span.start <= t.start && t.end <= d.span.end && d.type_params.iter().any(|p| p.name == name)) {
        return false;
    }
    targets_at(an, u, tok).is_empty()
}

/// The fully qualified names an import of the identifier `tok` could bring in, best first.
fn import_candidates(an: &Analyzer, u: UnitId, tok: usize) -> Vec<String> {
    let unit = an.unit(u);
    let (file, src) = (&unit.file, &unit.text);
    let lx = &file.lexed;
    let name = lx.text(src, tok);
    let prev = tok.checked_sub(1).and_then(|p| lx.kind(p));
    let mut syms: Vec<SymId> = Vec::new();
    match prev {
        // `receiver.name`: an extension that isn't imported.
        Some(Tok::Dot | Tok::SafeDot) => {
            let start = chain_start(src, lx, tok - 1);
            if start >= tok - 1 {
                return Vec::new();
            }
            let Some(receiver) = an.infer(u, start, tok - 1) else { return Vec::new() };
            let receiver = if prev == Some(Tok::SafeDot) { super::resolve::strip_nullable(receiver) } else { receiver };
            if matches!(receiver, Ty::Package(_)) || !an.members_named(&receiver, name, u).is_empty() {
                return Vec::new();
            }
            let own: HashSet<SymId> = an.members(&receiver, u, false).into_iter().map(|m| m.sym).collect();
            syms = an
                .members(&receiver, u, true)
                .into_iter()
                .map(|m| m.sym)
                .filter(|s| !own.contains(s) && an.decl(*s).name == name && an.decl(*s).receiver.is_some())
                .collect();
        }
        Some(Tok::ColonColon) if chain_start(src, lx, tok - 1) < tok - 1 => return Vec::new(),
        _ => {
            for index in [an.proj, an.lib] {
                syms.extend(index.by_name.get(name).into_iter().flatten().copied());
            }
            syms.extend(an.lib.jdk_named(name));
        }
    }
    let mut out: Vec<String> = Vec::new();
    for s in syms {
        let package = an.package_of(s);
        let d = an.decl(s);
        if package.is_empty()
            || package == file.package
            || d.local
            || d.kind == Kind::Constructor
            || (s.lib && is_internal_package(package))
            || !an.visible(u, s)
        {
            continue;
        }
        let fq = an.fq(s);
        if !file.imports.iter().any(|i| i.fq() == fq) && !out.contains(&fq) {
            out.push(fq);
        }
    }
    out.truncate(8);
    out
}

/// A dotted name as Kotlin source, each segment backquoted where it needs to be.
fn escape_path(fq: &str) -> String {
    fq.split('.').map(escape_name).collect::<Vec<_>>().join(".")
}

/// Where an import sorts, as IntelliJ IDEA and ktlint lay them out: everything else, then
/// `java.*`, `javax.*` and `kotlin.*`, then aliased imports, each group alphabetically.
fn import_order(path: &str, aliased: bool) -> (u8, String) {
    let group = if aliased {
        4
    } else if path.starts_with("java.") {
        1
    } else if path.starts_with("javax.") {
        2
    } else if path.starts_with("kotlin.") {
        3
    } else {
        0
    };
    (group, path.replace('`', ""))
}

/// An import as written, without `import` and a trailing `;`: `a.b.C`, `a.b.*`, `a.B as C`.
fn import_text<'s>(src: &'s str, import: &super::syntax::Import) -> &'s str {
    let text = src[import.span.clone()].trim();
    text.strip_prefix("import").unwrap_or(text).trim().trim_end_matches(';').trim_end()
}

/// The path of [`import_text`], without its alias.
fn import_path(text: &str) -> &str {
    text.split_once(" as ").map_or(text, |(path, _)| path.trim_end())
}

/// The edit that imports `fq` into `unit`: in sorted place among the imports, else after them.
pub fn import_edit(unit: &Unit, fq: &str) -> Value {
    let file = &unit.file;
    let text = &unit.text;
    let path = escape_path(fq);
    let key = import_order(&path, false);
    let later = file.imports.iter().find(|i| {
        let written = import_text(text, i);
        import_order(import_path(written), i.alias.is_some()) > key
    });
    if let Some(next) = later {
        let at = line_start(text, next.span.start);
        if text[at..next.span.start].trim().is_empty() {
            return text_edit(text, at..at, format!("import {path}\n"));
        }
    }
    let at = file.import_offset;
    let new_text = if file.imports.is_empty() && file.import_after_package {
        format!("\n\nimport {path}")
    } else if at == 0 {
        format!("import {path}\n\n")
    } else {
        format!("\nimport {path}")
    };
    text_edit(text, at..at, new_text)
}

/// Names that operator conventions call without writing them: an import of one stays.
fn is_operator_name(name: &str) -> bool {
    const OPERATORS: &[&str] = &[
        "getValue",
        "setValue",
        "provideDelegate",
        "invoke",
        "get",
        "set",
        "contains",
        "iterator",
        "next",
        "hasNext",
        "plus",
        "minus",
        "times",
        "div",
        "rem",
        "rangeTo",
        "rangeUntil",
        "unaryPlus",
        "unaryMinus",
        "not",
        "inc",
        "dec",
        "compareTo",
        "equals",
        "plusAssign",
        "minusAssign",
        "timesAssign",
        "divAssign",
        "remAssign",
    ];
    OPERATORS.contains(&name) || name.strip_prefix("component").is_some_and(|n| n.parse::<u32>().is_ok())
}

/// `source.organizeImports`: drops unused, duplicate and same-package imports and sorts the rest.
fn organize_imports(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let unit = an.unit(req.unit);
    let (file, src) = (&unit.file, &unit.text);
    let lx = &file.lexed;
    let (Some(first), Some(last)) = (file.imports.first(), file.imports.last()) else { return };
    let block = first.span.start..last.span.end;
    // A comment among the imports would be lost.
    if lx.comments.iter().any(|c| c.start < block.end && block.start < c.end) {
        return;
    }
    let package: HashSet<usize> = file.package_tokens.iter().copied().collect();
    let mut used: HashSet<&str> = lx
        .tokens
        .iter()
        .enumerate()
        .filter(|(i, t)| t.kind == Tok::Ident && !package.contains(i) && (t.start >= block.end || t.end <= block.start))
        .map(|(i, _)| lx.text(src, i))
        .collect();
    // KDoc links (`[Name]`) use imports too.
    for doc in lx.docs.values() {
        used.extend(src[doc.clone()].split(|c: char| !lexer::is_ident_char(c)).filter(|w| !w.is_empty()));
    }
    let mut kept: Vec<((u8, String), &str)> = Vec::new();
    for import in &file.imports {
        let written = import_text(src, import);
        let used = import.star || import.name().is_some_and(|n| used.contains(n) || is_operator_name(n));
        let same_package = import.alias.is_none()
            && !import.star
            && import.path.len() > 1
            && import.path[..import.path.len() - 1].join(".") == file.package;
        if used && !same_package && !kept.iter().any(|(_, w)| *w == written) {
            kept.push((import_order(import_path(written), import.alias.is_some()), written));
        }
    }
    kept.sort();
    let mut new = src.clone();
    if kept.is_empty() {
        let end = block.end + (src[block.end..].len() - src[block.end..].trim_start().len());
        new.replace_range(block.start..end, "");
    } else {
        let lines: Vec<String> = kept.iter().map(|(_, w)| format!("import {w}")).collect();
        new.replace_range(block, &lines.join("\n"));
    }
    if let Some(edit) = text::edit(src, &new) {
        let uri = text::path_to_uri(&unit.path);
        out.push(json!({ "title": "Organize imports", "kind": "source.organizeImports", "edit": { "changes": { uri: [edit] } } }));
    }
}

// ----- dependencies -----

/// Packages of well-known libraries and the Maven module that provides them. The longest
/// matching prefix wins.
const KNOWN_PACKAGES: &[(&str, &str)] = &[
    ("kotlinx.coroutines", "org.jetbrains.kotlinx:kotlinx-coroutines-core"),
    ("kotlinx.coroutines.test", "org.jetbrains.kotlinx:kotlinx-coroutines-test"),
    ("kotlinx.coroutines.swing", "org.jetbrains.kotlinx:kotlinx-coroutines-swing"),
    ("kotlinx.coroutines.javafx", "org.jetbrains.kotlinx:kotlinx-coroutines-javafx"),
    ("kotlinx.coroutines.android", "org.jetbrains.kotlinx:kotlinx-coroutines-android"),
    ("kotlinx.coroutines.reactive", "org.jetbrains.kotlinx:kotlinx-coroutines-reactive"),
    ("kotlinx.coroutines.reactor", "org.jetbrains.kotlinx:kotlinx-coroutines-reactor"),
    ("kotlinx.coroutines.slf4j", "org.jetbrains.kotlinx:kotlinx-coroutines-slf4j"),
    ("kotlinx.serialization", "org.jetbrains.kotlinx:kotlinx-serialization-core"),
    ("kotlinx.serialization.json", "org.jetbrains.kotlinx:kotlinx-serialization-json"),
    ("kotlinx.serialization.protobuf", "org.jetbrains.kotlinx:kotlinx-serialization-protobuf"),
    ("kotlinx.serialization.cbor", "org.jetbrains.kotlinx:kotlinx-serialization-cbor"),
    ("kotlinx.serialization.properties", "org.jetbrains.kotlinx:kotlinx-serialization-properties"),
    ("kotlinx.datetime", "org.jetbrains.kotlinx:kotlinx-datetime"),
    ("kotlinx.collections.immutable", "org.jetbrains.kotlinx:kotlinx-collections-immutable"),
    ("kotlinx.io", "org.jetbrains.kotlinx:kotlinx-io-core"),
    ("kotlinx.html", "org.jetbrains.kotlinx:kotlinx-html"),
    ("io.ktor.server", "io.ktor:ktor-server-core"),
    ("io.ktor.server.netty", "io.ktor:ktor-server-netty"),
    ("io.ktor.server.cio", "io.ktor:ktor-server-cio"),
    ("io.ktor.server.testing", "io.ktor:ktor-server-test-host"),
    ("io.ktor.server.auth", "io.ktor:ktor-server-auth"),
    ("io.ktor.server.auth.jwt", "io.ktor:ktor-server-auth-jwt"),
    ("io.ktor.server.sessions", "io.ktor:ktor-server-sessions"),
    ("io.ktor.server.websocket", "io.ktor:ktor-server-websockets"),
    ("io.ktor.server.html", "io.ktor:ktor-server-html-builder"),
    ("io.ktor.server.plugins.contentnegotiation", "io.ktor:ktor-server-content-negotiation"),
    ("io.ktor.server.plugins.cors", "io.ktor:ktor-server-cors"),
    ("io.ktor.server.plugins.calllogging", "io.ktor:ktor-server-call-logging"),
    ("io.ktor.server.plugins.statuspages", "io.ktor:ktor-server-status-pages"),
    ("io.ktor.server.plugins.defaultheaders", "io.ktor:ktor-server-default-headers"),
    ("io.ktor.server.plugins.compression", "io.ktor:ktor-server-compression"),
    ("io.ktor.client", "io.ktor:ktor-client-core"),
    ("io.ktor.client.engine.cio", "io.ktor:ktor-client-cio"),
    ("io.ktor.client.engine.okhttp", "io.ktor:ktor-client-okhttp"),
    ("io.ktor.client.engine.java", "io.ktor:ktor-client-java"),
    ("io.ktor.client.engine.darwin", "io.ktor:ktor-client-darwin"),
    ("io.ktor.client.engine.curl", "io.ktor:ktor-client-curl"),
    ("io.ktor.client.engine.js", "io.ktor:ktor-client-js"),
    ("io.ktor.client.plugins.contentnegotiation", "io.ktor:ktor-client-content-negotiation"),
    ("io.ktor.client.plugins.logging", "io.ktor:ktor-client-logging"),
    ("io.ktor.client.plugins.auth", "io.ktor:ktor-client-auth"),
    ("io.ktor.serialization.kotlinx.json", "io.ktor:ktor-serialization-kotlinx-json"),
    ("io.ktor.http", "io.ktor:ktor-http"),
    ("io.ktor.utils.io", "io.ktor:ktor-io"),
    ("org.junit.jupiter", "org.junit.jupiter:junit-jupiter-api"),
    ("org.junit.jupiter.params", "org.junit.jupiter:junit-jupiter-params"),
    ("io.mockk", "io.mockk:mockk"),
    ("io.kotest.matchers", "io.kotest:kotest-assertions-core"),
    ("org.assertj.core", "org.assertj:assertj-core"),
    ("com.google.gson", "com.google.code.gson:gson"),
    ("com.google.common", "com.google.guava:guava"),
    ("com.squareup.moshi", "com.squareup.moshi:moshi"),
    ("okhttp3", "com.squareup.okhttp3:okhttp"),
    ("okio", "com.squareup.okio:okio"),
    ("retrofit2", "com.squareup.retrofit2:retrofit"),
    ("com.fasterxml.jackson.core", "com.fasterxml.jackson.core:jackson-core"),
    ("com.fasterxml.jackson.annotation", "com.fasterxml.jackson.core:jackson-annotations"),
    ("com.fasterxml.jackson.databind", "com.fasterxml.jackson.core:jackson-databind"),
    ("com.fasterxml.jackson.module.kotlin", "com.fasterxml.jackson.module:jackson-module-kotlin"),
    ("org.slf4j", "org.slf4j:slf4j-api"),
    ("io.github.oshai.kotlinlogging", "io.github.oshai:kotlin-logging"),
    ("arrow.core", "io.arrow-kt:arrow-core"),
    ("arrow.fx.coroutines", "io.arrow-kt:arrow-fx-coroutines"),
    ("com.github.ajalt.clikt", "com.github.ajalt.clikt:clikt"),
    ("org.apache.commons.lang3", "org.apache.commons:commons-lang3"),
    ("org.apache.commons.io", "commons-io:commons-io"),
];

/// The Maven module that provides the package or class `fq`, when it is a well-known one.
pub fn known_module(fq: &str) -> Option<&'static str> {
    KNOWN_PACKAGES
        .iter()
        .filter(|(prefix, _)| fq == *prefix || fq.strip_prefix(prefix).is_some_and(|r| r.starts_with('.')))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, module)| *module)
}

/// Offers to add the dependency an unresolved import on the selected lines needs, through
/// `kargo/addDependency`: to `dev-dependencies` for test code, to the target's dependencies for
/// a target's own sources.
fn dependency_actions(an: &Analyzer, req: &Request, lines: &Range<usize>, out: &mut Vec<Value>) {
    let unit = an.unit(req.unit);
    let Origin::Source(set) = &unit.origin else { return };
    let Some(manifest) = req.manifests.get(set.package) else { return };
    let declared = std::fs::read_to_string(manifest).unwrap_or_default();
    let section = match (set.kind, &set.target) {
        (super::index::SetKind::Test, _) => "dev-dependencies".to_owned(),
        (_, Some(target)) => format!("target.{target}.dependencies"),
        _ => "dependencies".to_owned(),
    };
    let mut offered = HashSet::new();
    for import in &unit.file.imports {
        if !touches(lines, &import.span) {
            continue;
        }
        let fq = import.fq();
        let resolved =
            if import.star { an.has_package(&fq) || !an.by_fq(&fq).is_empty() } else { !an.import_targets(fq.clone()).is_empty() };
        let Some(module) = known_module(&fq).filter(|_| !resolved) else { continue };
        let artifact = module.split(':').nth(1).unwrap_or(module);
        if declared.contains(artifact) || !offered.insert(module) {
            continue;
        }
        let arguments = json!([{ "uri": text::path_to_uri(manifest), "coords": module, "section": section }]);
        let mut action = json!({
            "title": format!("Add `{module}` to kargo.toml"),
            "kind": "quickfix",
            "command": { "title": "Add dependency", "command": crate::actions::ADD_DEPENDENCY, "arguments": arguments },
        });
        let diagnostic = req.diagnostics.iter().find(|d| touches(&diagnostic_span(&unit.text, d), &import.span));
        if let Some(d) = diagnostic {
            action["diagnostics"] = json!([d]);
        }
        out.push(action);
    }
}

// ----- implementing members -----

/// Whether a compiler message says a class lacks implementations of abstract members.
fn is_missing_implementation(message: &str) -> bool {
    message.contains("does not implement abstract member") || message.contains("must override") && message.contains("abstract")
}

/// The braces of a class's body, as token indices.
fn class_body(file: &File, d: &Decl) -> Option<(usize, usize)> {
    let lx = &file.lexed;
    let close = lx.before(d.span.end)?;
    let open = lx.matching.get(close).copied().flatten()?;
    (lx.kind(close) == Some(Tok::RBrace) && open < close && lx.tokens[open].start > d.name_span.start).then_some((open, close))
}

/// Offers to implement the abstract members that a class on the selected lines inherits and
/// doesn't implement.
fn implement_actions(an: &Analyzer, req: &Request, lines: &Range<usize>, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (file, src) = (&unit.file, &unit.text);
    for (id, d) in file.decls.iter().enumerate() {
        if !matches!(d.kind, Kind::Class | Kind::Object | Kind::Companion | Kind::Enum)
            || d.supertypes.is_empty()
            || ["abstract", "sealed", "expect"].iter().any(|m| d.has(m))
        {
            continue;
        }
        let body = class_body(file, d);
        let header = d.span.start..body.map_or(d.span.end, |(open, _)| file.lexed.tokens[open].start);
        if !touches(lines, &header) {
            continue;
        }
        let missing = missing_members(an, u.sym(id));
        if missing.is_empty() {
            continue;
        }
        let class_indent = indent_at(src, d.span.start).to_owned();
        let member_indent = format!("{class_indent}    ");
        let target = Ctx { unit: u, decl: Some(id) };
        let mut imports = BTreeSet::new();
        let stubs: Vec<String> =
            missing.iter().map(|(sym, subst)| override_stub(an, *sym, subst, target, &member_indent, &mut imports)).collect();
        let members = stubs.join("\n\n");
        let mut edits = Vec::new();
        match body {
            Some((open, close)) => {
                let (open, close) = (file.lexed.tokens[open], file.lexed.tokens[close]);
                let inside = &src[open.end..close.start];
                if inside.trim().is_empty() {
                    edits.push(text_edit(src, open.end..close.start, format!("\n{members}\n{class_indent}")));
                } else {
                    let at = line_start(src, close.start);
                    if src[at..close.start].trim().is_empty() {
                        edits.push(text_edit(src, at..at, format!("\n{members}\n")));
                    } else {
                        edits.push(text_edit(src, close.start..close.start, format!("\n\n{members}\n{class_indent}")));
                    }
                }
            }
            None => edits.push(text_edit(src, d.span.end..d.span.end, format!(" {{\n{members}\n{class_indent}}}"))),
        }
        for fq in &imports {
            edits.push(import_edit(unit, fq));
        }
        let diagnostic = req
            .diagnostics
            .iter()
            .find(|x| x["message"].as_str().is_some_and(is_missing_implementation) && touches(&diagnostic_span(src, x), &header));
        let title = if missing.len() == 1 {
            format!("Implement `{}`", an.decl(missing[0].0).name)
        } else {
            format!("Implement {} members", missing.len())
        };
        out.push(edit_action(title, &text::path_to_uri(&unit.path), edits, diagnostic, true));
    }
}

/// The abstract functions and properties the class `class` inherits and doesn't implement,
/// with the type arguments of the supertype each comes from.
pub fn missing_members(an: &Analyzer, class: SymId) -> Vec<(SymId, Subst)> {
    let own = an.class_decls(class);
    // What the class delegates (`Iface by b`) is implemented by the delegate.
    let delegated = delegated_owners(an, class);
    let mut concrete: HashSet<(String, bool, usize)> = HashSet::new();
    let mut abstracts: Vec<((String, bool, usize), SymId, Subst)> = Vec::new();
    for (c, subst) in an.closure(&an.instance(class)) {
        let file = an.file(c.unit_id());
        for &m in &an.decl(c).members {
            let md = &file.decls[m];
            if !matches!(md.kind, Kind::Function | Kind::Property) || md.name.is_empty() {
                continue;
            }
            let function = md.kind == Kind::Function;
            let key = (md.name.clone(), function, if function { md.params.len() } else { 0 });
            let sym = c.unit_id().sym(m);
            if !own.contains(&c) && !delegated.contains_key(&c) && is_abstract(an, c, sym) {
                if !md.is_private() {
                    abstracts.push((key, sym, subst.clone()));
                }
            } else {
                concrete.insert(key);
            }
        }
    }
    let mut seen = HashSet::new();
    abstracts.into_iter().filter(|(key, ..)| !concrete.contains(key) && seen.insert(key.clone())).map(|(_, s, subst)| (s, subst)).collect()
}

/// The classes whose members the class `class` delegates with `by` (the delegated supertypes and
/// everything they inherit), each with the text of the delegate expression.
pub(super) fn delegated_owners(an: &Analyzer, class: SymId) -> HashMap<SymId, String> {
    let mut out = HashMap::new();
    for c in an.class_decls(class) {
        let (d, unit) = (an.decl(c), an.unit(c.unit_id()));
        let ctx = Ctx { unit: c.unit_id(), decl: Some(c.decl_id()) };
        for (index, span) in &d.delegated {
            let Some(ty) = d.supertypes.get(*index).and_then(|t| an.ty_of_ref(t, ctx, &Subst::new())) else { continue };
            let text = unit.text.get(span.clone()).unwrap_or_default().trim().to_owned();
            for (owner, _) in an.closure(&ty) {
                out.entry(owner).or_insert_with(|| text.clone());
            }
        }
    }
    out
}

/// Whether the member `m` of the class or interface `class` is abstract.
pub(super) fn is_abstract(an: &Analyzer, class: SymId, m: SymId) -> bool {
    let (cd, md) = (an.decl(class), an.decl(m));
    let unit = an.unit(m.unit_id());
    if md.has("abstract") {
        return true;
    }
    if cd.kind != Kind::Interface {
        return false;
    }
    if unit.is_java() {
        return md.kind == Kind::Function && !md.has("default") && !md.has("static");
    }
    let lx = &unit.file.lexed;
    match md.kind {
        Kind::Function => md.init.is_none() && !has_block_body(&unit.file, md),
        // `val x: Int` with no initializer and no getter after it.
        Kind::Property => {
            let next = lx.at_or_after(md.span.end);
            md.init.is_none() && !(lx.is(&unit.text, next, "get") || lx.is(&unit.text, next, "set"))
        }
        _ => false,
    }
}

/// Whether a function has a `{ ... }` body.
fn has_block_body(file: &File, d: &Decl) -> bool {
    let lx = &file.lexed;
    let Some(last) = lx.before(d.span.end) else { return false };
    let Some(open) = lx.matching.get(last).copied().flatten() else { return false };
    if lx.kind(last) != Some(Tok::RBrace) || lx.tokens[open].start < d.name_span.end {
        return false;
    }
    // Not the default value of a parameter: the brace comes after the parameter list.
    let name = lx.at_or_after(d.name_span.start);
    let params = (name..open).find(|&k| lx.kind(k) == Some(Tok::LParen)).and_then(|k| lx.matching[k]);
    params.is_none_or(|close| close < open)
}

/// A type as written in a member of a supertype, for an override in `target`: the type it
/// denotes with the supertype's type arguments, adding to `imports` the classes `target` doesn't
/// see by name; else the text as written, with the type arguments put in.
fn override_type(an: &Analyzer, tr: &TypeRef, ctx: Ctx, subst: &Subst, target: Ctx, imports: &mut BTreeSet<String>) -> String {
    if let Some(ty) = an.ty_of_ref(tr, ctx, subst)
        && fully_known(&ty)
    {
        collect_imports(an, &ty, target, imports);
        return display_ty(an, &ty);
    }
    let written = tr.display(&an.unit(ctx.unit).text);
    if subst.is_empty() {
        return written;
    }
    // Put the type arguments in place of the type parameters they bind.
    let lexed = lexer::lex(&written);
    let mut out = String::new();
    let mut at = 0;
    for (i, t) in lexed.tokens.iter().enumerate() {
        if t.kind == Tok::Ident
            && let Some(arg) = subst.get(lexed.text(&written, i))
            && fully_known(arg)
        {
            out.push_str(&written[at..t.start]);
            collect_imports(an, arg, target, imports);
            out.push_str(&display_ty(an, arg));
            at = t.end;
        }
    }
    out.push_str(&written[at..]);
    out
}

/// Adds to `imports` the classes in `ty` that the code at `target` doesn't see by their name.
fn collect_imports(an: &Analyzer, ty: &Ty, target: Ctx, imports: &mut BTreeSet<String>) {
    match ty {
        Ty::Class { sym, args, .. } => {
            let d = an.decl(*sym);
            let package = an.package_of(*sym);
            if !d.local && !package.is_empty() && package != an.file(target.unit).package && !DEFAULT_IMPORTS.contains(&package) {
                let fq = an.fq(*sym);
                match an.resolve_type(target, &d.name) {
                    // A different class of that name is in scope: leave it to the user.
                    Some(found) if an.fq(found) != fq => {}
                    Some(_) => {}
                    None => {
                        imports.insert(fq);
                    }
                }
            }
            for arg in args.iter().flatten() {
                collect_imports(an, arg, target, imports);
            }
        }
        Ty::Func { receiver, params, ret } => {
            for t in receiver.iter().map(|b| &**b).chain(params.iter().flatten()).chain(ret.iter().map(|b| &**b)) {
                collect_imports(an, t, target, imports);
            }
        }
        Ty::Static(_) | Ty::Package(_) => {}
    }
}

/// An `override` of the abstract member `m`, indented by `indent`, for the class at `target`.
fn override_stub(an: &Analyzer, m: SymId, subst: &Subst, target: Ctx, indent: &str, imports: &mut BTreeSet<String>) -> String {
    let d = an.decl(m);
    let unit = an.unit(m.unit_id());
    let ctx = Ctx { unit: m.unit_id(), decl: Some(m.decl_id()) };
    let mut ty = |tr: &TypeRef| override_type(an, tr, ctx, subst, target, imports);
    let mut modifiers = String::from("override ");
    if d.has("suspend") {
        modifiers.push_str("suspend ");
    }
    let type_params = type_params_text(&unit.text, d);
    let receiver = d.receiver.as_ref().map(|r| format!("{}.", ty(r))).unwrap_or_default();
    let name = escape_name(&d.name);
    let declared = d.ty.as_ref().map(&mut ty);
    match d.kind {
        Kind::Function => {
            let decls = &unit.file.decls;
            let params: Vec<String> = d
                .params
                .iter()
                .map(|&p| {
                    let pd = &decls[p];
                    let t = pd.ty.as_ref().map_or_else(|| "Any?".to_owned(), &mut ty);
                    format!("{}{}: {t}", if pd.vararg { "vararg " } else { "" }, escape_name(&pd.name))
                })
                .collect();
            let ret = declared.filter(|r| r != "Unit" && r != "void").map(|r| format!(": {r}")).unwrap_or_default();
            format!("{indent}{modifiers}fun {type_params}{receiver}{name}({}){ret} {{\n{indent}    {TODO}\n{indent}}}", params.join(", "))
        }
        _ => {
            let keyword = if d.mutable { "var" } else { "val" };
            let t = declared.unwrap_or_else(|| "Any?".to_owned());
            let mut stub = format!("{indent}{modifiers}{keyword} {type_params}{receiver}{name}: {t}\n{indent}    get() = {TODO}");
            if d.mutable {
                stub.push_str(&format!("\n{indent}    set(value) {{}}"));
            }
            stub
        }
    }
}

// ----- `actual` declarations -----

/// Offers to add the `actual` declarations an `expect` declaration on the selected lines lacks
/// in its package's targets: in `src-<target>/`, at the same path as the common file with the
/// target in its name (`Platform.kt` gets `Platform.jvm.kt`).
fn actual_actions(an: &Analyzer, req: &Request, lines: &Range<usize>, out: &mut Vec<Value>) {
    let unit = an.unit(req.unit);
    let file = &unit.file;
    let Origin::Source(set) = &unit.origin else { return };
    if set.target.is_some() {
        return;
    }
    let Some(common) = req.roots.iter().find(|(root, s)| s == set && unit.path.starts_with(root)).map(|(root, _)| root) else { return };
    let Ok(relative) = unit.path.strip_prefix(common) else { return };
    let Some(stem) = unit.path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { return };
    let mut targets: Vec<(&str, &PathBuf)> = Vec::new();
    for (root, s) in req.roots {
        if let Some(target) = s.target.as_deref()
            && s.package == set.package
            && s.kind == set.kind
            && !targets.iter().any(|(t, _)| *t == target)
        {
            targets.push((target, root));
        }
    }
    targets.sort();
    for &t in &file.top {
        let d = &file.decls[t];
        if !d.has("expect") || d.name.is_empty() || !touches(lines, &(d.span.start..d.name_span.end)) {
            continue;
        }
        let fq = file.fq_name(t);
        let mut changes: Vec<(&str, FileChange)> = Vec::new();
        for (target, root) in &targets {
            if has_actual(an, &fq, set, target) {
                continue;
            }
            let path = root.join(relative.with_file_name(format!("{stem}.{target}.kt")));
            if let Some(change) = actual_change(an, unit, t, &path, req.create_files) {
                changes.push((target, change));
            }
        }
        let name = &d.name;
        for (target, change) in &changes {
            if let Some(edit) = workspace_edit(req, std::slice::from_ref(change)) {
                out.push(json!({ "title": format!("Add `actual` for `{name}` to the {target} target"), "kind": "quickfix", "edit": edit }));
            }
        }
        if changes.len() > 1 {
            let all: Vec<FileChange> = changes.into_iter().map(|(_, c)| c).collect();
            if let Some(edit) = workspace_edit(req, &all) {
                out.push(json!({ "title": format!("Add `actual` for `{name}` to every target"), "kind": "quickfix", "edit": edit }));
            }
        }
    }
}

/// Edits to one file, which may have to be created first.
struct FileChange {
    uri: String,
    edits: Vec<Value>,
    create: bool,
}

/// A `WorkspaceEdit` for `changes`: `documentChanges` when a file is created, which needs the
/// editor's support.
fn workspace_edit(req: &Request, changes: &[FileChange]) -> Option<Value> {
    let creates = changes.iter().any(|c| c.create);
    if creates && !(req.create_files && req.document_changes) {
        return None;
    }
    if !req.document_changes {
        let map: serde_json::Map<String, Value> = changes.iter().map(|c| (c.uri.clone(), json!(c.edits))).collect();
        return Some(json!({ "changes": map }));
    }
    let mut list = Vec::new();
    for c in changes {
        if c.create {
            list.push(json!({ "kind": "create", "uri": c.uri, "options": { "ignoreIfExists": true } }));
        }
        list.push(json!({ "textDocument": { "uri": c.uri, "version": null }, "edits": c.edits }));
    }
    Some(json!({ "documentChanges": list }))
}

/// Whether the `target` sources of `set`'s package have an `actual` for `fq`.
fn has_actual(an: &Analyzer, fq: &str, set: &SourceSet, target: &str) -> bool {
    an.proj.by_fq.get(fq).into_iter().flatten().any(|&s| {
        matches!(&an.unit(s.unit_id()).origin, Origin::Source(o) if o.package == set.package && o.kind == set.kind && o.target.as_deref() == Some(target))
            && an.decl(s).has("actual")
    })
}

/// The change that adds the `actual` of the top-level declaration `id` of `unit` to the file at
/// `path`: appended to it, or a new file when there is none (and `create` allows it).
fn actual_change(an: &Analyzer, unit: &Unit, id: DeclId, path: &Path, create: bool) -> Option<FileChange> {
    let declaration = actual_decl(unit, id, "")?;
    let uri = text::path_to_uri(path);
    // The common file's imports that the declaration uses.
    let lexed = lexer::lex(&declaration);
    let names: HashSet<&str> =
        (0..lexed.tokens.len()).filter(|&i| lexed.tokens[i].kind == Tok::Ident).map(|i| lexed.text(&declaration, i)).collect();
    let needed: Vec<&str> = unit
        .file
        .imports
        .iter()
        .filter(|i| i.star || i.name().is_some_and(|n| names.contains(n)))
        .map(|i| import_text(&unit.text, i))
        .collect();
    let existing = an.proj.units.iter().find(|u| u.path == path).cloned().or_else(|| {
        let text = std::fs::read_to_string(path).ok()?;
        Some(std::sync::Arc::new(Unit::new(path.to_owned(), text, unit.origin.clone())))
    });
    match existing {
        Some(target) => {
            let text = &target.text;
            let mut edits: Vec<Value> = Vec::new();
            for import in needed {
                if !target.file.imports.iter().any(|i| import_text(text, i) == import) {
                    edits.push(match import.split_once(" as ").is_some() || import.ends_with(".*") {
                        true => text_edit(text, target.file.import_offset..target.file.import_offset, format!("\nimport {import}")),
                        false => import_edit(&target, import),
                    });
                }
            }
            let separator = if text.ends_with("\n\n") || text.is_empty() {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            edits.push(text_edit(text, text.len()..text.len(), format!("{separator}{declaration}\n")));
            Some(FileChange { uri, edits, create: false })
        }
        None if create => {
            let mut content = String::new();
            if !unit.file.package.is_empty() {
                content.push_str(&format!("package {}\n\n", escape_path(&unit.file.package)));
            }
            if !needed.is_empty() {
                for import in &needed {
                    content.push_str(&format!("import {import}\n"));
                }
                content.push('\n');
            }
            content.push_str(&declaration);
            content.push('\n');
            Some(FileChange { uri, edits: vec![json!({ "range": text::range("", 0..0), "newText": content })], create: true })
        }
        None => None,
    }
}

/// The parameters of a declaration as an `actual` repeats them: no default values.
fn params_text(unit: &Unit, d: &Decl, properties: bool) -> String {
    let decls = &unit.file.decls;
    let params: Vec<String> = d
        .params
        .iter()
        .map(|&p| {
            let pd = &decls[p];
            let mut s = String::new();
            for m in ["vararg", "noinline", "crossinline"] {
                if pd.has(m) || (m == "vararg" && pd.vararg) {
                    s.push_str(m);
                    s.push(' ');
                }
            }
            if properties && pd.kind == Kind::Property {
                s.push_str(if pd.mutable { "var " } else { "val " });
            }
            let ty = pd.ty.as_ref().map_or_else(|| "Any?".to_owned(), |t| t.display(&unit.text));
            s.push_str(&format!("{}: {ty}", escape_name(&pd.name)));
            s
        })
        .collect();
    params.join(", ")
}

/// The `actual` counterpart of the `expect` declaration `id`, with `TODO()` bodies.
fn actual_decl(unit: &Unit, id: DeclId, indent: &str) -> Option<String> {
    let file = &unit.file;
    let src = &unit.text;
    let d = &file.decls[id];
    let name = escape_name(&d.name);
    let kept = |list: &[&str]| -> String { list.iter().filter(|m| d.has(m)).map(|m| format!("{m} ")).collect() };
    let type_params = type_params_text(src, d);
    let receiver = d.receiver.as_ref().map(|r| format!("{}.", r.display(src))).unwrap_or_default();
    let in_interface = file.owner_class(id).is_some_and(|c| file.decls[c].kind == Kind::Interface);
    let abstract_member = in_interface || d.has("abstract");
    Some(match d.kind {
        Kind::Function => {
            let modifiers = kept(&[
                "internal",
                "protected",
                "private",
                "abstract",
                "open",
                "override",
                "suspend",
                "inline",
                "operator",
                "infix",
                "tailrec",
            ]);
            let ret = d.ty.as_ref().map(|t| format!(": {}", t.display(src))).unwrap_or_default();
            let head = format!("{indent}actual {modifiers}fun {type_params}{receiver}{name}({}){ret}", params_text(unit, d, false));
            if abstract_member && !has_block_body(file, d) && d.init.is_none() {
                head
            } else {
                format!("{head} {{\n{indent}    {TODO}\n{indent}}}")
            }
        }
        Kind::Property => {
            let modifiers = kept(&["internal", "protected", "private", "abstract", "open", "override"]);
            let keyword = if d.mutable { "var" } else { "val" };
            let ty = d.ty.as_ref().map_or_else(|| "Any?".to_owned(), |t| t.display(src));
            let head = format!("{indent}actual {modifiers}{keyword} {type_params}{receiver}{name}: {ty}");
            if abstract_member {
                head
            } else {
                let setter = if d.mutable { format!("\n{indent}    set(value) {{}}") } else { String::new() };
                format!("{head}\n{indent}    get() = {TODO}{setter}")
            }
        }
        Kind::Constructor => format!("{indent}actual constructor({}) {{\n{indent}    {TODO}\n{indent}}}", params_text(unit, d, false)),
        k if k.is_class() => {
            let modifiers = kept(&["internal", "protected", "private", "abstract", "open", "sealed", "data", "value", "inner", "fun"]);
            let keyword = match k {
                Kind::Interface => "interface",
                Kind::Object => "object",
                Kind::Companion => "companion object",
                Kind::Enum => "enum class",
                Kind::Annotation => "annotation class",
                _ => "class",
            };
            let mut head = format!("{indent}actual {modifiers}{keyword}");
            if k != Kind::Companion || d.name != "Companion" {
                head.push_str(&format!(" {name}"));
            }
            if !d.type_params.is_empty() {
                head.push_str(type_params_text(src, d).trim_end());
            }
            if !d.params.is_empty() {
                head.push_str(&format!(" actual constructor({})", params_text(unit, d, true)));
            }
            if !d.supertypes.is_empty() {
                let supertypes: Vec<String> = d.supertypes.iter().map(|t| t.display(src)).collect();
                head.push_str(&format!(" : {}", supertypes.join(", ")));
            }
            let inner = format!("{indent}    ");
            let entries: Vec<String> =
                d.members.iter().filter(|&&m| file.decls[m].kind == Kind::EnumEntry).map(|&m| escape_name(&file.decls[m].name)).collect();
            let members: Vec<String> = d
                .members
                .iter()
                .filter(|&&m| file.decls[m].kind != Kind::EnumEntry && !d.params.contains(&m))
                .filter_map(|&m| actual_decl(unit, m, &inner))
                .collect();
            if entries.is_empty() && members.is_empty() {
                head
            } else {
                let mut body = String::new();
                if !entries.is_empty() {
                    body.push_str(&format!("{inner}{}", entries.join(", ")));
                    body.push_str(if members.is_empty() { "\n" } else { ";\n\n" });
                }
                if !members.is_empty() {
                    body.push_str(&members.join("\n\n"));
                    body.push('\n');
                }
                format!("{head} {{\n{body}{indent}}}")
            }
        }
        _ => return None,
    })
}

// ----- quick fixes for compiler messages -----

/// The name in `Variable 'x' is never used.` or `Unused variable 'x'.`
fn unused_variable(message: &str) -> Option<&str> {
    let unused = message.starts_with("Variable '") && message.contains("never used") || message.starts_with("Unused variable '");
    unused.then(|| quoted(message)).flatten()
}

/// Whether a compiler message says a declaration overrides a member without `override`.
fn is_missing_override(message: &str) -> bool {
    message.contains("hides member of supertype") && message.contains("override")
}

/// Quick fixes for the compiler messages among the request's diagnostics.
fn compiler_fixes(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let unit = an.unit(req.unit);
    let (file, src) = (&unit.file, &unit.text);
    let lx = &file.lexed;
    let uri = text::path_to_uri(&unit.path);
    let line_of = |offset: usize| src[..offset.min(src.len())].matches('\n').count();
    for diagnostic in &req.diagnostics {
        let message = diagnostic["message"].as_str().unwrap_or_default();
        let span = diagnostic_span(src, diagnostic);
        if is_missing_override(message) {
            let on_line = |d: &&Decl| matches!(d.kind, Kind::Function | Kind::Property) && !d.local && !d.has("override");
            let Some(d) = file
                .decls
                .iter()
                .filter(on_line)
                .find(|d| d.name_span.start <= span.start && span.start <= d.name_span.end)
                .or_else(|| file.decls.iter().filter(on_line).find(|d| line_of(d.name_span.start) == line_of(span.start)))
            else {
                continue;
            };
            let name = lx.at_or_after(d.name_span.start);
            let Some(keyword) =
                (lx.at_or_after(d.span.start)..name).rev().find(|&k| ["fun", "val", "var"].iter().any(|w| lx.is(src, k, w)))
            else {
                continue;
            };
            let at = lx.tokens[keyword].start;
            let edit = text_edit(src, at..at, "override ");
            out.push(edit_action(format!("Add `override` to `{}`", d.name), &uri, vec![edit], Some(diagnostic), true));
        } else if let Some(name) = unused_variable(message) {
            let Some(d) =
                file.decls.iter().find(|d| d.kind == Kind::Variable && d.name == name && line_of(d.name_span.start) == line_of(span.start))
            else {
                continue;
            };
            match d.init_kind {
                InitKind::Value => {
                    let edit = remove_variable(src, lx, d);
                    out.push(edit_action(format!("Remove variable `{name}`"), &uri, vec![edit], Some(diagnostic), true));
                }
                _ => {
                    let edit = text_edit(src, d.name_span.clone(), "_");
                    out.push(edit_action("Rename to `_`".to_owned(), &uri, vec![edit], Some(diagnostic), true));
                }
            }
        }
    }
}

/// The edit that removes a local variable: the whole statement, or only `val x =` when its
/// initializer may have side effects.
fn remove_variable(src: &str, lx: &lexer::Lexed, d: &Decl) -> Value {
    if let Some(init) = d.init {
        let end = lx.at_or_after(d.span.end);
        let pure = (init..end).all(|k| {
            matches!(lx.kind(k), Some(Tok::Ident | Tok::Int | Tok::Float | Tok::Char | Tok::StrOpen | Tok::Str | Tok::StrClose | Tok::Dot))
        });
        if !pure {
            return text_edit(src, d.span.start..lx.tokens[init].start, "");
        }
    }
    let start = line_start(src, d.span.start);
    let line_end = src[d.span.end..].find('\n').map_or(src.len(), |i| d.span.end + i);
    if src[start..d.span.start].trim().is_empty() && src[d.span.end..line_end].trim().trim_end_matches(';').is_empty() {
        let next = (line_end + 1).min(src.len());
        return text_edit(src, start..next, "");
    }
    text_edit(src, d.span.clone(), "")
}
