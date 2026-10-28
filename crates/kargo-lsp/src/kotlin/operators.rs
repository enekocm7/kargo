//! Operator conventions: the `operator fun`s that Kotlin's syntax calls.
//!
//! `a + b` calls `plus`, `a[i]` calls `get`, `a(x)` calls `invoke`, `for (x in a)` calls
//! `iterator`, `by` calls `getValue`, and so on. This module finds the call behind an operator
//! token ([`Analyzer::operator_targets`], for go to definition and hover), picks the overload that
//! the operands fit, gives inference the result type of the call, and finds the uses of an
//! operator function ([`operator_uses`], for find references).

use std::ops::Range;

use super::diagnostics::{Problem, Severity};
use super::features::{self, Control};
use super::index::{SymId, UnitId};
use super::lexer::{HARD_KEYWORDS, Tok};
use super::resolve::{Analyzer, Arg, Ctx, Found, Member, Subst, Ty, chain_start, is_operand_end, matching_gt, matching_lt, strip_nullable};
use super::syntax::{self, InitKind, Kind};

/// Binds tighter than every binary operator.
const PREFIX: u8 = 11;

/// The operator functions with the signature a new one gets in completion.
pub const CONVENTIONS: &[(&str, &str, &str)] = &[
    ("plus", "other: T", "T"),
    ("minus", "other: T", "T"),
    ("times", "other: T", "T"),
    ("div", "other: T", "T"),
    ("rem", "other: T", "T"),
    ("rangeTo", "that: T", "ClosedRange<T>"),
    ("rangeUntil", "that: T", "OpenEndRange<T>"),
    ("contains", "element: T", "Boolean"),
    ("unaryPlus", "", "T"),
    ("unaryMinus", "", "T"),
    ("not", "", "T"),
    ("inc", "", "T"),
    ("dec", "", "T"),
    ("get", "index: Int", "T"),
    ("set", "index: Int, value: T", "Unit"),
    ("invoke", "", "T"),
    ("plusAssign", "other: T", "Unit"),
    ("minusAssign", "other: T", "Unit"),
    ("timesAssign", "other: T", "Unit"),
    ("divAssign", "other: T", "Unit"),
    ("remAssign", "other: T", "Unit"),
    ("compareTo", "other: T", "Int"),
    ("equals", "other: Any?", "Boolean"),
    ("iterator", "", "Iterator<T>"),
    ("hasNext", "", "Boolean"),
    ("next", "", "T"),
    ("component1", "", "T"),
    ("component2", "", "T"),
    ("component3", "", "T"),
    ("component4", "", "T"),
    ("component5", "", "T"),
    ("getValue", "thisRef: Any?, property: KProperty<*>", "T"),
    ("setValue", "thisRef: Any?, property: KProperty<*>, value: T", "Unit"),
    ("provideDelegate", "thisRef: Any?, property: KProperty<*>", "D"),
];

/// The function that the binary operator `op` calls.
pub fn binary_name(op: &str) -> Option<&'static str> {
    Some(match op {
        "+" => "plus",
        "-" => "minus",
        "*" => "times",
        "/" => "div",
        "%" => "rem",
        ".." => "rangeTo",
        "..<" => "rangeUntil",
        _ => return None,
    })
}

/// The function that `op=` calls, and the one it falls back to (`a = a op b`).
fn assign_names(op: &str) -> Option<(&'static str, &'static str)> {
    Some(match op {
        "+=" => ("plusAssign", "plus"),
        "-=" => ("minusAssign", "minus"),
        "*=" => ("timesAssign", "times"),
        "/=" => ("divAssign", "div"),
        "%=" => ("remAssign", "rem"),
        _ => return None,
    })
}

/// How tightly a binary operator binds; assignments end every operand.
fn binary_prec(op: &str) -> Option<u8> {
    Some(match op {
        "as" | "as?" => 10,
        "*" | "/" | "%" => 10,
        "+" | "-" => 9,
        ".." | "..<" => 8,
        "?:" => 6,
        "in" | "is" => 5,
        "<" | ">" | "<=" | ">=" => 4,
        "==" | "!=" | "===" | "!==" => 3,
        "&&" => 2,
        "||" => 1,
        "+=" | "-=" | "*=" | "/=" | "%=" => 0,
        _ => return None,
    })
}

