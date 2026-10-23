//! Refactoring code actions for Kotlin files (`refactor.*`): extracting a variable or a function,
//! inlining a variable or a function, changing a function's signature (moving or removing a
//! parameter) and moving a top-level declaration to another package or to a file of its own.
//!
//! They are built on the declaration parser, the scopes and the reference search that rename
//! uses, and edit through one `WorkspaceEdit` across files. A refactoring is offered only when
//! every name it moves or rewrites resolves and every use it rewrites is one it understands (a
//! call with its arguments in parentheses, an import, a qualified name), so it never changes the
//! meaning of code it can't fully resolve. Side effects stay where they were: an expression with
//! calls only moves to where it is evaluated once, unconditionally and in the same order.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;

use serde_json::{Value, json};

use super::actions::{
    FileChange, Request, SOFT_KEYWORDS, collect_imports, escape_path, import_edit, indent_at, line_start, text_edit, type_params_text,
    workspace_edit,
};
use super::features::{Control, Target, display_ty, escape_name, fully_known, rename_group, search, targets_at};
use super::index::{Origin, SymId, UnitId};
use super::lexer::{HARD_KEYWORDS, Lexed, Tok};
use super::resolve::{Analyzer, Ctx, Found, Subst, Ty, is_operand_end, matching_gt};
use super::syntax::{Decl, InitKind, Kind, ScopeId, ScopeKind, continues, expr_end};
use crate::text;

pub const EXTRACT: &str = "refactor.extract";
pub const INLINE: &str = "refactor.inline";
pub const REWRITE: &str = "refactor.rewrite";
pub const MOVE: &str = "refactor.move";

/// The refactorings for a `textDocument/codeAction` request on a Kotlin file.
pub fn code_actions(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    if req.wants(EXTRACT) {
        extract_variable(an, req, out);
        extract_function(an, req, out);
    }
    if req.wants(INLINE) {
        inline_variable(an, req, out);
        inline_function(an, req, out);
    }
    if req.wants(REWRITE) {
        change_signature(an, req, out);
    }
    if req.wants(MOVE) {
        move_declaration(an, req, out);
    }
}

// ----- edits -----

/// The edits of one refactoring, by file, and the files it creates.
#[derive(Default)]
struct Edits {
    by_unit: BTreeMap<UnitId, Vec<Value>>,
    imports: HashSet<(UnitId, String)>,
    created: Vec<FileChange>,
}

impl Edits {
    fn replace(&mut self, an: &Analyzer, u: UnitId, range: Range<usize>, new_text: impl Into<String>) {
        self.by_unit.entry(u).or_default().push(text_edit(&an.unit(u).text, range, new_text));
    }

    /// Imports `fq` into `u`, once.
    fn import(&mut self, an: &Analyzer, u: UnitId, fq: &str) {
        let unit = an.unit(u);
        if unit.file.imports.iter().any(|i| i.alias.is_none() && i.fq() == fq) || !self.imports.insert((u, fq.to_owned())) {
            return;
        }
        self.by_unit.entry(u).or_default().push(import_edit(unit, fq));
    }

    fn action(self, an: &Analyzer, req: &Request, title: String, kind: &str) -> Option<Value> {
        let mut changes: Vec<FileChange> = self
            .by_unit
            .into_iter()
            .map(|(u, edits)| FileChange { uri: text::path_to_uri(&an.unit(u).path), edits, create: false })
            .collect();
        changes.extend(self.created);
        let edit = workspace_edit(req, &changes)?;
        Some(json!({ "title": title, "kind": kind, "edit": edit }))
    }
}

// ----- expressions and statements -----

/// How tightly an expression binds: an atom or a postfix chain (`a.b(c)[d]!!`).
const ATOM: u8 = 15;
/// A prefix operator: `-a`, `!a`.
const PREFIX: u8 = 14;

fn is_compound_assignment(text: &str) -> bool {
    matches!(text, "+=" | "-=" | "*=" | "/=" | "%=")
}

/// How tightly the binary operator at token `k` binds (higher binds tighter, as in Kotlin's
/// grammar), or `None` when it is no binary operator.
fn binary_prec(src: &str, lx: &Lexed, k: usize) -> Option<u8> {
    let t = lx.tokens[k];
    let text = &src[t.span()];
    match t.kind {
        Tok::Op => match text {
            "*" | "/" | "%" => Some(12),
            "+" | "-" => Some(11),
            ".." | "..<" => Some(10),
            "?:" => Some(8),
            "<=" | ">=" => Some(6),
            "==" | "!=" | "===" | "!==" => Some(5),
            "&&" => Some(4),
            "||" => Some(3),
            "!" if lx.is(src, k + 1, "in") || lx.is(src, k + 1, "is") => Some(7),
            _ => None,
        },
        Tok::Lt | Tok::Gt => Some(6),
        Tok::Ident => match text {
            "as" => Some(13),
            "in" | "is" => Some(7),
            _ if HARD_KEYWORDS.contains(&text) || matches!(text, "catch" | "finally") => None,
            // An infix call: `a to b`.
            _ => Some(9),
        },
        _ => None,
    }
}

/// How tightly the expression in tokens `toks` binds: its loosest operator outside brackets, or
/// 0 when it isn't one expression that can stand in another (`if`, `return`, an assignment).
fn expr_prec(src: &str, lx: &Lexed, toks: Range<usize>) -> u8 {
    let mut p = ATOM;
    let mut k = toks.start;
    while k < toks.end {
        let t = lx.tokens[k];
        let text = &src[t.span()];
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen => {
                k = lx.matching[k].filter(|&c| c > k).map_or(toks.end, |c| c + 1);
                continue;
            }
            Tok::Lt => {
                if let Some(gt) = matching_gt(lx, k).filter(|&g| g < toks.end) {
                    k = gt + 1;
                    continue;
                }
            }
            Tok::Eq | Tok::Arrow | Tok::Semi => return 0,
            Tok::Ident if matches!(text, "if" | "return" | "throw" | "break" | "continue" | "val" | "var" | "fun") => return 0,
            Tok::Op if is_compound_assignment(text) => return 0,
            _ => {}
        }
        if k > toks.start && is_operand_end(src, lx, k - 1) {
            if let Some(q) = binary_prec(src, lx, k) {
                p = p.min(q);
            }
        } else if t.kind == Tok::Op && matches!(text, "-" | "+" | "!" | "++" | "--") {
            p = p.min(PREFIX);
        }
        k += 1;
    }
    p
}

/// Whether an expression that binds with `p` can take the place of tokens `toks` without
/// parentheses.
fn fits(src: &str, lx: &Lexed, toks: &Range<usize>, p: u8) -> bool {
    p > 0 && left_fits(src, lx, toks.start, p) && right_fits(src, lx, toks.end, p)
}

fn left_fits(src: &str, lx: &Lexed, first: usize, p: u8) -> bool {
    let Some(l) = first.checked_sub(1) else { return true };
    if lx.tokens[first].nl && !continues(src, lx, first) {
        return true;
    }
    let t = lx.tokens[l];
    let text = &src[t.span()];
    let binary = l > 0 && is_operand_end(src, lx, l - 1);
    match t.kind {
        Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::Comma | Tok::TemplateOpen | Tok::Eq | Tok::Arrow | Tok::Semi => true,
        Tok::Ident if matches!(text, "return" | "throw" | "else") => true,
        Tok::Ident if matches!(text, "is" | "as") => false,
        Tok::Op if matches!(text, "++" | "--") => false,
        Tok::Op if is_compound_assignment(text) => true,
        Tok::Op | Tok::Lt | Tok::Gt | Tok::Ident if binary => binary_prec(src, lx, l).is_some_and(|q| q < p),
        Tok::Op if matches!(text, "-" | "+" | "!") => p > PREFIX,
        _ => false,
    }
}

fn right_fits(src: &str, lx: &Lexed, end: usize, p: u8) -> bool {
    let Some(t) = lx.tokens.get(end).copied() else { return true };
    if t.nl && !continues(src, lx, end) {
        return true;
    }
    let text = &src[t.span()];
    match t.kind {
        Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Comma | Tok::Semi | Tok::StrClose | Tok::Arrow => true,
        Tok::Dot | Tok::SafeDot | Tok::LBracket | Tok::BangBang => p == ATOM,
        Tok::Ident if matches!(text, "else" | "catch" | "finally") => true,
        Tok::Lt if matching_gt(lx, end).is_some() => false,
        Tok::Op if matches!(text, "++" | "--") || is_compound_assignment(text) => false,
        _ => binary_prec(src, lx, end).is_some_and(|q| q <= p),
    }
}

