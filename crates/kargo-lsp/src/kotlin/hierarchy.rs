//! Go to implementation, the call hierarchy and the type hierarchy.

use std::ops::Range;

use serde_json::{Value, json};

use super::features::{self, Target};
use super::index::{SymId, UnitId};
use super::lexer::Tok;
use super::resolve::{Analyzer, Ctx, Subst, Ty};
use super::syntax::{Decl, Kind};
use crate::text;

/// The declarations that implement or override `sym`: the subtypes of a class, the overrides of
/// a member in them, and the `actual` declarations of an `expect` one.
pub fn implementations(an: &Analyzer, sym: SymId) -> Vec<SymId> {
    let d = an.decl(sym);
    let mut out: Vec<SymId> = Vec::new();
    if d.local || matches!(d.kind, Kind::Parameter | Kind::Variable) {
        return out;
    }
    if d.has("expect") {
        let fq = an.fq(sym);
        out.extend(an.by_fq(&fq).into_iter().filter(|&o| o != sym && !o.lib && features::same_shape(an, o, sym)));
    }
    if d.kind.is_class() {
        out.extend(subtypes(an, sym, true));
    } else if matches!(d.kind, Kind::Function | Kind::Property)
        && let Some(owner) = an.file(sym.unit_id()).owner_class(sym.decl_id())
    {
        for class in subtypes(an, sym.unit_id().sym(owner), true) {
            for &m in &an.decl(class).members {
                let ms = class.unit_id().sym(m);
                let md = an.decl(ms);
                if md.name == d.name && md.kind == d.kind && features::same_shape(an, ms, sym) && !out.contains(&ms) {
                    out.push(ms);
                }
            }
        }
    }
    out
}

/// The classes of the project that have `class` among their supertypes: all of them, or with
/// `transitive` false only the ones that name it directly.
fn subtypes(an: &Analyzer, class: SymId, transitive: bool) -> Vec<SymId> {
    let fq = an.fq(class);
    let mut out = Vec::new();
    for (ui, unit) in an.proj.units.iter().enumerate() {
        let uid = UnitId { lib: false, unit: ui as u32 };
        if unit.is_java() {
            continue;
        }
        for (ci, cd) in unit.file.decls.iter().enumerate() {
            if !cd.kind.is_class() || cd.name.is_empty() {
                continue;
            }
            let sub = uid.sym(ci);
            if sub == class || an.fq(sub) == fq {
                continue;
            }
            let hit = if transitive {
                an.closure(&an.instance(sub)).iter().any(|(c, _)| an.fq(*c) == fq)
            } else {
                direct_supertypes(an, sub).iter().any(|s| an.fq(*s) == fq)
            };
            if hit {
                out.push(sub);
            }
        }
    }
    out
}

/// The classes `class` names after its `:`.
fn direct_supertypes(an: &Analyzer, class: SymId) -> Vec<SymId> {
    let ctx = Ctx { unit: class.unit_id(), decl: Some(class.decl_id()) };
    an.decl(class)
        .supertypes
        .iter()
        .filter_map(|st| match an.ty_of_ref(st, ctx, &Subst::new())? {
            Ty::Class { sym, .. } => Some(sym),
            _ => None,
        })
        .collect()
}

// ----- items -----

/// A `CallHierarchyItem` or `TypeHierarchyItem` for `sym`.
fn item(an: &Analyzer, sym: SymId) -> Value {
    let unit = an.unit(sym.unit_id());
    let d = an.decl(sym);
    let file = &unit.file;
    let owner = file.owner_class(sym.decl_id());
    let detail = match owner {
        Some(c) => file.fq_name(c),
        None => file.package.clone(),
    };
    let span = d.span.start.min(d.name_span.start)..d.span.end.max(d.name_span.end);
    json!({
        "name": d.name,
        "kind": features::symbol_kind(d, owner.is_some()),
        "detail": detail,
        "uri": text::path_to_uri(&unit.path),
        "range": text::range(&unit.text, span),
        "selectionRange": text::range(&unit.text, d.name_span.clone()),
        "data": { "lib": sym.lib, "unit": sym.unit, "decl": sym.decl },
    })
}

/// The declaration an item stands for, `None` when the project changed since it was sent.
fn resolve_item(an: &Analyzer, item: &Value) -> Option<SymId> {
    let data = &item["data"];
    let (lib, unit, decl) = (data["lib"].as_bool()?, data["unit"].as_u64()? as u32, data["decl"].as_u64()? as usize);
    let uid = UnitId { lib, unit };
    let u = an.index_of(lib).unit(uid)?;
    let d = u.file.decls.get(decl)?;
    let same =
        text::path_to_uri(&u.path) == item["uri"].as_str()? && d.name_span.start == text::offset(&u.text, &item["selectionRange"]["start"]);
    same.then(|| uid.sym(decl))
}

fn target_sym(an: &Analyzer, unit: UnitId, tok: usize) -> Option<SymId> {
    features::targets_at(an, unit, tok).into_iter().find_map(|t| t.sym())
}

// ----- type hierarchy -----