/// The call that an operator stands for: the function `name` on `receiver`, or `fallback` when
/// there is no `name`.
#[derive(Debug, Clone)]
pub struct Site {
    pub receiver: Ty,
    pub name: &'static str,
    pub fallback: Option<&'static str>,
    pub args: Vec<Arg>,
}

fn arg(start: usize, end: usize) -> Arg {
    Arg { start, end, name: None, lambda: false, trailing: false }
}

impl<'a> Analyzer<'a> {
    /// The candidates of the operator call `site`, the ones the arguments fit best first. Members
    /// win over extensions of the same fit, as in the compiler.
    pub fn operator_members(&self, u: UnitId, site: &Site) -> Vec<Member> {
        // The candidates, and whether the arguments fit any of them.
        let pick = |name: &str| -> (Vec<Member>, bool) {
            let cands: Vec<Member> =
                self.members_named(&site.receiver, name, u).into_iter().filter(|m| self.decl(m.sym).kind == Kind::Function).collect();
            if cands.is_empty() {
                return (cands, false);
            }
            let types = self.arg_types(u, &site.args);
            let fits: Vec<Option<i32>> = cands.iter().map(|m| self.call_fit(m, &site.args, &types, u)).collect();
            let best = fits.iter().flatten().max().copied();
            let mut out: Vec<(Member, bool)> = cands
                .into_iter()
                .zip(fits)
                .filter(|(_, f)| best.is_none() || *f == best)
                .map(|(m, _)| {
                    let extension = self.decl(m.sym).is_extension();
                    (m, extension)
                })
                .collect();
            if out.iter().any(|(_, e)| !*e) {
                out.retain(|(_, e)| !*e);
            }
            (out.into_iter().map(|(m, _)| m).collect(), best.is_some())
        };
        // `a += b` is `a = a + b` when no `plusAssign` takes `b`.
        let (found, fits) = pick(site.name);
        match site.fallback {
            Some(name) if !fits => {
                let (fallback, fallback_fits) = pick(name);
                if fallback_fits || found.is_empty() { fallback } else { found }
            }
            _ => found,
        }
    }

    /// The type of the operator call `name` on `receiver` with `args`.
    pub fn operator_ret(&self, u: UnitId, receiver: &Ty, name: &'static str, args: Vec<Arg>) -> Option<Ty> {
        let site = Site { receiver: receiver.clone(), name, fallback: None, args };
        let chosen = self.operator_members(u, &site).into_iter().next()?;
        let mut subst = chosen.subst.clone();
        self.bind_args(&chosen, &mut subst, u, &site.args, false);
        self.value_ty(chosen.sym, &subst)
    }