/// Whether evaluating tokens `toks` can't have side effects: no calls, indexing, lambdas or
/// assignments.
fn is_pure(src: &str, lx: &Lexed, toks: Range<usize>) -> bool {
    toks.into_iter().all(|k| {
        let t = lx.tokens[k];
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::Eq => false,
            Tok::Op => !matches!(&src[t.span()], "++" | "--") && !is_compound_assignment(&src[t.span()]),
            _ => true,
        }
    })
}

/// Whether the `(` or `{` at token `k` calls something (`f(`, `run {`), rather than opening a
/// condition or a body.
fn is_call(src: &str, lx: &Lexed, k: usize) -> bool {
    let Some(p) = k.checked_sub(1) else { return false };
    let control = |i: usize| {
        ["if", "while", "for", "when", "catch", "else", "try", "do", "finally", "init", "object", "class"].iter().any(|w| lx.is(src, i, w))
    };
    match lx.kind(k) {
        Some(Tok::LParen) => match lx.kind(p) {
            Some(Tok::Ident) => !control(p),
            Some(Tok::RParen | Tok::RBracket | Tok::Gt) => true,
            _ => false,
        },
        Some(Tok::LBrace) if !lx.tokens[k].nl => match lx.kind(p) {
            Some(Tok::Ident) => !control(p) && !HARD_KEYWORDS.contains(&&src[lx.tokens[p].span()]),
            Some(Tok::RParen) => lx.matching[p].and_then(|o| o.checked_sub(1)).is_some_and(|q| !control(q)),
            _ => false,
        },
        _ => false,
    }
}

/// Whether the identifier at token `k` is assigned to there: `x = `, `x += `, `x++`, `--x`.
fn is_assigned(src: &str, lx: &Lexed, k: usize) -> bool {
    let next = lx.tokens.get(k + 1).map(|t| (t.kind, &src[t.span()]));
    let prev = k.checked_sub(1).map(|p| (lx.tokens[p].kind, &src[lx.tokens[p].span()]));
    matches!(next, Some((Tok::Eq, _)))
        || matches!(next, Some((Tok::Op, op)) if is_compound_assignment(op) || op == "++" || op == "--")
        || matches!(prev, Some((Tok::Op, "++" | "--")))
}

/// Whether the expression starting at token `first`, in the statement starting at token `stmt`,
/// runs exactly once each time the statement does, and, unless `pure`, after nothing else with
/// side effects in the statement: moving it to a statement of its own before then changes
/// nothing.
fn evaluated_first(src: &str, lx: &Lexed, stmt: usize, first: usize, pure: bool) -> bool {
    for k in stmt..first {
        let t = lx.tokens[k];
        let text = &src[t.span()];
        let encloses = lx.matching[k].is_some_and(|c| c > first);
        match t.kind {
            Tok::Ident if matches!(text, "else" | "while" | "for" | "do" | "catch" | "finally" | "try") => return false,
            // Only the condition of an `if` and the subject of a `when` always run.
            Tok::Ident if matches!(text, "if" | "when") => {
                if !(lx.kind(k + 1) == Some(Tok::LParen) && lx.matching[k + 1].is_some_and(|c| c > first)) {
                    return false;
                }
            }
            Tok::Op if matches!(text, "&&" | "||" | "?:") => return false,
            Tok::SafeDot | Tok::Arrow => return false,
            Tok::LBrace if encloses => return false,
            // A call or an indexing that ran before.
            Tok::LParen | Tok::LBracket if !pure && !encloses && (t.kind == Tok::LBracket || is_call(src, lx, k)) => return false,
            Tok::Op if !pure && (matches!(text, "++" | "--") || is_compound_assignment(text)) => return false,
            _ => {}
        }
    }
    true
}

/// A `{ ... }` block or lambda body, as statements.
struct Block {
    scope: ScopeId,
    /// The tokens of its braces.
    open: usize,
    close: usize,
    lambda: bool,
    /// The statements, as token ranges.
    stmts: Vec<Range<usize>>,
}

/// The block that holds the code at `offset` directly.
fn block_at(an: &Analyzer, u: UnitId, offset: usize) -> Option<Block> {
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    let scope = file.scope_at(offset);
    let s = &file.scopes[scope];
    let (open, body, lambda) = match s.kind {
        ScopeKind::Block => {
            let open = lx.at_or_after(s.range.start);
            (lx.kind(open) == Some(Tok::LBrace)).then_some((open, open + 1, false))?
        }
        ScopeKind::Lambda { brace, it } => {
            let body = if it { brace + 1 } else { lambda_arrow(lx, brace)? + 1 };
            (brace, body, true)
        }
        // A function's body is in the scope of its parameters.
        ScopeKind::Function(d) => {
            let open = body_brace(&file.decls[d], lx)?;
            (lx.tokens[open].start < offset).then_some((open, open + 1, false))?
        }
        _ => return None,
    };
    let close = lx.matching.get(open).copied().flatten().filter(|&c| c > open)?;
    let mut stmts = Vec::new();
    let mut k = body;
    while k < close {
        if lx.kind(k) == Some(Tok::Semi) {
            k += 1;
            continue;
        }
        let end = expr_end(src, lx, k, close).max(k + 1);
        stmts.push(k..end);
        k = end;
    }
    Some(Block { scope, open, close, lambda, stmts })
}

/// The `{` of a function's block body.
fn body_brace(d: &Decl, lx: &Lexed) -> Option<usize> {
    let close = lx.before(d.span.end).filter(|&c| lx.kind(c) == Some(Tok::RBrace))?;
    let open = lx.matching[close].filter(|&o| o < close && lx.tokens[o].start > d.name_span.end)?;
    // Not the default value of a parameter: the brace comes after the parameter list.
    let params = (lx.at_or_after(d.name_span.end)..open).find(|&k| lx.kind(k) == Some(Tok::LParen)).and_then(|k| lx.matching[k]);
    params.is_none_or(|close| close < open).then_some(open)
}

/// The `->` after the parameters of the lambda whose `{` is token `brace`.
fn lambda_arrow(lx: &Lexed, brace: usize) -> Option<usize> {
    let close = lx.matching[brace]?;
    let mut k = brace + 1;
    while k < close {
        match lx.kind(k)? {
            Tok::Arrow => return Some(k),
            Tok::LParen | Tok::LBracket | Tok::LBrace => k = lx.matching[k].filter(|&c| c > k)? + 1,
            _ => k += 1,
        }
    }
    None
}

/// The tokens of a selection, when it starts and ends on token boundaries and pairs its brackets.
fn selected_tokens(src: &str, lx: &Lexed, range: &Range<usize>) -> Option<Range<usize>> {
    if range.start >= range.end || range.end > src.len() {
        return None;
    }
    let first = lx.at_or_after(range.start);
    let last = lx.before(range.end)?;
    if first > last || lx.tokens[first].start < range.start {
        return None;
    }
    let (a, b) = (lx.tokens[first].start, lx.tokens[last].end);
    if !src[range.start..a].trim().is_empty() || !src[b..range.end].trim().is_empty() {
        return None;
    }
    let paired = (first..=last).all(|k| match lx.kind(k) {
        Some(
            Tok::LParen
            | Tok::RParen
            | Tok::LBracket
            | Tok::RBracket
            | Tok::LBrace
            | Tok::RBrace
            | Tok::StrOpen
            | Tok::StrClose
            | Tok::TemplateOpen,
        ) => lx.matching[k].is_some_and(|m| (first..=last).contains(&m)),
        Some(Tok::Str) => k > first && k < last,
        _ => true,
    });
    paired.then_some(first..last + 1)
}

/// The bytes of tokens `toks`.
fn span_of(lx: &Lexed, toks: &Range<usize>) -> Range<usize> {
    lx.tokens[toks.start].start..lx.tokens[toks.end - 1].end
}

/// Whether some identifier in tokens `toks` that names something resolves to nothing: a
/// refactoring would not know what it moves.
fn unresolved(an: &Analyzer, u: UnitId, toks: Range<usize>) -> bool {
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    toks.into_iter().any(|k| {
        let t = lx.tokens[k];
        if t.kind != Tok::Ident {
            return false;
        }
        let raw = &src[t.span()];
        let prev = k.checked_sub(1).and_then(|p| lx.kind(p));
        if HARD_KEYWORDS.contains(&raw) || SOFT_KEYWORDS.contains(&raw) || prev == Some(Tok::At) {
            return false;
        }
        if !file.decls_named_at(&t.span()).is_empty() {
            return false;
        }
        // A named argument.
        if lx.kind(k + 1) == Some(Tok::Eq) && matches!(prev, Some(Tok::LParen | Tok::Comma)) {
            return false;
        }
        let name = lx.text(src, k);
        if file.decls.iter().any(|d| d.span.start <= t.start && t.end <= d.span.end && d.type_params.iter().any(|p| p.name == name)) {
            return false;
        }
        targets_at(an, u, k).is_empty()
    })
}