/// The class at `tok`, as the item the hierarchy starts from.
pub fn prepare_types(an: &Analyzer, unit: UnitId, tok: usize) -> Value {
    match target_sym(an, unit, tok) {
        Some(s) if an.decl(s).kind.is_class() => json!([item(an, s)]),
        _ => Value::Null,
    }
}

pub fn supertypes(an: &Analyzer, from: &Value) -> Value {
    let Some(sym) = resolve_item(an, from) else { return json!([]) };
    json!(direct_supertypes(an, sym).into_iter().map(|s| item(an, s)).collect::<Vec<_>>())
}

pub fn direct_subtypes(an: &Analyzer, from: &Value) -> Value {
    let Some(sym) = resolve_item(an, from) else { return json!([]) };
    json!(subtypes(an, sym, false).into_iter().map(|s| item(an, s)).collect::<Vec<_>>())
}

// ----- call hierarchy -----

fn callable(d: &Decl) -> bool {
    matches!(d.kind, Kind::Function | Kind::Constructor) || d.kind.is_class()
}

/// The function, constructor or class at `tok`.
pub fn prepare_calls(an: &Analyzer, unit: UnitId, tok: usize) -> Value {
    match target_sym(an, unit, tok) {
        Some(s) if callable(an.decl(s)) => json!([item(an, s)]),
        _ => Value::Null,
    }
}

/// Whether the token after `tok` opens the arguments of a call.
fn called(an: &Analyzer, u: UnitId, tok: usize) -> bool {
    an.file(u).lexed.kind(tok + 1) == Some(Tok::LParen)
}

/// The declaration a call at `offset` is made from: the smallest function, constructor,
/// property or class around it.
fn caller(an: &Analyzer, u: UnitId, offset: usize) -> Option<SymId> {
    an.file(u)
        .decls
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            !d.local
                && !d.name.is_empty()
                && (matches!(d.kind, Kind::Function | Kind::Constructor | Kind::Property) || d.kind.is_class())
                && d.span.contains(&offset)
                && !d.name_span.contains(&offset)
        })
        .min_by_key(|(_, d)| d.span.len())
        .map(|(i, _)| u.sym(i))
}

fn ranges_json(an: &Analyzer, u: UnitId, ranges: &[Range<usize>]) -> Vec<Value> {
    let text = &an.unit(u).text;
    ranges.iter().map(|r| text::range(text, r.clone())).collect()
}

pub fn incoming(an: &Analyzer, from: &Value) -> Value {
    let Some(sym) = resolve_item(an, from) else { return json!([]) };
    let d = an.decl(sym);
    let mut group = features::rename_group(an, sym);
    // A constructor is called by the name of its class.
    let by_class = d.kind.is_class() || d.kind == Kind::Constructor;
    if d.kind == Kind::Constructor
        && let Some(owner) = an.file(sym.unit_id()).owner_class(sym.decl_id())
    {
        group.extend(an.class_decls(sym.unit_id().sym(owner)));
    }
    let mut callers: Vec<(SymId, Vec<Range<usize>>)> = Vec::new();
    for (u, range) in features::references(an, &group, false) {
        let Some(tok) = an.file(u).lexed.ident_at(range.start) else { continue };
        if by_class && !called(an, u, tok) {
            continue;
        }
        let Some(from) = caller(an, u, range.start) else { continue };
        match callers.iter().position(|(c, _)| *c == from) {
            Some(i) => callers[i].1.push(range),
            None => callers.push((from, vec![range])),
        }
    }
    json!(
        callers
            .iter()
            .map(|(c, ranges)| json!({ "from": item(an, *c), "fromRanges": ranges_json(an, c.unit_id(), ranges) }))
            .collect::<Vec<_>>()
    )
}

pub fn outgoing(an: &Analyzer, from: &Value) -> Value {
    let Some(sym) = resolve_item(an, from) else { return json!([]) };
    if sym.lib {
        return json!([]);
    }
    let u = sym.unit_id();
    let unit = an.unit(u);
    let d = an.decl(sym);
    let lx = &unit.file.lexed;
    let mut calls: Vec<(SymId, Vec<Range<usize>>)> = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        let span = t.span();
        if t.kind != Tok::Ident || span.start < d.span.start || span.end > d.span.end {
            continue;
        }
        // A declaration's own name is no call.
        if unit.file.decls.iter().any(|x| x.name_span == span) {
            continue;
        }
        let paren = called(an, u, i);
        if !paren && lx.kind(i + 1) != Some(Tok::LBrace) {
            continue;
        }
        let Some(Target::Sym(m)) = features::targets_at(an, u, i).into_iter().next() else { continue };
        let cd = an.decl(m.sym);
        if cd.local || !(matches!(cd.kind, Kind::Function | Kind::Constructor) || (paren && cd.kind.is_class())) {
            continue;
        }
        match calls.iter().position(|(c, _)| *c == m.sym) {
            Some(k) => calls[k].1.push(span),
            None => calls.push((m.sym, vec![span])),
        }
    }
    json!(calls.iter().map(|(c, ranges)| json!({ "to": item(an, *c), "fromRanges": ranges_json(an, u, ranges) })).collect::<Vec<_>>())
}