    /// The functions that the operator at token `tok` calls; none when `tok` is no operator
    /// or its operands are unknown.
    pub fn operator_targets(&self, u: UnitId, tok: usize) -> Vec<Member> {
        let mut out: Vec<Member> = Vec::new();
        for site in self.operator_sites(u, tok) {
            for m in self.operator_members(u, &site) {
                if !out.iter().any(|o| o.sym == m.sym) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// The calls that token `tok` makes as an operator.
    pub fn operator_sites(&self, u: UnitId, tok: usize) -> Vec<Site> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let Some(t) = lx.tokens.get(tok).copied() else { return Vec::new() };
        let word = &src[t.span()];
        let n = lx.tokens.len();
        let site = |receiver: Option<Ty>, name: &'static str, args: Vec<Arg>| {
            receiver.map(|receiver| Site { receiver, name, fallback: None, args })
        };
        let one = |s: Option<Site>| s.into_iter().collect::<Vec<_>>();
        match t.kind {
            Tok::Op | Tok::Lt | Tok::Gt => {
                if matches!(word, "++" | "--") {
                    let name = if word == "++" { "inc" } else { "dec" };
                    let receiver = if self.binary_at(u, tok) {
                        self.infer(u, chain_start(src, lx, tok), tok)
                    } else {
                        self.infer(u, tok + 1, self.right_end(u, tok + 1, PREFIX))
                    };
                    return one(site(receiver, name, Vec::new()));
                }
                if !self.binary_at(u, tok) {
                    let name = match word {
                        "-" => "unaryMinus",
                        "+" => "unaryPlus",
                        "!" => "not",
                        _ => return Vec::new(),
                    };
                    return one(site(self.infer(u, tok + 1, self.right_end(u, tok + 1, PREFIX)), name, Vec::new()));
                }
                let Some(prec) = binary_prec(word) else { return Vec::new() };
                // The brackets of type arguments are no comparisons.
                if word == "<" && matching_gt(lx, tok).is_some() {
                    return Vec::new();
                }
                if word == ">" && matching_lt(lx, tok).is_some_and(|lt| matching_gt(lx, lt) == Some(tok)) {
                    return Vec::new();
                }
                let start = self.left_start(u, tok, prec);
                let end = self.right_end(u, tok + 1, prec);
                let left = self.infer(u, start, tok);
                let right = vec![arg(tok + 1, end)];
                if let Some((name, fallback)) = assign_names(word) {
                    return left.map(|receiver| Site { receiver, name, fallback: Some(fallback), args: right }).into_iter().collect();
                }
                let name = match word {
                    "<" | ">" | "<=" | ">=" => "compareTo",
                    "==" | "!=" => "equals",
                    _ => match binary_name(word) {
                        Some(name) => name,
                        None => return Vec::new(),
                    },
                };
                // The deprecated `mod` stands in for `rem`.
                let fallback = (name == "rem").then_some("mod");
                left.map(|receiver| Site { receiver, name, fallback, args: right }).into_iter().collect()
            }
            Tok::Ident if word == "in" => {
                // `for (x in a)`: `iterator`, then `hasNext` and `next` of what it returns.
                if let Some(open) = self.for_open(u, tok) {
                    let close = lx.matching[open].filter(|&c| c > open).unwrap_or(n);
                    let Some(iterated) = self.infer(u, tok + 1, close) else { return Vec::new() };
                    let mut sites = vec![Site { receiver: iterated.clone(), name: "iterator", fallback: None, args: Vec::new() }];
                    if let Some(iterator) = self.operator_ret(u, &iterated, "iterator", Vec::new()) {
                        for name in ["hasNext", "next"] {
                            sites.push(Site { receiver: iterator.clone(), name, fallback: None, args: Vec::new() });
                        }
                    }
                    return sites;
                }
                // `x in a`, `x !in a` and a `when` branch `in a ->`: `a.contains(x)`.
                let negated = tok > 0 && lx.is(src, tok - 1, "!") && !lx.tokens[tok].nl;
                let binary = self.binary_at(u, tok) || negated;
                let end = self.right_end(u, tok + 1, 5);
                let args = if binary {
                    let left_end = if negated { tok - 1 } else { tok };
                    vec![arg(self.left_start(u, left_end, 5), left_end)]
                } else {
                    Vec::new()
                };
                one(site(self.infer(u, tok + 1, end), "contains", args))
            }
            Tok::Ident if word == "by" => {
                // A property delegate: `getValue`, `setValue` for a `var`, and `provideDelegate`.
                let Some(d) = file.decls.iter().find(|d| d.init_kind == InitKind::Delegate && d.init == Some(tok + 1)) else {
                    return Vec::new();
                };
                let end = syntax::expr_end(src, lx, tok + 1, n);
                let Some(delegate) = self.infer(u, tok + 1, end) else { return Vec::new() };
                let mut names = vec!["getValue", "provideDelegate"];
                if d.mutable {
                    names.push("setValue");
                }
                names.into_iter().map(|name| Site { receiver: delegate.clone(), name, fallback: None, args: Vec::new() }).collect()
            }
            Tok::LBracket | Tok::RBracket => {
                let open = if t.kind == Tok::RBracket { lx.matching[tok].filter(|&o| o < tok) } else { Some(tok) };
                let Some(open) = open else { return Vec::new() };
                let Some(close) = lx.matching[open].filter(|&c| c > open) else { return Vec::new() };
                if open == 0 || lx.tokens[open].nl || !is_operand_end(src, lx, open - 1) {
                    return Vec::new();
                }
                let Some(receiver) = self.infer(u, chain_start(src, lx, open), open) else { return Vec::new() };
                let (args, _) = self.call_args(u, open, n);
                let args: Vec<Arg> = args.into_iter().filter(|a| !a.trailing).collect();
                let next = lx.kind(close + 1);
                let value_end = syntax::expr_end(src, lx, (close + 2).min(n), n);
                let mut sites = Vec::new();
                let assigned = next == Some(Tok::Eq);
                let augmented = next == Some(Tok::Op) && assign_names(lx.text(src, close + 1)).is_some();
                if !assigned {
                    sites.push(Site { receiver: receiver.clone(), name: "get", fallback: None, args: args.clone() });
                }
                if assigned || augmented {
                    let mut set = args;
                    if assigned {
                        set.push(arg(close + 2, value_end));
                    }
                    sites.push(Site { receiver, name: "set", fallback: None, args: set });
                }
                sites
            }
            Tok::LParen | Tok::LBrace => {
                let Some(receiver) = self.invoke_receiver(u, tok) else { return Vec::new() };
                let (args, _) = self.call_args(u, tok, n);
                one(site(Some(receiver), "invoke", args))
            }
            _ => Vec::new(),
        }
    }

    /// The value that `(` or `{` at `tok` calls through `invoke`: a variable, parameter or
    /// property of a type that is no function type, or the result of a call.
    fn invoke_receiver(&self, u: UnitId, tok: usize) -> Option<Ty> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let prev = tok.checked_sub(1)?;
        if lx.tokens[tok].nl {
            return None;
        }
        let ty = match lx.kind(prev)? {
            Tok::Ident => {
                let word = lx.text(src, prev);
                if HARD_KEYWORDS.contains(&word) && !matches!(word, "this" | "super") {
                    return None;
                }
                // `fun name(`, `class Name(`: a declaration, not a call.
                if file.decls_named_at(&lx.tokens[prev].span()).iter().any(|&d| !file.decls[d].name.is_empty()) {
                    return None;
                }
                let found: Vec<Member> = if prev > 0 && matches!(lx.kind(prev - 1), Some(Tok::Dot | Tok::SafeDot)) {
                    let receiver = self.infer(u, chain_start(src, lx, prev - 1), prev - 1)?;
                    self.members_named(&strip_nullable(receiver), word, u)
                } else {
                    self.resolve_name(u, lx.tokens[prev].start, word)
                        .into_iter()
                        .filter_map(|f| if let Found::Sym(m) = f { Some(m) } else { None })
                        .collect()
                };
                if found.is_empty() || found.iter().any(|m| !self.decl(m.sym).kind.is_value()) {
                    return None;
                }
                let first = &found[0];
                self.value_ty(first.sym, &first.subst)?
            }
            Tok::RParen | Tok::RBracket if lx.kind(tok) == Some(Tok::LParen) => {
                let start = chain_start(src, lx, tok);
                if start == 0 || start >= prev || lx.kind(start - 1).is_some_and(|k| k == Tok::Ident && lx.is(src, start - 1, "if")) {
                    return None;
                }
                self.infer(u, start, tok)?
            }
            _ => return None,
        };
        match ty {
            Ty::Class { .. } => Some(ty),
            _ => None,
        }
    }