/// The identifier tokens in `toks` that are plain names (not keywords, members after a `.`,
/// declarations or named arguments).
fn plain_names(an: &Analyzer, u: UnitId, toks: Range<usize>) -> Vec<usize> {
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    toks.into_iter()
        .filter(|&k| {
            let t = lx.tokens[k];
            let prev = k.checked_sub(1).and_then(|p| lx.kind(p));
            t.kind == Tok::Ident
                && !HARD_KEYWORDS.contains(&&src[t.span()])
                && !matches!(prev, Some(Tok::Dot | Tok::SafeDot | Tok::ColonColon | Tok::At))
                && file.decls_named_at(&t.span()).is_empty()
                && !(lx.kind(k + 1) == Some(Tok::Eq) && matches!(prev, Some(Tok::LParen | Tok::Comma)))
        })
        .collect()
}

/// The declaration the identifier at token `k` refers to first.
fn first_sym(an: &Analyzer, u: UnitId, k: usize) -> Option<SymId> {
    targets_at(an, u, k).first().and_then(Target::sym)
}

/// A type as Kotlin source in the code at `at`, with the imports it needs; `None` when it isn't
/// fully known or a name in it means something else there.
fn type_text(an: &Analyzer, ty: &Ty, at: Ctx) -> Option<(String, BTreeSet<String>)> {
    fn clashes(an: &Analyzer, ty: &Ty, at: Ctx) -> bool {
        match ty {
            Ty::Class { sym, args, .. } => {
                let d = an.decl(*sym);
                d.local
                    || an.resolve_type(at, &d.name).is_some_and(|found| an.fq(found) != an.fq(*sym))
                    || args.iter().flatten().any(|a| clashes(an, a, at))
            }
            Ty::Func { receiver, params, ret } => {
                receiver.iter().chain(ret.iter()).any(|t| clashes(an, t, at)) || params.iter().flatten().any(|t| clashes(an, t, at))
            }
            Ty::Static(_) | Ty::Package(_) => true,
        }
    }
    if !fully_known(ty) || clashes(an, ty, at) {
        return None;
    }
    let mut imports = BTreeSet::new();
    collect_imports(an, ty, at, &mut imports);
    Some((display_ty(an, ty), imports))
}

/// `name`, or `name2`, `name3`... when the code at `offset` sees `name` or the tokens `toks`
/// use it.
fn unique_name(an: &Analyzer, u: UnitId, offset: usize, toks: Range<usize>, name: &str) -> String {
    let unit = an.unit(u);
    let lx = &unit.file.lexed;
    // A member read after a `.` doesn't clash with a local.
    let member = |k: usize| k.checked_sub(1).is_some_and(|p| matches!(lx.kind(p), Some(Tok::Dot | Tok::SafeDot | Tok::ColonColon)));
    let taken = |candidate: &str| {
        HARD_KEYWORDS.contains(&candidate)
            || unit.ident_tokens(candidate).iter().any(|&k| toks.contains(&k) && !member(k))
            || !an.resolve_name(u, offset, candidate).is_empty()
    };
    if !taken(name) {
        return name.to_owned();
    }
    (2..).map(|n| format!("{name}{n}")).find(|c| !taken(c)).unwrap_or_default()
}

