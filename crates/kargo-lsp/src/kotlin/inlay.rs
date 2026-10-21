//! Inlay hints: the inferred types of values and lambda parameters, the parameter names of
//! literal arguments, and the implicit `it` and `this` of lambdas.

use std::ops::Range;

use serde_json::{Value, json};

use super::features::{self, Target};
use super::index::UnitId;
use super::lexer::Tok;
use super::resolve::{Analyzer, Ty};
use super::syntax::{InitKind, Kind, ScopeKind};
use crate::text;

/// Which kinds of hints are shown: `inlayHints.types`, `.parameterNames`, `.implicitIt` and
/// `.implicitReceivers` of `initializationOptions`, all on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub types: bool,
    pub parameter_names: bool,
    pub implicit_it: bool,
    pub implicit_receivers: bool,
    /// The `operator fun` of the project that an operator calls; off by default.
    pub operators: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { types: true, parameter_names: true, implicit_it: true, implicit_receivers: true, operators: false }
    }
}

impl Options {
    /// Reads the options from `initializationOptions`, nested (`{"inlayHints": {"types": false}}`)
    /// or flat (`{"inlayHints.types": false}`).
    pub fn from_json(init: &Value) -> Self {
        let get = |key: &str| init["inlayHints"][key] != false && init[format!("inlayHints.{key}")] != false;
        Options {
            types: get("types"),
            parameter_names: get("parameterNames"),
            implicit_it: get("implicitIt"),
            implicit_receivers: get("implicitReceivers"),
            operators: init["inlayHints"]["operators"] == true || init["inlayHints.operators"] == true,
        }
    }
}

/// `InlayHintKind`.
const TYPE: u32 = 1;
const PARAMETER: u32 = 2;

struct Hint {
    offset: usize,
    label: String,
    kind: u32,
    padding_left: bool,
    padding_right: bool,
}

/// The hints of `u` inside `range` (the whole file when `None`), as LSP `InlayHint`s.
pub fn hints(an: &Analyzer, u: UnitId, range: Option<Range<usize>>, options: Options) -> Value {
    let mut found = Vec::new();
    if options.types {
        type_hints(an, u, &mut found);
    }
    if options.implicit_it || options.implicit_receivers {
        lambda_hints(an, u, options, &mut found);
    }
    if options.parameter_names {
        parameter_hints(an, u, &mut found);
    }
    if options.operators {
        operator_hints(an, u, &mut found);
    }
    let text = &an.unit(u).text;
    found.retain(|h| range.as_ref().is_none_or(|r| r.start <= h.offset && h.offset <= r.end));
    found.sort_by_key(|h| h.offset);
    Value::Array(
        found
            .iter()
            .map(|h| {
                json!({
                    "position": text::position(text, h.offset),
                    "label": h.label,
                    "kind": h.kind,
                    "paddingLeft": h.padding_left,
                    "paddingRight": h.padding_right,
                })
            })
            .collect(),
    )
}

/// The type as a hint: only when it is a class or function type with no unknown parts.
fn shown(an: &Analyzer, ty: Option<Ty>) -> Option<String> {
    let ty = ty?;
    if !matches!(ty, Ty::Class { .. } | Ty::Func { .. }) || !features::fully_known(&ty) {
        return None;
    }
    Some(features::display_ty(an, &ty)).filter(|t| !t.is_empty())
}

/// Whether the initializer starting at token `start` shows its type: a literal or a constructor
/// call.
fn obvious(an: &Analyzer, u: UnitId, start: usize) -> bool {
    let lx = &an.file(u).lexed;
    let src = &an.unit(u).text;
    match lx.kind(start) {
        Some(Tok::Int | Tok::Float | Tok::Char | Tok::StrOpen) => true,
        Some(Tok::Op) if lx.text(src, start) == "-" => matches!(lx.kind(start + 1), Some(Tok::Int | Tok::Float)),
        Some(Tok::Ident) => {
            if matches!(lx.text(src, start), "true" | "false" | "null") {
                return true;
            }
            lx.kind(start + 1) == Some(Tok::LParen)
                && features::targets_at(an, u, start).iter().any(|t| {
                    t.sym().is_some_and(|s| {
                        let d = an.decl(s);
                        d.kind == Kind::Constructor || d.kind.is_class()
                    })
                })
        }
        _ => false,
    }
}

/// `: Type` after `val`, `var`, `for` and lambda parameter names without a declared type.
fn type_hints(an: &Analyzer, u: UnitId, out: &mut Vec<Hint>) {
    let file = an.file(u);
    for (id, d) in file.decls.iter().enumerate() {
        let declares = matches!(d.kind, Kind::Property | Kind::Variable | Kind::Parameter);
        if !declares || d.ty.is_some() || d.generated || d.name == "_" || d.name.is_empty() || d.name_span.is_empty() {
            continue;
        }
        // Parameters of functions have their types written; the ones left are lambda parameters.
        if d.kind == Kind::Parameter && !matches!(d.init_kind, InitKind::LambdaParam(_) | InitKind::LambdaComponent(..)) {
            continue;
        }
        if d.kind != Kind::Parameter
            && d.init_kind == InitKind::Value
            && let Some(start) = d.init
            && obvious(an, u, start)
        {
            continue;
        }
        let Some(label) = shown(an, an.value_ty(u.sym(id), &Default::default())) else { continue };
        out.push(Hint { offset: d.name_span.end, label: format!(": {label}"), kind: TYPE, padding_left: false, padding_right: false });
    }
}