    /// The `(` of the `for (` whose iterated expression the `in` at `tok` starts.
    fn for_open(&self, u: UnitId, tok: usize) -> Option<usize> {
        let lx = &self.file(u).lexed;
        let src = &self.unit(u).text;
        let mut k = tok;
        while k > 0 {
            k -= 1;
            match lx.kind(k)? {
                Tok::RParen | Tok::RBracket | Tok::RBrace => k = lx.matching[k].filter(|&o| o < k)?,
                Tok::LParen => return (k > 0 && lx.is(src, k - 1, "for")).then_some(k),
                Tok::LBracket | Tok::LBrace | Tok::Semi => return None,
                _ => {}
            }
        }
        None
    }

    /// Whether the operator token `k` has an operand before it (is binary or postfix).
    fn binary_at(&self, u: UnitId, k: usize) -> bool {
        let lx = &self.file(u).lexed;
        let src = &self.unit(u).text;
        k > 0 && is_operand_end(src, lx, k - 1) && (!lx.tokens[k].nl || syntax::continues(src, lx, k))
    }

    /// Where the left operand of the binary operator at `tok` (of precedence `prec`) starts.
    pub fn left_start(&self, u: UnitId, tok: usize, prec: u8) -> usize {
        let lx = &self.file(u).lexed;
        let src = &self.unit(u).text;
        let mut k = tok;
        while k > 0 {
            let j = k - 1;
            let t = lx.tokens[j];
            let word = &src[t.span()];
            let next = match t.kind {
                Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose => match lx.matching[j].filter(|&o| o < j) {
                    Some(o) => o,
                    None => break,
                },
                Tok::Gt => match matching_lt(lx, j) {
                    Some(lt) => lt,
                    None if 4 < prec => break,
                    None => j,
                },
                Tok::Lt => {
                    if 4 < prec {
                        break;
                    }
                    j
                }
                Tok::Op => {
                    if self.binary_at(u, j) && binary_prec(word).is_none_or(|p| p < prec) {
                        break;
                    }
                    j
                }
                Tok::Ident => {
                    if matches!(word, "is" | "in" | "as") {
                        if binary_prec(word).is_none_or(|p| p < prec) {
                            break;
                        }
                    } else if HARD_KEYWORDS.contains(&word) && !matches!(word, "this" | "super" | "null" | "true" | "false") {
                        break;
                    } else if j > 0 && is_operand_end(src, lx, j - 1) && !lx.tokens[j].nl && 7 < prec {
                        // An infix call.
                        break;
                    }
                    j
                }
                Tok::Dot | Tok::SafeDot | Tok::ColonColon | Tok::BangBang | Tok::Int | Tok::Float | Tok::Char | Tok::LabelRef => j,
                _ => break,
            };
            k = next;
            if lx.tokens[k].nl && k > 0 && !syntax::continues(src, lx, k) {
                break;
            }
        }
        k
    }

