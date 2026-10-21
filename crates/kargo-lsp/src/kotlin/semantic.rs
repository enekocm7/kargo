//! Semantic tokens: every identifier classified by what it resolves to, so an editor can tell a
//! property from a local and a class from a function call.

use std::ops::Range;

use serde_json::{Value, json};

use super::features::{self, Target};
use super::index::UnitId;
use super::lexer::Tok;
use super::resolve::Analyzer;
use super::syntax::{Decl, File, Kind};
use crate::text;

/// The token types of the legend, in the order the editor is told about them.
pub const TYPES: &[&str] = &[
    "namespace",
    "class",
    "enum",
    "interface",
    "typeParameter",
    "type",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
];

/// The token modifiers of the legend: bit `n` is the `n`th.
pub const MODIFIERS: &[&str] = &["declaration", "readonly", "static", "deprecated", "defaultLibrary"];

const NAMESPACE: u32 = 0;
const CLASS: u32 = 1;
const ENUM: u32 = 2;
const INTERFACE: u32 = 3;
const TYPE_PARAMETER: u32 = 4;
const TYPE: u32 = 5;
const PARAMETER: u32 = 6;
const VARIABLE: u32 = 7;
const PROPERTY: u32 = 8;
const ENUM_MEMBER: u32 = 9;
const FUNCTION: u32 = 10;
const METHOD: u32 = 11;

const DECLARATION: u32 = 1;
const READONLY: u32 = 2;
const STATIC: u32 = 4;
const DEPRECATED: u32 = 8;
const DEFAULT_LIBRARY: u32 = 16;

/// An identifier with its classification: byte span, token type and modifier bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub span: Range<usize>,
    pub kind: u32,
    pub modifiers: u32,
}

/// The `semanticTokensProvider` capability.
pub fn legend() -> Value {
    json!({ "legend": { "tokenTypes": TYPES, "tokenModifiers": MODIFIERS }, "full": true, "range": true })
}

fn class_type(kind: Kind) -> u32 {
    match kind {
        Kind::Interface => INTERFACE,
        Kind::Enum => ENUM,
        Kind::TypeAlias => TYPE,
        _ => CLASS,
    }
}

fn classify_decl(file: &File, d: &Decl, library: bool) -> (u32, u32) {
    let parent = d.parent.map(|p| &file.decls[p]);
    let kind = match d.kind {
        Kind::Constructor => parent.map_or(CLASS, |p| class_type(p.kind)),
        Kind::Function if parent.is_some_and(|p| p.kind.is_class()) => METHOD,
        Kind::Function => FUNCTION,
        Kind::Property => PROPERTY,
        Kind::Parameter => PARAMETER,
        Kind::Variable => VARIABLE,
        Kind::EnumEntry => ENUM_MEMBER,
        k => class_type(k),
    };
    let mut modifiers = 0;
    if (matches!(d.kind, Kind::Property | Kind::Variable) && !d.mutable) || matches!(d.kind, Kind::Parameter | Kind::EnumEntry) {
        modifiers |= READONLY;
    }
    if (parent.is_some_and(|p| matches!(p.kind, Kind::Object | Kind::Companion)) && d.kind != Kind::Constructor) || d.has("static") {
        modifiers |= STATIC;
    }
    if d.has("@Deprecated") {
        modifiers |= DEPRECATED;
    }
    if library {
        modifiers |= DEFAULT_LIBRARY;
    }
    (kind, modifiers)
}

/// The classified identifiers of a file, in source order.
pub fn classify(an: &Analyzer, u: UnitId) -> Vec<Classified> {
    let file = an.file(u);
    let src = &an.unit(u).text;
    let lx = &file.lexed;
    let generic: Vec<&Decl> = file.decls.iter().filter(|d| !d.type_params.is_empty()).collect();
    let mut out = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind != Tok::Ident {
            continue;
        }
        let mut found = None;
        match features::targets_at(an, u, i).first() {
            Some(Target::Sym(m)) => {
                let d = an.decl(m.sym);
                let (kind, mut modifiers) = classify_decl(an.file(m.sym.unit_id()), d, m.sym.lib);
                if m.sym.unit_id() == u && d.name_span == t.span() {
                    modifiers |= DECLARATION;
                }
                found = Some((kind, modifiers));
            }
            Some(Target::It { .. }) => found = Some((PARAMETER, READONLY)),
            Some(Target::Package(_)) => found = Some((NAMESPACE, 0)),
            None => {
                let name = lx.text(src, i);
                for d in &generic {
                    if d.span.start <= t.start
                        && t.end <= d.span.end
                        && let Some(p) = d.type_params.iter().find(|p| p.name == name)
                    {
                        found = Some((TYPE_PARAMETER, if p.span.start <= t.start && t.end <= p.span.end { DECLARATION } else { 0 }));
                        break;
                    }
                }
            }
        }
        if let Some((kind, modifiers)) = found {
            out.push(Classified { span: t.span(), kind, modifiers });
        }
    }
    out
}

/// The LSP encoding of `tokens` (relative to the previous one), limited to `range` if given.
pub fn encode(text: &str, tokens: &[Classified], range: Option<Range<usize>>) -> Value {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let encoding = text::encoding();
    let column = |offset: usize, line: usize| -> usize { text[starts[line]..offset].chars().map(|c| encoding.len(c)).sum() };
    let (mut data, mut prev_line, mut prev_col) = (Vec::new(), 0, 0);
    for t in tokens {
        if range.as_ref().is_some_and(|r| t.span.end <= r.start || t.span.start >= r.end) {
            continue;
        }
        let line = starts.partition_point(|&s| s <= t.span.start) - 1;
        let col = column(t.span.start, line);
        let len: usize = text[t.span.clone()].chars().map(|c| encoding.len(c)).sum();
        data.extend([line - prev_line, if line == prev_line { col - prev_col } else { col }, len, t.kind as usize, t.modifiers as usize]);
        (prev_line, prev_col) = (line, col);
    }
    json!({ "data": data })
}