/// Lines `from..to` of `src` indented by `indent` in place of the first line's indentation.
fn reindent(src: &str, from: usize, to: usize, indent: &str) -> String {
    let text = &src[from..to];
    let base = &text[..text.len() - text.trim_start_matches([' ', '\t']).len()];
    text.lines()
        .map(|line| {
            let rest = line.strip_prefix(base).unwrap_or_else(|| line.trim_start());
            if rest.is_empty() { String::new() } else { format!("{indent}{rest}") }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bytes a statement or declaration at `span` takes with its line: the whole lines when
/// nothing else is on them, with a blank line that follows.
fn whole_lines(src: &str, span: Range<usize>) -> Range<usize> {
    let start = line_start(src, span.start);
    let line_end = src[span.end..].find('\n').map_or(src.len(), |i| span.end + i);
    if !src[start..span.start].trim().is_empty() || !src[span.end..line_end].trim().trim_end_matches(';').is_empty() {
        let semi = if src[span.end..].starts_with(';') { 1 } else { 0 };
        return span.start..span.end + semi;
    }
    let mut start = start;
    let mut end = (line_end + 1).min(src.len());
    // The blank line that separated it from the code before or after goes too.
    let blank_before = src[..start].ends_with("\n\n") || start == 0;
    match src[end..].find('\n').map(|i| end + i) {
        Some(next) if blank_before && src[end..next].trim().is_empty() => end = next + 1,
        _ if end == src.len() && src[..start].ends_with("\n\n") => start -= 1,
        _ => {}
    }
    start..end
}

/// The pieces of a parenthesized list (parameters or arguments) between tokens `open` and
/// `close`, as token ranges, split at the commas outside brackets.
fn list_pieces(lx: &Lexed, open: usize, close: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = open + 1;
    let mut k = open + 1;
    while k < close {
        match lx.kind(k) {
            Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen) => {
                k = lx.matching[k].filter(|&c| c > k && c < close).map_or(close, |c| c + 1);
                continue;
            }
            Some(Tok::Lt) => {
                if let Some(gt) = matching_gt(lx, k).filter(|&g| g < close) {
                    k = gt + 1;
                    continue;
                }
            }
            Some(Tok::Comma) => {
                if k > start {
                    out.push(start..k);
                }
                start = k + 1;
            }
            _ => {}
        }
        k += 1;
    }
    if close > start {
        out.push(start..close);
    }
    out
}

/// The bytes to delete to remove piece `i` of a list with its comma.
fn without_piece(lx: &Lexed, pieces: &[Range<usize>], i: usize) -> Range<usize> {
    let byte = |r: &Range<usize>| span_of(lx, r);
    if i + 1 < pieces.len() {
        byte(&pieces[i]).start..byte(&pieces[i + 1]).start
    } else if i > 0 {
        byte(&pieces[i - 1]).end..byte(&pieces[i]).end
    } else {
        byte(&pieces[i])
    }
}

/// Whether a Java file of the project names `name`: its uses aren't resolved, so a refactoring
/// that rewrites them can't see them.
fn named_in_java(an: &Analyzer, name: &str) -> bool {
    an.proj.units.iter().any(|unit| unit.is_java() && !unit.ident_tokens(name).is_empty())
}

/// The declaration at the identifier the selection starts on: declared there or referred to.
fn sym_at(an: &Analyzer, u: UnitId, offset: usize) -> Option<(usize, SymId)> {
    let file = an.file(u);
    let tok = file.lexed.ident_at(offset)?;
    let span = file.lexed.tokens[tok].span();
    let sym = match file.decls_named_at(&span).first() {
        Some(&id) => u.sym(id),
        None => first_sym(an, u, tok)?,
    };
    Some((tok, sym))
}

// ----- extract variable -----

/// "Extract variable": the selected expression becomes a `val` declared before its statement,
/// with its type when it is fully known; a second action replaces every equal expression of the
/// block.
fn extract_variable(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    let Some(sel) = selected_tokens(src, lx, &req.range) else { return };
    let Some(block) = block_at(an, u, lx.tokens[sel.start].start) else { return };
    let Some(stmt) = block.stmts.iter().find(|s| s.start <= sel.start && sel.end <= s.end).cloned() else { return };
    let p = expr_prec(src, lx, sel.clone());
    if stmt == sel || !fits(src, lx, &sel, p) {
        return;
    }
    let pure = is_pure(src, lx, sel.clone());
    if !evaluated_first(src, lx, stmt.start, sel.start, pure) || unresolved(an, u, sel.clone()) {
        return;
    }
    let ty = an.infer(u, sel.start, sel.end);
    if matches!(ty, Some(Ty::Static(_) | Ty::Package(_))) {
        return;
    }
    let expr = &src[span_of(lx, &sel)];

    // The equal expressions of the block, for "replace all".
    let mut all = vec![(stmt.clone(), sel.clone())];
    if pure {
        let len = sel.len();
        for s in &block.stmts {
            let mut k = s.start;
            while k + len <= s.end {
                let candidate = k..k + len;
                let same = candidate != sel
                    && (0..len).all(|i| lx.kind(k + i) == lx.kind(sel.start + i) && lx.text(src, k + i) == lx.text(src, sel.start + i));
                if same
                    && file.scope_at(lx.tokens[k].start) == block.scope
                    && fits(src, lx, &candidate, p)
                    && evaluated_first(src, lx, s.start, k, true)
                    && (0..len).all(|i| {
                        lx.kind(k + i) != Some(Tok::Ident) || targets_at(an, u, k + i).first() == targets_at(an, u, sel.start + i).first()
                    })
                {
                    all.push((s.clone(), candidate));
                    k += len;
                } else {
                    k += 1;
                }
            }
        }
        all.sort_by_key(|(_, c)| c.start);
        // Nothing between them may change what the expression reads.
        let (from, to) = (all[0].1.start, all[all.len() - 1].1.end);
        let changes = (from..to).filter(|k| !all.iter().any(|(_, c)| c.contains(k))).any(|k| {
            let t = lx.tokens[k];
            let text = &src[t.span()];
            is_call(src, lx, k)
                || t.kind == Tok::Op && (matches!(text, "++" | "--") || is_compound_assignment(text))
                || t.kind == Tok::Eq && !file.decls.iter().any(|d| d.init == Some(k + 1) && d.init_kind == InitKind::Value)
        });
        if changes {
            all = vec![(stmt.clone(), sel.clone())];
        }
    }

    let build = |occurrences: &[(Range<usize>, Range<usize>)]| -> Option<Value> {
        let first_stmt = &occurrences[0].0;
        let at = lx.tokens[first_stmt.start].start;
        let ctx = Ctx { unit: u, decl: an.decl_containing(u, at) };
        let typed = ty.as_ref().and_then(|t| type_text(an, t, ctx));
        let name = unique_name(an, u, at, block.open..block.close, &suggest_name(an, u, &sel, ty.as_ref()));
        let mut edits = Edits::default();
        let declared = match &typed {
            Some((t, imports)) => {
                for fq in imports {
                    edits.import(an, u, fq);
                }
                format!("val {name}: {t} = {expr}")
            }
            None => format!("val {name} = {expr}"),
        };
        let line = line_start(src, at);
        if src[line..at].trim().is_empty() {
            edits.replace(an, u, line..line, format!("{}{declared}\n", &src[line..at]));
        } else {
            edits.replace(an, u, at..at, format!("{declared}; "));
        }
        for (_, occurrence) in occurrences {
            edits.replace(an, u, span_of(lx, occurrence), name.clone());
        }
        let title = if occurrences.len() > 1 {
            format!("Extract variable (replace all {} occurrences)", occurrences.len())
        } else {
            "Extract variable".to_owned()
        };
        edits.action(an, req, title, EXTRACT)
    };
    let single = all.iter().find(|(_, c)| *c == sel).cloned().into_iter().collect::<Vec<_>>();
    out.extend(build(&single));
    if all.len() > 1 {
        out.extend(build(&all));
    }
}

/// A name for the value of the expression in tokens `sel`: what it calls or reads last
/// (`getUser()` and `user` give `user`), else its type's name, else `value`.
fn suggest_name(an: &Analyzer, u: UnitId, sel: &Range<usize>, ty: Option<&Ty>) -> String {
    let unit = an.unit(u);
    let (src, lx) = (&unit.text, &unit.file.lexed);
    let last = sel.end - 1;
    // Only a call or a property read is named after what it reads: `a * b` isn't `b`.
    let atom = expr_prec(src, lx, sel.clone()) == ATOM;
    let word = match lx.kind(last) {
        _ if !atom => None,
        Some(Tok::RParen | Tok::RBrace) => {
            lx.matching[last].and_then(|o| o.checked_sub(1)).filter(|&p| p >= sel.start && lx.kind(p) == Some(Tok::Ident))
        }
        Some(Tok::Ident) => Some(last),
        _ => None,
    }
    .map(|k| lx.text(src, k))
    .filter(|w| !HARD_KEYWORDS.contains(w));
    let lower_first = |w: &str| {
        let mut chars = w.chars();
        chars.next().map(|c| c.to_lowercase().chain(chars).collect::<String>()).unwrap_or_default()
    };
    let from_word = word.map(|w| match w.strip_prefix("get") {
        Some(rest) if rest.starts_with(|c: char| c.is_uppercase()) => lower_first(rest),
        _ => lower_first(w),
    });
    let from_type = || match ty {
        Some(Ty::Class { sym, .. }) => Some(lower_first(&an.decl(*sym).name)),
        Some(Ty::Func { .. }) => Some("function".to_owned()),
        _ => None,
    };
    from_word
        .filter(|w| !w.is_empty())
        .or_else(from_type)
        .filter(|w| !HARD_KEYWORDS.contains(&w.as_str()))
        .unwrap_or_else(|| "value".to_owned())
}

// ----- extract function -----

/// "Extract function": the selected statements become a private function next to the one they
/// are in. Its parameters are the locals they read; it returns the one local they declare that
/// the code after them uses.
fn extract_function(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    let Some(sel) = selected_tokens(src, lx, &req.range) else { return };
    let Some(block) = block_at(an, u, lx.tokens[sel.start].start) else { return };
    if block.lambda || !block.stmts.iter().any(|s| s.start == sel.start) || !block.stmts.iter().any(|s| s.end == sel.end) {
        return;
    }
    // The function the block is in, with no lambda in between (whose receiver or `it` the
    // statements may use).
    let mut function = None;
    for sid in file.scope_chain(lx.tokens[block.open].end) {
        match file.scopes[sid].kind {
            ScopeKind::Block => {}
            ScopeKind::Function(d) => {
                function = Some(d);
                break;
            }
            _ => return,
        }
    }
    let Some(fid) = function else { return };
    let fd = &file.decls[fid];
    if fd.kind != Kind::Function || fd.local || fd.name.is_empty() || fd.type_params.iter().any(|p| src[p.span.clone()].contains("reified"))
    {
        return;
    }
    if unresolved(an, u, sel.clone()) {
        return;
    }
    let sel_span = span_of(lx, &sel);
    // Jumps out of the statements can't be extracted.
    for k in sel.clone() {
        if lx.is(src, k, "return") {
            let label = (lx.kind(k + 1) == Some(Tok::LabelRef)).then(|| lx.text(src, k + 1).trim_start_matches('@'));
            let inside = label.is_some_and(|l| {
                (sel.start..k).any(|i| lx.text(src, i).trim_end_matches('@') == l && matches!(lx.kind(i), Some(Tok::Ident | Tok::Label)))
            });
            if !inside {
                return;
            }
        }
        if (lx.is(src, k, "break") || lx.is(src, k, "continue"))
            && !(sel.start..k).any(|i| ["for", "while", "do"].iter().any(|w| lx.is(src, i, w)))
        {
            return;
        }
    }
    // The locals declared before the statements that they read: the parameters.
    let mut inputs: Vec<SymId> = Vec::new();
    for k in plain_names(an, u, sel.clone()) {
        match targets_at(an, u, k).first() {
            Some(Target::It { brace, .. }) if !sel.contains(brace) => return,
            Some(Target::Sym(m)) if m.sym.unit_id() == u => {
                let d = an.decl(m.sym);
                if !fd.span.contains(&d.name_span.start) || sel_span.contains(&d.name_span.start) {
                    continue;
                }
                if d.kind != Kind::Variable && d.kind != Kind::Parameter || d.vararg || d.mutable && is_assigned(src, lx, k) {
                    return;
                }
                if !inputs.contains(&m.sym) {
                    inputs.push(m.sym);
                }
            }
            _ => {}
        }
    }
    // The locals declared in the statements that the rest of the function uses: the result.
    let fn_end = lx.at_or_after(fd.span.end);
    let declared: Vec<usize> =
        file.scopes[block.scope].decls.iter().copied().filter(|&d| sel_span.contains(&file.decls[d].name_span.start)).collect();
    let used_after: Vec<usize> = declared
        .into_iter()
        .filter(|&d| {
            let name = &file.decls[d].name;
            unit.ident_tokens(name).iter().any(|&k| k >= sel.end && k < fn_end && first_sym(an, u, k) == Some(u.sym(d)))
        })
        .collect();
    if used_after.len() > 1 {
        return;
    }
    let body_start = lx.at_or_after(fd.name_span.end);
    let ctx = Ctx { unit: u, decl: Some(fid) };
    let mut imports = BTreeSet::new();
    let mut typed = |s: SymId| -> Option<String> {
        let d = an.decl(s);
        match &d.ty {
            Some(t) => Some(t.display(src)),
            None => {
                let (t, more) = type_text(an, &an.value_ty(s, &Subst::new())?, ctx)?;
                imports.extend(more);
                Some(t)
            }
        }
    };
    let mut params = Vec::new();
    for &s in &inputs {
        let name = &an.decl(s).name;
        // A smart cast before the statements may make the local more than its type says.
        if may_be_smart_cast(src, lx, name, body_start..sel.start) {
            return;
        }
        let Some(t) = typed(s) else { return };
        params.push(format!("{}: {t}", escape_name(name)));
    }
    let result = match used_after.first() {
        Some(&d) => {
            let rd = &file.decls[d];
            if rd.kind != Kind::Variable || rd.init_kind != InitKind::Value {
                return;
            }
            let Some(t) = typed(u.sym(d)) else { return };
            Some((rd, t))
        }
        None => None,
    };
    let indent = indent_at(src, fd.span.start).to_owned();
    let inner = format!("{indent}    ");
    let mentions_type_param = fd.type_params.iter().any(|p| {
        sel.clone().any(|k| lx.kind(k) == Some(Tok::Ident) && lx.text(src, k) == p.name)
            || params.iter().any(|t| t.contains(p.name.as_str()))
    });
    let type_params = if mentions_type_param { type_params_text(src, fd) } else { String::new() };
    let receiver = fd.receiver.as_ref().map(|r| format!("{}.", r.display(src))).unwrap_or_default();
    let modifiers = if fd.has("suspend") { "private suspend " } else { "private " };
    let name = unique_name(an, u, sel_span.start, 0..lx.tokens.len(), "extracted");
    let mut body = reindent(src, line_start(src, sel_span.start), sel_span.end, &inner);
    let ret = match &result {
        Some((rd, t)) => {
            body.push_str(&format!("\n{inner}return {}", escape_name(&rd.name)));
            format!(": {t}")
        }
        None => String::new(),
    };
    let function = format!("{indent}{modifiers}fun {type_params}{receiver}{name}({}){ret} {{\n{body}\n{indent}}}", params.join(", "));
    let args: Vec<String> = inputs.iter().map(|&s| escape_name(&an.decl(s).name)).collect();
    let call = format!("{name}({})", args.join(", "));
    let call = match &result {
        Some((rd, _)) => format!("{} {} = {call}", if rd.mutable { "var" } else { "val" }, escape_name(&rd.name)),
        None => call,
    };
    let mut edits = Edits::default();
    edits.replace(an, u, sel_span, call);
    edits.replace(an, u, fd.span.end..fd.span.end, format!("\n\n{function}"));
    for fq in &imports {
        edits.import(an, u, fq);
    }
    out.extend(edits.action(an, req, "Extract function".to_owned(), EXTRACT));
}

/// Whether a check before tokens `range` may smart cast the local `name`: `name is T`,
/// `name != null`, `name!!`, `name as T`, `name ?: return`.
fn may_be_smart_cast(src: &str, lx: &Lexed, name: &str, range: Range<usize>) -> bool {
    range.into_iter().any(|k| {
        if !(lx.kind(k) == Some(Tok::Ident) && lx.text(src, k) == name) {
            return false;
        }
        let is_null = |i: usize| lx.is(src, i, "null");
        let op = |i: usize, ops: &[&str]| lx.kind(i) == Some(Tok::Op) && ops.contains(&&src[lx.tokens[i].span()]);
        lx.is(src, k + 1, "is")
            || lx.is(src, k + 1, "as")
            || op(k + 1, &["!", "?:"])
            || lx.kind(k + 1) == Some(Tok::BangBang)
            || op(k + 1, &["!=", "=="]) && is_null(k + 2)
            || k >= 2 && op(k - 1, &["!=", "=="]) && is_null(k - 2)
    })
}

// ----- inline variable -----

/// "Inline variable": every use of a local `val` gets its initializer, and the declaration goes.
/// An initializer that reads only `val`s and literals goes anywhere; any other only to a single
/// use that the next statement evaluates first.
fn inline_variable(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    let Some((_, sym)) = sym_at(an, u, req.range.start) else { return };
    let d = an.decl(sym);
    if sym.unit_id() != u || d.kind != Kind::Variable || !d.local || d.mutable || d.init_kind != InitKind::Value || d.has("lateinit") {
        return;
    }
    let Some(init) = d.init else { return };
    let Some(block) = block_at(an, u, d.name_span.start) else { return };
    let name_tok = lx.at_or_after(d.name_span.start);
    let Some(si) = block.stmts.iter().position(|s| s.start <= name_tok && name_tok < s.end) else { return };
    let stmt = block.stmts[si].clone();
    if !lx.is(src, stmt.start, "val") || expr_end(src, lx, init, block.close) != stmt.end {
        return;
    }
    let init_toks = init..stmt.end;
    if unresolved(an, u, init_toks.clone()) {
        return;
    }
    // A declared type may make the initializer mean more than it says (`val x: Long = 1`).
    if let Some(declared) = &d.ty {
        let inferred = an.infer(u, init, stmt.end).filter(fully_known).map(|t| display_ty(an, &t));
        if inferred.as_deref().map(super::syntax::normalize) != Some(super::syntax::normalize(&declared.display(src))) {
            return;
        }
    }
    let Some(uses) = search(an, &[sym], false, &Control::default()) else { return };
    let uses: Vec<usize> = uses.into_iter().filter(|(uu, _)| *uu == u).map(|(_, r)| lx.at_or_after(r.start)).collect();
    // What the initializer reads, and whether it reads the same everywhere.
    let names = plain_names(an, u, init_toks.clone());
    let stable = is_pure(src, lx, init_toks.clone())
        && (init..stmt.end).all(|k| !matches!(lx.kind(k), Some(Tok::Dot | Tok::SafeDot | Tok::ColonColon)))
        && names.iter().all(|&k| match targets_at(an, u, k).first() {
            Some(Target::Sym(m)) => {
                let t = an.decl(m.sym);
                matches!(t.kind, Kind::Parameter | Kind::EnumEntry)
                    || t.kind == Kind::Variable && !t.mutable
                    || t.kind == Kind::Property && t.has("const")
            }
            Some(Target::It { .. }) => true,
            _ => false,
        });
    if !stable {
        let [only] = uses.as_slice() else { return };
        let Some(next) = block.stmts.get(si + 1) else { return };
        if !next.contains(only)
            || file.scope_at(lx.tokens[*only].start) != block.scope
            || !evaluated_first(src, lx, next.start, *only, false)
        {
            return;
        }
    }
    for &use_tok in &uses {
        let offset = lx.tokens[use_tok].start;
        let same = names.iter().all(|&k| {
            let original = targets_at(an, u, k);
            let found = an.resolve_name(u, offset, lx.text(src, k));
            match (original.first(), found.first()) {
                (Some(Target::Sym(a)), Some(Found::Sym(b))) => a.sym == b.sym,
                (Some(Target::It { brace: a, .. }), Some(Found::It { brace: b, .. })) => a == b,
                _ => false,
            }
        });
        if !same || is_assigned(src, lx, use_tok) {
            return;
        }
    }
    let expr = &src[span_of(lx, &init_toks)];
    let p = expr_prec(src, lx, init_toks);
    let mut edits = Edits::default();
    for &use_tok in &uses {
        let t = lx.tokens[use_tok];
        if t.start > 0 && src.as_bytes()[t.start - 1] == b'$' {
            edits.replace(an, u, t.start - 1..t.end, format!("${{{expr}}}"));
        } else if fits(src, lx, &(use_tok..use_tok + 1), p) {
            edits.replace(an, u, t.span(), expr);
        } else {
            edits.replace(an, u, t.span(), format!("({expr})"));
        }
    }
    edits.replace(an, u, whole_lines(src, span_of(lx, &stmt)), "");
    out.extend(edits.action(an, req, format!("Inline variable `{}`", d.name), INLINE));
}

// ----- inline function -----

/// The expression a function returns, as tokens, and whether it is a statement of a `Unit`
/// function's body rather than a value.
fn function_body(an: &Analyzer, f: SymId) -> Option<(Range<usize>, bool)> {
    let unit = an.unit(f.unit_id());
    let (src, lx) = (&unit.text, &unit.file.lexed);
    let d = an.decl(f);
    let end = lx.at_or_after(d.span.end);
    if let Some(init) = d.init {
        return (expr_end(src, lx, init, end) == end && init < end).then_some((init..end, false));
    }
    let open = body_brace(d, lx)?;
    let block = block_at(an, f.unit_id(), lx.tokens[open].end).filter(|b| b.open == open)?;
    let [stmt] = block.stmts.as_slice() else { return None };
    if lx.is(src, stmt.start, "return") {
        return (stmt.start + 1 < stmt.end).then(|| (stmt.start + 1..stmt.end, false));
    }
    let unit_type = d.ty.as_ref().is_none_or(|t| t.display(src) == "Unit");
    (unit_type && expr_prec(src, lx, stmt.clone()) > 0).then(|| (stmt.clone(), true))
}

/// "Inline function": every call of a top-level function whose body is one expression gets the
/// expression, with the arguments in place of the parameters, and the declaration goes.
fn inline_function(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let Some((_, f)) = sym_at(an, u, req.range.start) else { return };
    let d = an.decl(f);
    let du = f.unit_id();
    let modifiers = ["override", "open", "abstract", "expect", "actual", "operator", "infix", "external", "tailrec"];
    if f.lib
        || d.kind != Kind::Function
        || d.parent.is_some()
        || d.receiver.is_some()
        || !d.type_params.is_empty()
        || modifiers.iter().any(|m| d.has(m))
        || named_in_java(an, &d.name)
    {
        return;
    }
    let dunit = an.unit(du);
    let (dsrc, dfile) = (&dunit.text, &dunit.file);
    let dlx = &dfile.lexed;
    if d.params.iter().any(|&p| dfile.decls[p].vararg) {
        return;
    }
    let Some((body, statement)) = function_body(an, f) else { return };
    if unresolved(an, du, body.clone()) {
        return;
    }
    let params: Vec<SymId> = d.params.iter().map(|&p| du.sym(p)).collect();
    let names = plain_names(an, du, body.clone());
    let mut param_uses: Vec<Vec<usize>> = vec![Vec::new(); params.len()];
    let mut others = Vec::new();
    for &k in &names {
        let target = first_sym(an, du, k);
        if target == Some(f) {
            return;
        }
        match target.and_then(|s| params.iter().position(|&p| p == s)) {
            Some(i) => param_uses[i].push(k),
            None => others.push(k),
        }
    }
    let Some(refs) = search(an, &[f], false, &Control::default()) else { return };
    if refs.is_empty() {
        return;
    }
    let body_span = span_of(dlx, &body);
    let body_prec = expr_prec(dsrc, dlx, body.clone());
    let mut edits = Edits::default();
    // The imports of the function, which go with it, and the imports its body needs at the calls.
    let mut dropped: BTreeMap<UnitId, Vec<Range<usize>>> = BTreeMap::new();
    let mut needed: BTreeMap<UnitId, BTreeSet<String>> = BTreeMap::new();
    for (ru, range) in &refs {
        let runit = an.unit(*ru);
        let (rsrc, rlx) = (&runit.text, &runit.file.lexed);
        let k = rlx.at_or_after(range.start);
        if let Some(import) = runit.file.imports.iter().find(|i| i.tokens.contains(&k)) {
            let written = rsrc[import.span.clone()].trim_end().len();
            dropped.entry(*ru).or_default().push(import.span.start..import.span.start + written);
            continue;
        }
        let prev = k.checked_sub(1).and_then(|p| rlx.kind(p));
        if *ru == du && body_span.contains(&range.start) || matches!(prev, Some(Tok::Dot | Tok::SafeDot | Tok::ColonColon)) {
            return;
        }
        if rlx.kind(k + 1) != Some(Tok::LParen) {
            return;
        }
        let Some(close) = rlx.matching[k + 1].filter(|&c| c > k + 1) else { return };
        if rlx.kind(close + 1) == Some(Tok::LBrace) && !rlx.tokens[close + 1].nl {
            return;
        }
        let call = k..close + 1;
        if statement {
            let whole = block_at(an, *ru, rlx.tokens[k].start).is_some_and(|b| b.stmts.contains(&call));
            if !whole {
                return;
            }
        }
        // The arguments, by parameter.
        let mut args: Vec<Option<Range<usize>>> = vec![None; params.len()];
        for (i, piece) in list_pieces(rlx, k + 1, close).into_iter().enumerate() {
            if rlx.kind(piece.start) == Some(Tok::Op) && &rsrc[rlx.tokens[piece.start].span()] == "*" {
                return;
            }
            let named = (rlx.kind(piece.start + 1) == Some(Tok::Eq) && piece.len() > 2).then(|| rlx.text(rsrc, piece.start));
            let index = match named {
                Some(n) => d.params.iter().position(|&p| dfile.decls[p].name == n),
                None => Some(i),
            };
            let Some(index) = index.filter(|&x| x < params.len()) else { return };
            args[index] = Some(if named.is_some() { piece.start + 2..piece.end } else { piece });
        }
        let Some(args) = args.into_iter().collect::<Option<Vec<_>>>() else { return };
        // Each argument is evaluated as often as before, and in the same order.
        let mut last_use = None;
        for (i, arg) in args.iter().enumerate() {
            if is_pure(rsrc, rlx, arg.clone()) {
                continue;
            }
            let [use_tok] = param_uses[i].as_slice() else { return };
            if !evaluated_first(dsrc, dlx, body.start, *use_tok, false) || last_use.is_some_and(|l| l > *use_tok) {
                return;
            }
            last_use = Some(*use_tok);
        }
        // The other names of the body mean the same at the call.
        let at = rlx.tokens[k].start;
        for &o in &others {
            let name = dlx.text(dsrc, o);
            let Some(target) = first_sym(an, du, o) else {
                // `it` of a lambda in the body.
                if matches!(targets_at(an, du, o).first(), Some(Target::It { brace, .. }) if body.contains(brace)) {
                    continue;
                }
                return;
            };
            if target.unit_id() == du && body_span.contains(&an.decl(target).name_span.start) {
                continue;
            }
            match an.resolve_name(*ru, at, name).first() {
                Some(Found::Sym(m)) if m.sym == target => {}
                None => {
                    let td = an.decl(target);
                    if td.local || td.parent.is_some() || td.is_private() || an.package_of(target).is_empty() {
                        return;
                    }
                    needed.entry(*ru).or_default().insert(an.fq(target));
                }
                _ => return,
            }
        }
        // The body with the arguments in place.
        let mut text = String::new();
        let mut pos = body_span.start;
        let mut uses: Vec<(usize, usize)> = param_uses.iter().enumerate().flat_map(|(i, list)| list.iter().map(move |&t| (t, i))).collect();
        uses.sort();
        for (t, i) in uses {
            let span = dlx.tokens[t].span();
            let arg = &args[i];
            let arg_text = &rsrc[span_of(rlx, arg)];
            let template = span.start > 0 && dsrc.as_bytes()[span.start - 1] == b'$';
            let start = if template { span.start - 1 } else { span.start };
            text.push_str(&dsrc[pos..start]);
            if template {
                text.push_str(&format!("${{{arg_text}}}"));
            } else if fits(dsrc, dlx, &(t..t + 1), expr_prec(rsrc, rlx, arg.clone())) {
                text.push_str(arg_text);
            } else {
                text.push_str(&format!("({arg_text})"));
            }
            pos = span.end;
        }
        text.push_str(&dsrc[pos..body_span.end]);
        if !statement && !fits(rsrc, rlx, &call, body_prec) {
            text = format!("({text})");
        }
        edits.replace(an, *ru, span_of(rlx, &call), text);
    }
    // A dropped import makes room for the needed ones, so that no two edits touch the same place.
    for (ru, fqs) in &needed {
        let file = an.file(*ru);
        let fqs: Vec<&String> = fqs.iter().filter(|fq| !file.imports.iter().any(|i| i.alias.is_none() && i.fq() == **fq)).collect();
        match dropped.get_mut(ru).and_then(|spans| spans.pop()) {
            Some(span) => {
                let lines: Vec<String> = fqs.iter().map(|fq| format!("import {}", escape_path(fq))).collect();
                if lines.is_empty() {
                    dropped.entry(*ru).or_default().push(span);
                } else {
                    edits.replace(an, *ru, span, lines.join("\n"));
                }
            }
            None => fqs.into_iter().for_each(|fq| edits.import(an, *ru, fq)),
        }
    }
    for (ru, spans) in &dropped {
        let rsrc = &an.unit(*ru).text;
        for span in spans {
            let end = rsrc[span.end..].find('\n').map_or(rsrc.len(), |i| span.end + i + 1);
            edits.replace(an, *ru, line_start(rsrc, span.start)..end, "");
        }
    }
    let start = d.doc.as_ref().map_or(d.span.start, |doc| doc.start.min(d.span.start));
    edits.replace(an, du, whole_lines(dsrc, start..d.span.end), "");
    out.extend(edits.action(an, req, format!("Inline function `{}`", d.name), INLINE));
}

// ----- change signature -----

/// One call of a function: its argument list and whether a lambda follows it.
struct Call {
    unit: UnitId,
    open: usize,
    close: usize,
    trailing_lambda: bool,
}

/// The calls of `group`, or `None` when a use of it isn't a call with its arguments in
/// parentheses (a callable reference, an infix call).
fn calls_of(an: &Analyzer, group: &[SymId]) -> Option<Vec<Call>> {
    let refs = search(an, group, false, &Control::default())?;
    let mut out = Vec::new();
    for (u, range) in refs {
        let unit = an.unit(u);
        let lx = &unit.file.lexed;
        let k = lx.at_or_after(range.start);
        if k.checked_sub(1).and_then(|p| lx.kind(p)) == Some(Tok::ColonColon) {
            return None;
        }
        let mut open = k + 1;
        if lx.kind(open) == Some(Tok::Lt) {
            open = matching_gt(lx, open)? + 1;
        }
        if lx.kind(open) != Some(Tok::LParen) {
            return None;
        }
        let close = lx.matching[open].filter(|&c| c > open)?;
        let trailing_lambda = lx.kind(close + 1) == Some(Tok::LBrace) && !lx.tokens[close + 1].nl;
        out.push(Call { unit: u, open, close, trailing_lambda });
    }
    Some(out)
}

/// The `(` of a function's parameter list.
fn params_open(lx: &Lexed, d: &Decl) -> Option<usize> {
    let k = lx.at_or_after(d.name_span.end);
    (lx.kind(k) == Some(Tok::LParen)).then_some(k)
}

/// An argument, by its parameter's name when named.
fn arg_name<'s>(src: &'s str, lx: &Lexed, piece: &Range<usize>) -> Option<&'s str> {
    (piece.len() > 2 && lx.kind(piece.start) == Some(Tok::Ident) && lx.kind(piece.start + 1) == Some(Tok::Eq))
        .then(|| lx.text(src, piece.start))
}