    /// Where the right operand of an operator of precedence `prec` that ends at `from - 1` ends
    /// (exclusive).
    pub fn right_end(&self, u: UnitId, from: usize, prec: u8) -> usize {
        let lx = &self.file(u).lexed;
        let src = &self.unit(u).text;
        let n = lx.tokens.len();
        let mut k = from;
        while k < n {
            let t = lx.tokens[k];
            let word = &src[t.span()];
            if k > from && t.nl && !syntax::continues(src, lx, k) {
                break;
            }
            match t.kind {
                Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen => {
                    k = lx.matching[k].filter(|&c| c > k).map_or(n, |c| c + 1);
                    continue;
                }
                Tok::Lt => {
                    if let Some(gt) = matching_gt(lx, k) {
                        k = gt + 1;
                        continue;
                    }
                    if k > from && 4 <= prec {
                        break;
                    }
                }
                Tok::Gt if k > from && 4 <= prec => break,
                Tok::Op if k > from && is_operand_end(src, lx, k - 1) => {
                    if binary_prec(word).is_none_or(|p| p <= prec) {
                        break;
                    }
                }
                Tok::Ident if k > from => {
                    if matches!(word, "is" | "in" | "as") {
                        if binary_prec(word).is_none_or(|p| p <= prec) {
                            break;
                        }
                    } else if (HARD_KEYWORDS.contains(&word) && !matches!(word, "this" | "super" | "null" | "true" | "false"))
                        // An infix call.
                        || (is_operand_end(src, lx, k - 1) && !matches!(lx.kind(k - 1), Some(Tok::Dot | Tok::SafeDot)) && 7 <= prec)
                    {
                        break;
                    }
                }
                Tok::Comma
                | Tok::Semi
                | Tok::Eq
                | Tok::Colon
                | Tok::Arrow
                | Tok::RParen
                | Tok::RBracket
                | Tok::RBrace
                | Tok::StrClose
                | Tok::Str => break,
                _ => {}
            }
            k += 1;
        }
        k
    }
}

/// Which tokens can call an operator function named `name`.
fn operator_tokens(name: &str) -> &'static [&'static str] {
    match name {
        "plus" => &["+", "+="],
        "minus" => &["-", "-="],
        "times" => &["*", "*="],
        "div" => &["/", "/="],
        "rem" | "mod" => &["%", "%="],
        "plusAssign" => &["+="],
        "minusAssign" => &["-="],
        "timesAssign" => &["*="],
        "divAssign" => &["/="],
        "remAssign" => &["%="],
        "rangeTo" => &[".."],
        "rangeUntil" => &["..<"],
        "contains" | "iterator" | "hasNext" | "next" => &["in"],
        "unaryPlus" => &["+"],
        "unaryMinus" => &["-"],
        "not" => &["!"],
        "inc" => &["++"],
        "dec" => &["--"],
        "get" | "set" => &["["],
        "invoke" => &["(", "{"],
        "compareTo" => &["<", ">", "<=", ">="],
        "equals" => &["==", "!="],
        "getValue" | "setValue" | "provideDelegate" => &["by"],
        _ => &[],
    }
}