/// `this: T` and `it: T` after the `{` of lambdas that get them implicitly.
fn lambda_hints(an: &Analyzer, u: UnitId, options: Options, out: &mut Vec<Hint>) {
    let file = an.file(u);
    for scope in &file.scopes {
        let ScopeKind::Lambda { brace, it } = scope.kind else { continue };
        let Some(ctx) = an.lambda_ctx(u, brace) else { continue };
        let offset = file.lexed.tokens[brace].end;
        let mut push = |label: String| out.push(Hint { offset, label, kind: TYPE, padding_left: true, padding_right: true });
        if options.implicit_receivers
            && let Some(t) = shown(an, ctx.receiver.clone())
        {
            push(format!("this: {t}"));
        }
        if options.implicit_it
            && it
            && let Some(t) = shown(an, ctx.params.first().cloned().flatten())
        {
            push(format!("it: {t}"));
        }
    }
}

/// Whether the tokens `start..end` are one literal: a number, character, string without
/// templates, `true`, `false` or `null`.
fn literal(an: &Analyzer, u: UnitId, start: usize, end: usize) -> bool {
    let lx = &an.file(u).lexed;
    let src = &an.unit(u).text;
    match lx.kind(start) {
        Some(Tok::Int | Tok::Float | Tok::Char) => end == start + 1,
        Some(Tok::Op) if lx.text(src, start) == "-" => end == start + 2 && matches!(lx.kind(start + 1), Some(Tok::Int | Tok::Float)),
        Some(Tok::Ident) => end == start + 1 && matches!(lx.text(src, start), "true" | "false" | "null"),
        Some(Tok::StrOpen) => {
            end > start + 1
                && lx.kind(end - 1) == Some(Tok::StrClose)
                && (start..end)
                    .all(|i| matches!(lx.kind(i), Some(Tok::StrOpen | Tok::Str | Tok::StrClose)) && !lx.text(src, i).contains('$'))
        }
        _ => false,
    }
}

/// `name:` before the positional literal arguments of calls.
fn parameter_hints(an: &Analyzer, u: UnitId, out: &mut Vec<Hint>) {
    let file = an.file(u);
    let lx = &file.lexed;
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind != Tok::Ident || lx.kind(i + 1) != Some(Tok::LParen) || lx.tokens[i + 1].nl || file.decl_at_name(t.start).is_some() {
            continue;
        }
        let (args, _) = an.call_args(u, i + 1, lx.tokens.len());
        if !args.iter().any(|a| !a.lambda && a.name.is_none() && literal(an, u, a.start, a.end)) {
            continue;
        }
        let candidates: Vec<_> = features::targets_at(an, u, i)
            .into_iter()
            .filter_map(|t| match t {
                Target::Sym(m) if matches!(an.decl(m.sym).kind, Kind::Function | Kind::Constructor) || an.decl(m.sym).kind.is_class() => {
                    Some(m)
                }
                _ => None,
            })
            .collect();
        let types = an.arg_types(u, &args);
        let Some(best) = candidates
            .iter()
            .filter_map(|m| an.call_fit(m, &args, &types, u).map(|score| (score, m)))
            .max_by_key(|(score, _)| *score)
            .map(|(_, m)| m)
        else {
            continue;
        };
        let d = an.decl(best.sym);
        let decls = &an.file(best.sym.unit_id()).decls;
        for (index, arg) in args.iter().enumerate() {
            if arg.name.is_some() {
                break;
            }
            let Some(&p) = d.params.get(index) else { break };
            let param = &decls[p];
            if param.vararg {
                break;
            }
            // Compiled libraries without parameter names call them `p0`, `arg0`.
            let unnamed = param.name.strip_prefix("arg").or_else(|| param.name.strip_prefix('p')).is_some_and(|r| r.parse::<u32>().is_ok());
            if arg.lambda || unnamed || !literal(an, u, arg.start, arg.end) {
                continue;
            }
            out.push(Hint {
                offset: lx.tokens[arg.start].start,
                label: format!("{}:", param.name),
                kind: PARAMETER,
                padding_left: false,
                padding_right: true,
            });
        }
    }
}

/// The function that an operator of the project calls, after the operator: `a + b` shows
/// `plus(o: Money)`. Operators of libraries are left out, since their functions are well known.
fn operator_hints(an: &Analyzer, u: UnitId, out: &mut Vec<Hint>) {
    let lx = &an.file(u).lexed;
    let src = &an.unit(u).text;
    for (i, t) in lx.tokens.iter().enumerate() {
        let word = &src[t.span()];
        let candidate = match t.kind {
            Tok::Op => !matches!(word, "==" | "!=" | "===" | "!==" | "&&" | "||" | "?:" | "!!"),
            Tok::Lt | Tok::Gt | Tok::LBracket => true,
            Tok::Ident => word == "in",
            _ => false,
        };
        if !candidate {
            continue;
        }
        let Some(m) = an.operator_targets(u, i).into_iter().find(|m| !m.sym.lib && !an.unit(m.sym.unit_id()).is_java()) else { continue };
        let signature = features::signature(an, &m);
        let rest = signature.split_once("fun ").map_or(signature.as_str(), |(_, r)| r);
        let label = rest.rfind("): ").map_or(rest, |k| &rest[..=k]).to_owned();
        let offset = if t.kind == Tok::LBracket { lx.matching[i].map_or(t.end, |c| lx.tokens[c].end) } else { t.end };
        out.push(Hint { offset, label, kind: PARAMETER, padding_left: true, padding_right: false });
    }
}