/// "Move parameter left/right" and "Remove parameter" on a parameter of a function: the
/// declarations (with overrides and `expect`/`actual` counterparts) and every call change
/// together.
fn change_signature(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let file = an.file(u);
    let lx = &file.lexed;
    let Some(tok) = lx.ident_at(req.range.start) else { return };
    let Some(&pid) = file.decls_named_at(&lx.tokens[tok].span()).first() else { return };
    if file.decls[pid].kind != Kind::Parameter {
        return;
    }
    let Some(fid) = file.decls.iter().position(|d| d.kind == Kind::Function && d.params.contains(&pid)) else { return };
    let fd = &file.decls[fid];
    let index = fd.params.iter().position(|&p| p == pid).unwrap_or_default();
    let n = fd.params.len();
    if fd.params.iter().any(|&p| file.decls[p].vararg) || fd.name.is_empty() || named_in_java(an, &fd.name) {
        return;
    }
    let group = rename_group(an, u.sym(fid));
    if group.iter().any(|s| s.lib) {
        return;
    }
    // Each declaration's parameter list.
    let mut decls = Vec::new();
    for &g in &group {
        let gl = &an.file(g.unit_id()).lexed;
        let Some(open) = params_open(gl, an.decl(g)) else { return };
        let Some(close) = gl.matching[open].filter(|&c| c > open) else { return };
        let pieces = list_pieces(gl, open, close);
        if pieces.len() != n {
            return;
        }
        decls.push((g, pieces));
    }
    let Some(calls) = calls_of(an, &group) else { return };
    let name = |i: usize| file.decls[fd.params[i]].name.clone();
    let pname = name(index);

    // Swaps parameters `j` and `j + 1`.
    let swap = |j: usize| -> Option<Edits> {
        let mut edits = Edits::default();
        for (g, pieces) in &decls {
            let gu = g.unit_id();
            let (gsrc, gl) = (&an.unit(gu).text, &an.file(gu).lexed);
            let (a, b) = (span_of(gl, &pieces[j]), span_of(gl, &pieces[j + 1]));
            edits.replace(an, gu, a.clone(), &gsrc[b.clone()]);
            edits.replace(an, gu, b, &gsrc[a]);
        }
        for call in &calls {
            let (csrc, cl) = (&an.unit(call.unit).text, &an.file(call.unit).lexed);
            let args = list_pieces(cl, call.open, call.close);
            let positional = args.iter().take_while(|a| arg_name(csrc, cl, a).is_none()).count();
            if args[positional..].iter().any(|a| arg_name(csrc, cl, a).is_none()) || call.trailing_lambda && j + 1 >= n - 1 {
                return None;
            }
            if j + 1 < positional {
                let (a, b) = (&args[j], &args[j + 1]);
                if !is_pure(csrc, cl, a.clone()) && !is_pure(csrc, cl, b.clone()) {
                    return None;
                }
                let (sa, sb) = (span_of(cl, a), span_of(cl, b));
                edits.replace(an, call.unit, sa.clone(), &csrc[sb.clone()]);
                edits.replace(an, call.unit, sb, &csrc[sa]);
            } else if j + 1 == positional {
                // The argument at `j` would go to the other parameter: name it.
                let at = span_of(cl, &args[j]).start;
                edits.replace(an, call.unit, at..at, format!("{} = ", escape_name(&name(j))));
            }
        }
        Some(edits)
    };
    if index > 0
        && let Some(edits) = swap(index - 1)
    {
        out.extend(edits.action(an, req, format!("Move parameter `{pname}` left"), REWRITE));
    }
    if index + 1 < n
        && let Some(edits) = swap(index)
    {
        out.extend(edits.action(an, req, format!("Move parameter `{pname}` right"), REWRITE));
    }

    // Removes the parameter, when no body uses it and no argument for it has side effects.
    let remove = || -> Option<Edits> {
        let mut edits = Edits::default();
        for (g, pieces) in &decls {
            let gu = g.unit_id();
            let gd = an.decl(*g);
            let param = gu.sym(gd.params[index]);
            let uses = search(an, &[param], false, &Control::default())?;
            if uses.iter().any(|(uu, r)| *uu == gu && gd.span.contains(&r.start)) {
                return None;
            }
            edits.replace(an, gu, without_piece(&an.file(gu).lexed, pieces, index), "");
        }
        for call in &calls {
            let (csrc, cl) = (&an.unit(call.unit).text, &an.file(call.unit).lexed);
            let args = list_pieces(cl, call.open, call.close);
            if call.trailing_lambda && index == n - 1 {
                return None;
            }
            let positional = args.iter().take_while(|a| arg_name(csrc, cl, a).is_none()).count();
            let found =
                if index < positional { Some(index) } else { args.iter().position(|a| arg_name(csrc, cl, a) == Some(pname.as_str())) };
            if let Some(i) = found {
                if !is_pure(csrc, cl, args[i].clone()) {
                    return None;
                }
                edits.replace(an, call.unit, without_piece(cl, &args, i), "");
            }
        }
        Some(edits)
    };
    if let Some(edits) = remove() {
        out.extend(edits.action(an, req, format!("Remove parameter `{pname}`"), REWRITE));
    }
}