/// The places that call the operator function in `group` through syntax: `a + b`, `a[i]`,
/// `for (x in a)`, `val (x, y) = a`, ... Each range is the operator token (or the name of a
/// variable a `componentN` fills), so find references can list them next to the named uses.
pub fn operator_uses(an: &Analyzer, group: &[SymId], control: &Control) -> Option<Vec<(UnitId, Range<usize>)>> {
    let Some(&first) = group.first() else { return Some(Vec::new()) };
    let d = an.decl(first);
    if d.kind != Kind::Function || !d.has("operator") {
        return Some(Vec::new());
    }
    let component = d.name.strip_prefix("component").and_then(|n| n.parse::<usize>().ok());
    let tokens = operator_tokens(&d.name);
    let mut out = Vec::new();
    if tokens.is_empty() && component.is_none() {
        return Some(out);
    }
    let local = d.local || d.is_private();
    for (ui, unit) in an.proj.units.iter().enumerate() {
        let uid = UnitId { lib: false, unit: ui as u32 };
        if unit.is_java() || (local && !first.lib && uid != first.unit_id()) {
            continue;
        }
        if control.cancelled.is_some_and(|c| c()) {
            return None;
        }
        let lx = &unit.file.lexed;
        for i in 0..lx.tokens.len() {
            let text = lx.text(&unit.text, i);
            if !tokens.contains(&text) {
                continue;
            }
            if control.cancelled.is_some_and(|c| c()) {
                return None;
            }
            if an.operator_targets(uid, i).iter().any(|m| group.contains(&m.sym)) {
                out.push((uid, lx.tokens[i].span()));
            }
        }
        if let Some(n) = component {
            for d in &unit.file.decls {
                let (InitKind::Component(c) | InitKind::IteratedComponent(c)) = d.init_kind else { continue };
                if c != n || d.name.is_empty() {
                    continue;
                }
                let Some(start) = d.init else { continue };
                let end = if d.init_kind == InitKind::Component(c) {
                    syntax::expr_end(&unit.text, lx, start, lx.tokens.len())
                } else {
                    an.iterated_end(uid, start)
                };
                let Some(value) = an.infer(uid, start, end) else { continue };
                let value = if matches!(d.init_kind, InitKind::IteratedComponent(_)) { an.element_ty(&value, uid) } else { Some(value) };
                let Some(value) = value else { continue };
                let site = Site { receiver: value, name: "component", fallback: None, args: Vec::new() };
                let name = format!("component{c}");
                let members = an.members_named(&site.receiver, &name, uid);
                if members.iter().any(|m| group.contains(&m.sym)) {
                    out.push((uid, d.name_span.clone()));
                }
            }
        }
    }
    out.sort_by_key(|(u, r)| (u.unit, r.start));
    out.dedup();
    Some(out)
}

/// Problems with the operators of `u` that the compiler would report, for operators on classes
/// of the project: a function that syntax calls but is not marked `operator`, and a class with
/// no function the operator can call. Types the server cannot see all of (a class with a
/// supertype it does not know) are left alone.
pub fn problems(an: &Analyzer, u: UnitId) -> Vec<Problem> {
    let file = an.file(u);
    let lx = &file.lexed;
    let src = &an.unit(u).text;
    let mut out = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        let word = &src[t.span()];
        if t.kind == Tok::Ident && word == "by" {
            out.extend(delegate_problem(an, u, i));
            continue;
        }
        let checked = match t.kind {
            Tok::Op => !matches!(word, "==" | "!=" | "===" | "!==" | "&&" | "||" | "?:" | "!!"),
            Tok::Lt | Tok::Gt | Tok::LBracket | Tok::LParen => true,
            Tok::Ident => word == "in",
            _ => false,
        };
        if !checked {
            continue;
        }
        for site in an.operator_sites(u, i) {
            if let Some(message) = operator_problem(an, u, &site) {
                out.push(Problem::new(t.span(), Severity::Error, message));
                break;
            }
        }
    }
    out
}

/// What is wrong with the delegate of the property declared with the `by` at token `tok`: no
/// `getValue` (or `setValue` for a `var`) to call, a `getValue` without `operator`, or a type
/// that is not the declared one. Delegates the server cannot see all of are left alone.
fn delegate_problem(an: &Analyzer, u: UnitId, tok: usize) -> Option<Problem> {
    let file = an.file(u);
    let lx = &file.lexed;
    let src = &an.unit(u).text;
    let d = file.decls.iter().find(|d| d.init_kind == InitKind::Delegate && d.init == Some(tok + 1))?;
    let end = syntax::expr_end(src, lx, tok + 1, lx.tokens.len());
    let mut delegate = an.infer(u, tok + 1, end)?;
    let functions = |ty: &Ty, name: &str| -> Vec<Member> {
        an.members_named(ty, name, u).into_iter().filter(|m| an.decl(m.sym).kind == Kind::Function).collect()
    };
    if let Some(m) = functions(&delegate, "provideDelegate").first() {
        delegate = an.value_ty(m.sym, &m.subst)?;
    }
    let Ty::Class { sym, .. } = &delegate else { return None };
    let own = |s: &super::index::SymId| !s.lib && !an.unit(s.unit_id()).is_java();
    let span = lx.tokens[tok].span();
    let shown = features::display_ty(an, &delegate);
    // Only classes of the project, whose supertypes are all known.
    let class_known = own(sym) && {
        let ctx = Ctx { unit: sym.unit_id(), decl: Some(sym.decl_id()) };
        an.decl(*sym).kind.is_class() && an.decl(*sym).supertypes.iter().all(|t| an.ty_of_ref(t, ctx, &Subst::new()).is_some())
    };
    let mut needed = vec!["getValue"];
    if d.mutable {
        needed.push("setValue");
    }
    for name in needed {
        let found = functions(&delegate, name);
        if found.is_empty() {
            if class_known {
                let message = format!("the delegate `{shown}` has no `{name}` operator function");
                return Some(Problem::new(span, Severity::Error, message));
            }
            continue;
        }
        if found.iter().all(|m| own(&m.sym) && !an.decl(m.sym).has("operator")) {
            let message = format!("the `operator` modifier is required on `{name}` to delegate a property to it");
            return Some(Problem::new(span, Severity::Error, message));
        }
    }
    let declared = d
        .ty
        .as_ref()
        .and_then(|t| an.ty_of_ref(t, Ctx { unit: u, decl: Some(file.decls.iter().position(|x| std::ptr::eq(x, d))?) }, &Subst::new()))?;
    let got = functions(&delegate, "getValue").into_iter().find_map(|m| an.value_ty(m.sym, &m.subst))?;
    if features::fully_known(&declared) && features::fully_known(&got) && an.type_fit(&got, &declared).is_none() {
        let message = format!(
            "the type of the delegate is `{}`, not the declared `{}`",
            features::display_ty(an, &got),
            features::display_ty(an, &declared)
        );
        return Some(Problem::new(span, Severity::Error, message));
    }
    None
}

fn operator_problem(an: &Analyzer, u: UnitId, site: &Site) -> Option<String> {
    let Ty::Class { sym, .. } = &site.receiver else { return None };
    if sym.lib || an.unit(sym.unit_id()).is_java() || !an.decl(*sym).kind.is_class() {
        return None;
    }
    let receiver = features::display_ty(an, &site.receiver);
    let named = |name: &str| -> Vec<Member> {
        an.members_named(&site.receiver, name, u).into_iter().filter(|m| an.decl(m.sym).kind == Kind::Function).collect()
    };
    let mut cands = named(site.name);
    if let Some(fallback) = site.fallback {
        cands.extend(named(fallback));
    }
    if cands.is_empty() {
        let ctx = Ctx { unit: sym.unit_id(), decl: Some(sym.decl_id()) };
        let known = an.decl(*sym).supertypes.iter().all(|t| an.ty_of_ref(t, ctx, &Subst::new()).is_some());
        return known.then(|| format!("`{receiver}` has no operator function `{}`", site.name));
    }
    let chosen = an.operator_members(u, site);
    let own = |m: &Member| !m.sym.lib && !an.unit(m.sym.unit_id()).is_java();
    if !chosen.is_empty() && chosen.iter().all(|m| own(m) && !an.decl(m.sym).has("operator")) {
        let name = &an.decl(chosen[0].sym).name;
        return Some(format!("the `operator` modifier is required on `{name}` to call it with an operator"));
    }
    let types = an.arg_types(u, &site.args);
    if !types.is_empty() && types.iter().all(Option::is_some) && cands.iter().all(|m| an.call_fit(m, &site.args, &types, u).is_none()) {
        let shown: Vec<String> = types.iter().flatten().map(|t| features::display_ty(an, t)).collect();
        return Some(format!("`{receiver}` has no operator function `{}` that takes `{}`", site.name, shown.join(", ")));
    }
    None
}