// ----- move -----

/// How a use of a moved declaration changes.
enum Use {
    /// An import of it, or of something inside it: the path gets the new package.
    Import(usize),
    /// `old.package.Name`: the qualifier, as bytes, becomes the new package.
    Qualified(Range<usize>),
    /// The bare name, which an import of the new package's declaration keeps working.
    Plain,
}

/// "Move to package": a top-level declaration moves to another package of its source set, into
/// a file named after it (or after its file); the imports and qualified names of its users
/// change, and the code moved gets the imports it needs. A class also moves to a file of its
/// own in the same package.
fn move_declaration(an: &Analyzer, req: &Request, out: &mut Vec<Value>) {
    let u = req.unit;
    let unit = an.unit(u);
    let (src, file) = (&unit.text, &unit.file);
    let lx = &file.lexed;
    let Origin::Source(set) = &unit.origin else { return };
    let Some(&id) = file.top.iter().find(|&&t| {
        let d = &file.decls[t];
        d.span.start <= req.range.start && req.range.start <= d.name_span.end && !d.generated
    }) else {
        return;
    };
    let d = &file.decls[id];
    let sym = u.sym(id);
    let movable = d.kind.is_class() || matches!(d.kind, Kind::TypeAlias | Kind::Function | Kind::Property);
    if !movable
        || d.name.is_empty()
        || ["private", "expect", "actual"].iter().any(|m| d.has(m))
        || d.kind == Kind::Function && d.name == "main"
        || an.by_fq(&an.fq(sym)).len() > 1
        || named_in_java(an, &d.name)
    {
        return;
    }
    let old = file.package.clone();
    let decl_toks = lx.at_or_after(d.span.start)..lx.at_or_after(d.span.end);
    if unresolved(an, u, decl_toks.clone()) {
        return;
    }
    let start = d.doc.as_ref().map_or(d.span.start, |doc| doc.start.min(d.span.start));
    let moved = &src[start..d.span.end];
    // The declarations of the old package the moved code uses without importing them.
    let mut from_old = BTreeSet::new();
    for k in plain_names(an, u, decl_toks.clone()) {
        let Some(s) = first_sym(an, u, k) else { continue };
        let sd = an.decl(s);
        if s.lib || (s.unit_id() == u && d.span.contains(&sd.name_span.start)) || sd.parent.is_some() || sd.local {
            continue;
        }
        if an.package_of(s) == old {
            if sd.is_private() {
                return;
            }
            from_old.insert(escape_path(&an.fq(s)));
        }
    }
    // The old file's imports the moved code uses.
    let words: HashSet<&str> = decl_toks.clone().filter(|&k| lx.kind(k) == Some(Tok::Ident)).map(|k| lx.text(src, k)).collect();
    let copied: Vec<String> = file
        .imports
        .iter()
        .filter(|i| i.star || i.name().is_some_and(|n| words.contains(n)))
        .map(|i| src[i.span.clone()].trim().trim_start_matches("import").trim().trim_end_matches(';').trim_end().to_owned())
        .collect();
    // The uses, and how each changes.
    let Some(refs) = search(an, &[sym], false, &Control::default()) else { return };
    let mut uses: Vec<(UnitId, Range<usize>, Use)> = Vec::new();
    for (ru, range) in refs {
        if ru == u && d.span.contains(&range.start) {
            continue;
        }
        let rfile = an.file(ru);
        let rlx = &rfile.lexed;
        let k = rlx.at_or_after(range.start);
        if let Some(i) = rfile.imports.iter().position(|i| i.tokens.contains(&k)) {
            let segments = &rfile.imports[i].path;
            let depth = if old.is_empty() { 0 } else { old.split('.').count() };
            if segments.len() <= depth || segments[..depth].join(".") != old || segments[depth] != d.name {
                return;
            }
            uses.push((ru, range, Use::Import(i)));
        } else if matches!(k.checked_sub(1).and_then(|p| rlx.kind(p)), Some(Tok::Dot | Tok::SafeDot)) {
            let rsrc = &an.unit(ru).text;
            let mut q = k - 1;
            let mut segments = Vec::new();
            while q > 0 && rlx.kind(q) == Some(Tok::Dot) && rlx.kind(q - 1) == Some(Tok::Ident) {
                segments.push(rlx.text(rsrc, q - 1));
                q -= 2;
                if segments.len() == old.split('.').count() {
                    break;
                }
            }
            segments.reverse();
            if old.is_empty() || segments.join(".") != old {
                return;
            }
            uses.push((ru, range, Use::Qualified(rlx.tokens[q + 1].start..rlx.tokens[k - 1].start)));
        } else {
            uses.push((ru, range, Use::Plain));
        }
    }

    let header = |package: &str, imports: &[String]| {
        let mut text = String::new();
        if !package.is_empty() {
            text.push_str(&format!("package {}\n\n", escape_path(package)));
        }
        if !imports.is_empty() {
            for import in imports {
                text.push_str(&format!("import {import}\n"));
            }
            text.push('\n');
        }
        text
    };
    let removal = whole_lines(src, start..d.span.end);
    let file_name = |package_dir: &std::path::Path| -> std::path::PathBuf {
        let named = d.kind.is_class() || d.kind == Kind::TypeAlias;
        let name = if named {
            format!("{}.kt", d.name)
        } else {
            unit.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        };
        package_dir.join(name)
    };

    // Into the file at `path` in `package`: the declaration, with what it needs.
    let into = |edits: &mut Edits, package: &str, path: &std::path::Path| -> Option<()> {
        let mut imports: Vec<String> = copied.clone();
        if package != old {
            imports.extend(from_old.iter().cloned());
        }
        imports.sort_by_key(|i| (i.starts_with("java.") || i.starts_with("javax.") || i.starts_with("kotlin."), i.clone()));
        imports.dedup();
        match an.proj.units.iter().position(|t| t.path == path) {
            Some(ti) => {
                let tu = UnitId { lib: false, unit: ti as u32 };
                let target = an.unit(tu);
                if target.file.package != package {
                    return None;
                }
                for import in &imports {
                    if !target.file.imports.iter().any(|i| target.text[i.span.clone()].contains(import.as_str())) {
                        if import.contains(" as ") || import.ends_with(".*") {
                            let at = target.file.import_offset;
                            edits.replace(an, tu, at..at, format!("\nimport {import}"));
                        } else {
                            edits.import(an, tu, import);
                        }
                    }
                }
                let text = &target.text;
                let separator = if text.ends_with("\n\n") || text.is_empty() {
                    ""
                } else if text.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                edits.replace(an, tu, text.len()..text.len(), format!("{separator}{moved}\n"));
            }
            None if path.exists() => return None,
            None => {
                let content = format!("{}{moved}\n", header(package, &imports));
                edits.created.push(FileChange {
                    uri: text::path_to_uri(path),
                    edits: vec![json!({ "range": text::range("", 0..0), "newText": content })],
                    create: true,
                });
            }
        }
        Some(())
    };

    // To another package of the same source set.
    let mut packages: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for other in an.proj.units.iter() {
        if matches!(&other.origin, Origin::Source(s) if s == set)
            && !other.is_java()
            && !other.file.package.is_empty()
            && other.file.package != old
            && let Some(dir) = other.path.parent()
        {
            packages.entry(other.file.package.clone()).or_insert_with(|| dir.to_owned());
        }
    }
    for (package, dir) in packages.iter().take(12) {
        let new_fq = format!("{package}.{}", d.name);
        if !an.by_fq(&new_fq).is_empty() {
            continue;
        }
        let mut edits = Edits::default();
        edits.replace(an, u, removal.clone(), "");
        if into(&mut edits, package, &file_name(dir)).is_none() {
            continue;
        }
        let imported: HashSet<UnitId> = uses.iter().filter(|(_, _, w)| matches!(w, Use::Import(_))).map(|(ru, ..)| *ru).collect();
        for (ru, _, change) in &uses {
            let rfile = an.file(*ru);
            match change {
                Use::Import(i) => {
                    let import = &rfile.imports[*i];
                    let depth = if old.is_empty() { 0 } else { old.split('.').count() };
                    let mut path = escape_path(package);
                    for segment in &import.path[depth..] {
                        path.push('.');
                        path.push_str(&escape_name(segment));
                    }
                    if import.star {
                        path.push_str(".*");
                    }
                    if let Some(alias) = &import.alias {
                        path.push_str(&format!(" as {}", escape_name(alias)));
                    }
                    let rsrc = &an.unit(*ru).text;
                    let span = import.span.clone();
                    let written = rsrc[span.clone()].trim_end();
                    let end = span.start + written.len();
                    edits.replace(an, *ru, span.start..end, format!("import {path}"));
                }
                Use::Qualified(q) => edits.replace(an, *ru, q.clone(), escape_path(package)),
                Use::Plain => {
                    if rfile.package != *package && !imported.contains(ru) {
                        edits.import(an, *ru, &new_fq);
                    }
                }
            }
        }
        out.extend(edits.action(an, req, format!("Move `{}` to package `{package}`", d.name), MOVE));
    }

    // To a file of its own.
    let stem = unit.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if (d.kind.is_class() || d.kind == Kind::TypeAlias)
        && file.top.iter().filter(|&&t| !file.decls[t].generated).count() > 1
        && stem != d.name
    {
        let path = unit.path.with_file_name(format!("{}.kt", d.name));
        if !an.proj.units.iter().any(|t| t.path == path) && !path.exists() {
            let mut edits = Edits::default();
            edits.replace(an, u, removal.clone(), "");
            if into(&mut edits, &old, &path).is_some() {
                out.extend(edits.action(an, req, format!("Move `{}` to `{}.kt`", d.name, d.name), MOVE));
            }
        }
    }
}
