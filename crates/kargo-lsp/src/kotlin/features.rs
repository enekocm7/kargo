//! The language features over a resolved snapshot: completion, hover, go to definition,
//! references and highlights, rename, and document and workspace symbols.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::index::{DEFAULT_IMPORTS, SymId, UnitId, is_internal_package};
use super::java::javadoc_markdown;
use super::lexer::{self, HARD_KEYWORDS, Tok};
use super::resolve::{Analyzer, Arg, Found, Member, Subst, Ty, chain_start, strip_nullable};
use super::syntax::{Decl, DeclId, File, Kind, ScopeKind, normalize};
use crate::text;

/// What an identifier refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Sym(Member),
    /// The implicit parameter `it` of the lambda whose `{` is token `brace`.
    It {
        unit: UnitId,
        brace: usize,
    },
    Package(String),
}

impl Target {
    pub fn sym(&self) -> Option<SymId> {
        match self {
            Target::Sym(m) => Some(m.sym),
            _ => None,
        }
    }
}

/// What the identifier token `tok` of `u` refers to, best candidate first.
pub fn targets_at(an: &Analyzer, u: UnitId, tok: usize) -> Vec<Target> {
    let file = an.file(u);
    let lx = &file.lexed;
    let src = &an.unit(u).text;
    let Some(t) = lx.tokens.get(tok).copied() else { return Vec::new() };
    if t.kind != Tok::Ident {
        return Vec::new();
    }
    let name = lx.text(src, tok);
    let plain = |s: SymId| Target::Sym(Member { sym: s, subst: Subst::new() });
    // A declaration's own name.
    if let Some(d) = file.decls.iter().position(|d| d.name_span == t.span() && !d.name.is_empty() && d.kind != Kind::Constructor) {
        return vec![plain(u.sym(d))];
    }
    if let Some(pos) = file.package_tokens.iter().position(|&p| p == tok) {
        let fq: Vec<&str> = file.package_tokens[..=pos].iter().map(|&p| lx.text(src, p)).collect();
        return vec![Target::Package(fq.join("."))];
    }
    for import in &file.imports {
        if let Some(pos) = import.tokens.iter().position(|&p| p == tok) {
            let fq = import.path[..=pos].join(".");
            let syms = an.import_targets(fq.clone());
            if !syms.is_empty() {
                return syms.into_iter().map(plain).collect();
            }
            return if an.has_package(&fq) { vec![Target::Package(fq)] } else { Vec::new() };
        }
    }
    match name {
        "this" | "super" if !src[t.span()].starts_with('`') => {
            let ty = if name == "this" {
                let label = (lx.kind(tok + 1) == Some(Tok::LabelRef)).then(|| lx.text(src, tok + 1));
                an.this_ty(u, t.start, label)
            } else {
                an.infer(u, tok, tok + 1)
            };
            return match ty {
                Some(Ty::Class { sym, .. }) => vec![plain(sym)],
                _ => Vec::new(),
            };
        }
        _ if HARD_KEYWORDS.contains(&&src[t.span()]) => return Vec::new(),
        _ => {}
    }
    let call_args = |at: usize| -> Option<Vec<Arg>> {
        let mut j = at;
        if lx.kind(j) == Some(Tok::Lt)
            && let Some(gt) = super::resolve::matching_gt(lx, j)
        {
            j = gt + 1;
        }
        (matches!(lx.kind(j), Some(Tok::LParen | Tok::LBrace)) && !lx.tokens[j].nl).then(|| an.call_args(u, j, lx.tokens.len()).0)
    };
    let prev = tok.checked_sub(1).and_then(|p| lx.kind(p));
    // `receiver.name`
    if matches!(prev, Some(Tok::Dot | Tok::SafeDot)) {
        let start = chain_start(src, lx, tok - 1);
        let Some(receiver) = an.infer(u, start, tok - 1) else { return Vec::new() };
        let receiver = if prev == Some(Tok::SafeDot) { strip_nullable(receiver) } else { receiver };
        if let Ty::Package(p) = &receiver {
            let fq = format!("{p}.{name}");
            let syms = an.by_fq(&fq);
            if !syms.is_empty() {
                return syms.into_iter().map(plain).collect();
            }
            return if an.has_package(&fq) { vec![Target::Package(fq)] } else { Vec::new() };
        }
        let members = an.members_named(&receiver, name, u);
        return rank(an, u, members, call_args(tok + 1)).into_iter().map(Target::Sym).collect();
    }
    // `Type::member`, `::function`
    if prev == Some(Tok::ColonColon) {
        let start = chain_start(src, lx, tok - 1);
        if start < tok - 1 {
            let Some(receiver) = an.infer(u, start, tok - 1) else { return Vec::new() };
            let receiver = match receiver {
                Ty::Static(c) => an.instance(c),
                t => t,
            };
            return an.members_named(&receiver, name, u).into_iter().map(Target::Sym).collect();
        }
    }
    // A named argument: `f(name = ...)`.
    if lx.kind(tok + 1) == Some(Tok::Eq)
        && matches!(prev, Some(Tok::LParen | Tok::Comma))
        && let Some(open) = enclosing_open_paren(lx, tok)
        && let Some(params) = named_argument(an, u, open, name)
    {
        return params;
    }
    let found = an.resolve_name(u, t.start, name);
    let mut out = Vec::new();
    let mut members = Vec::new();
    for f in found {
        match f {
            Found::Sym(m) => members.push(m),
            Found::It { unit, brace } => out.push(Target::It { unit, brace }),
        }
    }
    out.extend(rank(an, u, members, call_args(tok + 1)).into_iter().map(Target::Sym));
    if out.is_empty() && an.has_package(name) {
        out.push(Target::Package(name.to_owned()));
    }
    out
}

/// The parameters named `name` of the function called with the parentheses at `open`, or the
/// arguments of the annotation there.
fn named_argument(an: &Analyzer, u: UnitId, open: usize, name: &str) -> Option<Vec<Target>> {
    let file = an.file(u);
    let lx = &file.lexed;
    let mut callee = open.checked_sub(1)?;
    if lx.kind(callee) == Some(Tok::Gt) {
        callee = super::resolve::matching_lt(lx, callee)?.checked_sub(1)?;
    }
    if lx.kind(callee) != Some(Tok::Ident) {
        return None;
    }
    let candidates = targets_at(an, u, callee);
    let mut out = Vec::new();
    for c in candidates {
        let Target::Sym(m) = c else { continue };
        let d = an.decl(m.sym);
        let file = an.file(m.sym.unit_id());
        let args = if d.kind == Kind::Annotation { annotation_arguments(file, m.sym.decl_id()) } else { d.params.clone() };
        if let Some(&p) = args.iter().find(|&&p| file.decls[p].name == name) {
            out.push(Target::Sym(Member { sym: m.sym.unit_id().sym(p), subst: Subst::new() }));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The arguments an annotation class takes, in order: its constructor parameters (Kotlin), and
/// the elements of a Java `@interface`, which are its methods.
fn annotation_arguments(file: &File, class: DeclId) -> Vec<DeclId> {
    let d = &file.decls[class];
    let elements = d.members.iter().copied().filter(|&m| file.decls[m].kind == Kind::Function);
    d.params.iter().copied().chain(elements).collect()
}

/// The name token of the annotation whose arguments open at the parenthesis `open`: `@Name(`,
/// `@pkg.Name(` or `@get:Name(`.
fn annotation_name(lx: &lexer::Lexed, open: usize) -> Option<usize> {
    let name = open.checked_sub(1)?;
    if lx.kind(name) != Some(Tok::Ident) {
        return None;
    }
    let mut start = name;
    while start >= 2 && lx.kind(start - 1) == Some(Tok::Dot) && lx.kind(start - 2) == Some(Tok::Ident) {
        start -= 2;
    }
    let mut at = start.checked_sub(1)?;
    if lx.kind(at) == Some(Tok::Colon) {
        at = at.checked_sub(2)?;
    }
    (lx.kind(at) == Some(Tok::At)).then_some(name)
}

/// The names of the arguments given in the parentheses at `open`, before the token `end`.
fn given_arguments(lx: &lexer::Lexed, src: &str, open: usize, end: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = open + 1;
    while i < end {
        match lx.kind(i) {
            // Nested brackets are skipped: their arguments are not this call's.
            Some(Tok::LParen | Tok::LBracket | Tok::LBrace) => {
                i = lx.matching.get(i).copied().flatten().filter(|&c| c > i).map_or(end, |c| c + 1);
                continue;
            }
            Some(Tok::Ident) if lx.kind(i + 1) == Some(Tok::Eq) && matches!(lx.kind(i - 1), Some(Tok::LParen | Tok::Comma)) => {
                out.push(lx.text(src, i).to_owned());
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Offers the arguments of the annotation whose parentheses open at `open` and that are not
/// given yet; `end` is the token the completion is at.
fn annotation_argument_completion(c: &mut Collector, open: usize, end: usize) {
    let an = c.an;
    let u = c.u;
    let lx = &an.file(u).lexed;
    let Some(name) = annotation_name(lx, open) else { return };
    let Some(class) = targets_at(an, u, name).iter().find_map(|t| t.sym()) else { return };
    if an.decl(class).kind != Kind::Annotation {
        return;
    }
    let file = an.file(class.unit_id());
    let src = &an.unit(class.unit_id()).text;
    let given = given_arguments(lx, &an.unit(u).text, open, end);
    for arg in annotation_arguments(file, class.decl_id()) {
        let d = &file.decls[arg];
        if !matches_prefix(&d.name, &c.prefix) || given.contains(&d.name) || !c.seen.insert(format!("{}|argument", d.name)) {
            continue;
        }
        let ty = d.ty.as_ref().map(|t| t.display(src)).unwrap_or_default();
        c.items.push(json!({
            "label": d.name,
            "kind": 10,
            "detail": ty,
            "sortText": format!("{}{}", Group::Local as u8, d.name),
            "filterText": d.name,
            "textEdit": { "range": c.range, "newText": format!("{} = ", escape_name(&d.name)) },
            "insertTextFormat": 1,
        }));
    }
}

fn enclosing_open_paren(lx: &lexer::Lexed, tok: usize) -> Option<usize> {
    let mut k = tok;
    while k > 0 {
        k -= 1;
        match lx.tokens[k].kind {
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose => k = lx.matching[k].filter(|&o| o < k)?,
            Tok::LParen => return Some(k),
            Tok::LBrace | Tok::LBracket | Tok::Semi => return None,
            _ => {}
        }
    }
    None
}

/// Orders candidates for a call by how well they fit its arguments.
fn rank(an: &Analyzer, u: UnitId, mut members: Vec<Member>, args: Option<Vec<Arg>>) -> Vec<Member> {
    let Some(args) = args else {
        // Not a call: values before functions.
        members.sort_by_key(|m| an.decl(m.sym).kind == Kind::Function);
        return members;
    };
    let arg_types: Vec<Option<Ty>> = args.iter().map(|a| if a.lambda { None } else { an.infer(u, a.start, a.end) }).collect();
    let score = |m: &Member| -> i32 {
        let d = an.decl(m.sym);
        let decls = &an.file(m.sym.unit_id()).decls;
        let params: Vec<&Decl> = d.params.iter().map(|&p| &decls[p]).collect();
        let required = params.iter().filter(|p| !p.has_default && !p.vararg).count();
        let vararg = params.iter().any(|p| p.vararg);
        let mut s = 0;
        if d.kind == Kind::Function || d.kind.is_class() {
            s += 1;
        }
        if args.len() >= required && (vararg || args.len() <= params.len()) {
            s += 10;
        }
        for (i, t) in arg_types.iter().enumerate() {
            let (Some(Ty::Class { sym, .. }), Some(p)) = (t, params.get(i)) else { continue };
            let Some(pt) = &p.ty else { continue };
            let arg_name = &an.decl(*sym).name;
            if pt.simple_name() == arg_name {
                s += 3;
            } else if d.type_params.iter().any(|t| t.name == pt.name) || pt.simple_name() == "Any" {
                s += 1;
            }
        }
        if d.doc.is_some() {
            s += 1;
        }
        s
    };
    members.sort_by_key(|m| std::cmp::Reverse(score(m)));
    members
}

/// The location of a target's declaration.
pub fn location(an: &Analyzer, target: &Target) -> Option<Value> {
    match target {
        Target::Sym(m) => {
            let unit = an.unit(m.sym.unit_id());
            let d = an.decl(m.sym);
            Some(json!({ "uri": text::path_to_uri(&unit.path), "range": text::range(&unit.text, d.name_span.clone()) }))
        }
        Target::It { unit, brace } => {
            let unit = an.unit(*unit);
            let t = unit.file.lexed.tokens[*brace];
            Some(json!({ "uri": text::path_to_uri(&unit.path), "range": text::range(&unit.text, t.span()) }))
        }
        Target::Package(_) => None,
    }
}

// ----- hover -----

pub fn hover(an: &Analyzer, u: UnitId, tok: usize) -> Option<Value> {
    let targets = targets_at(an, u, tok);
    let target = targets.first()?;
    let markdown = match target {
        Target::Sym(m) => describe(an, m),
        Target::It { unit, brace } => {
            let ty = an.lambda_ctx(*unit, *brace).and_then(|c| c.params.first().cloned().flatten());
            let ty = ty.map(|t| display_ty(an, &t)).unwrap_or_else(|| "?".into());
            format!("```kotlin\nit: {ty}\n```\nThe implicit parameter of the lambda.")
        }
        Target::Package(p) => format!("```kotlin\npackage {p}\n```"),
    };
    let unit = an.unit(u);
    let range = text::range(&unit.text, unit.file.lexed.tokens[tok].span());
    Some(json!({ "contents": { "kind": "markdown", "value": markdown }, "range": range }))
}

/// A declaration's signature, where it is, and its documentation, as Markdown.
pub fn describe(an: &Analyzer, m: &Member) -> String {
    let mut out = format!("```kotlin\n{}\n```", signature(an, m));
    let d = an.decl(m.sym);
    let file = an.file(m.sym.unit_id());
    let container = match file.owner_class(m.sym.decl_id()) {
        Some(c) if !d.local && d.kind != Kind::Parameter => Some(file.fq_name(c)),
        _ if !d.local && d.kind != Kind::Parameter && d.kind != Kind::Variable && !file.package.is_empty() => Some(file.package.clone()),
        _ => None,
    };
    if let Some(c) = container {
        out.push_str(&format!("\n\n*{c}*"));
    }
    if let Some(origin) = library_name(an, m.sym) {
        out.push_str(&format!(" — {origin}"));
    }
    if let Some(doc) = documentation(an, m.sym) {
        out.push_str("\n\n---\n\n");
        out.push_str(&doc);
    }
    out
}

fn library_name(an: &Analyzer, s: SymId) -> Option<String> {
    match &an.unit(s.unit_id()).origin {
        super::index::Origin::Library { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// The KDoc of a declaration (or of the `expect` declaration it implements) as Markdown.
pub fn documentation(an: &Analyzer, s: SymId) -> Option<String> {
    let d = an.decl(s);
    let doc = match &d.doc {
        Some(range) => &an.unit(s.unit_id()).text[range.clone()],
        None => {
            // An `actual` declaration or an override documents itself through its counterpart.
            if d.local || d.name.is_empty() {
                return None;
            }
            let fq = an.fq(s);
            let other = an.by_fq(&fq).into_iter().find(|&o| o != s && an.decl(o).doc.is_some() && an.decl(o).kind == d.kind)?;
            return documentation(an, other);
        }
    };
    let markdown = kdoc_to_markdown(doc);
    // Javadoc has inline tags and HTML that KDoc doesn't.
    if an.unit(s.unit_id()).is_java() { Some(javadoc_markdown(&markdown)) } else { Some(markdown) }
}

/// Turns a `/** ... */` comment into Markdown: block tags become lists, `[links]` code.
pub fn kdoc_to_markdown(doc: &str) -> String {
    let body = doc.trim_start_matches("/**").trim_end_matches("*/");
    let mut lines: Vec<String> = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        let t = t.strip_prefix('*').map(|r| r.strip_prefix(' ').unwrap_or(r)).unwrap_or(t);
        lines.push(t.trim_end().to_owned());
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut text = Vec::new();
    let mut params = Vec::new();
    let mut tags = Vec::new();
    let mut in_code = false;
    for line in lines {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            text.push(line);
            continue;
        }
        if in_code {
            text.push(line);
            continue;
        }
        let line = links(&line);
        if let Some(rest) = line.strip_prefix('@') {
            let (tag, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            match tag {
                "param" | "property" => {
                    let (name, desc) = rest.split_once(' ').unwrap_or((rest, ""));
                    params.push(format!("- `{}` {}", name.trim_matches(['[', ']', '`']), desc.trim()));
                }
                "return" => tags.push(format!("**Returns** {}", rest.trim())),
                "throws" | "exception" => {
                    let (name, desc) = rest.split_once(' ').unwrap_or((rest, ""));
                    tags.push(format!("**Throws** `{}` {}", name.trim_matches(['[', ']', '`']), desc.trim()));
                }
                "see" => tags.push(format!("**See also** {}", rest.trim())),
                "since" => tags.push(format!("**Since** Kotlin {}", rest.trim())),
                "sample" | "suppress" => {}
                other => tags.push(format!("**{other}** {}", rest.trim())),
            }
        } else if let Some(last) = params.last_mut().filter(|_| !line.is_empty() && tags.is_empty() && text.is_empty()) {
            last.push(' ');
            last.push_str(line.trim());
        } else {
            text.push(line);
        }
    }
    let mut out = text.join("\n").trim().to_owned();
    if !params.is_empty() {
        out.push_str("\n\n**Parameters**\n");
        out.push_str(&params.join("\n"));
    }
    for tag in tags {
        out.push_str("\n\n");
        out.push_str(&tag);
    }
    out.trim().to_owned()
}

/// `[name]` and `[text][name]` KDoc links as code.
fn links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close) if !after[..close].contains(' ') || after[close + 1..].starts_with('[') => {
                let inner = &after[..close];
                let mut next = &after[close + 1..];
                if let Some(target) = next.strip_prefix('[').and_then(|n| n.find(']').map(|c| &n[..c])) {
                    next = &next[target.len() + 2..];
                    out.push_str(&format!("`{inner}`"));
                } else {
                    out.push_str(&format!("`{inner}`"));
                }
                rest = next;
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

const SHOWN_MODIFIERS: &[&str] = &[
    "private",
    "protected",
    "internal",
    "expect",
    "actual",
    "abstract",
    "open",
    "sealed",
    "data",
    "inner",
    "value",
    "fun",
    "override",
    "lateinit",
    "const",
    "external",
    "suspend",
    "operator",
    "infix",
    "inline",
    "tailrec",
    "vararg",
];

/// A declaration's signature as Kotlin, with type parameters bound by `m.subst` where known.
pub fn signature(an: &Analyzer, m: &Member) -> String {
    let s = m.sym;
    let d = an.decl(s);
    let unit = an.unit(s.unit_id());
    let src = &unit.text;
    let file = &unit.file;
    // `expect` and `actual` matter in the project, not in a library's API.
    let mods: Vec<&str> = d
        .modifiers
        .iter()
        .map(String::as_str)
        .filter(|m| SHOWN_MODIFIERS.contains(m) && !(s.lib && matches!(*m, "expect" | "actual")))
        .collect();
    let mut out = mods.join(" ");
    if !out.is_empty() {
        out.push(' ');
    }
    // Functions keep their declared signature; a property's type gets the receiver's type
    // arguments when they are all known.
    let ty_text = |t: &super::syntax::TypeRef| -> String {
        if !m.subst.is_empty()
            && d.kind != Kind::Function
            && let Some(ty) = an.ty_of_ref(t, super::resolve::Ctx { unit: s.unit_id(), decl: Some(s.decl_id()) }, &m.subst)
            && fully_known(&ty)
        {
            return display_ty(an, &ty);
        }
        t.display(src)
    };
    let type_params = |out: &mut String| {
        if !d.type_params.is_empty() {
            let tps: Vec<String> = d
                .type_params
                .iter()
                .map(|t| match &t.bound {
                    Some(b) => format!("{}: {}", t.name, b.display(src)),
                    None => t.name.clone(),
                })
                .collect();
            out.push_str(&format!("<{}>", tps.join(", ")));
        }
    };
    let params = |ids: &[usize]| -> String {
        let list: Vec<String> = ids
            .iter()
            .map(|&p| {
                let pd = &file.decls[p];
                let mut s = String::new();
                if pd.vararg {
                    s.push_str("vararg ");
                }
                if pd.kind == Kind::Property {
                    s.push_str(if pd.mutable { "var " } else { "val " });
                }
                s.push_str(&pd.name);
                if let Some(t) = &pd.ty {
                    s.push_str(": ");
                    s.push_str(&ty_text(t));
                }
                if pd.has_default && !pd.vararg {
                    s.push_str(" = …");
                }
                s
            })
            .collect();
        if list.len() > 3 { format!("(\n    {}\n)", list.join(",\n    ")) } else { format!("({})", list.join(", ")) }
    };
    match d.kind {
        k if k.is_class() => {
            out.push_str(if k == Kind::Companion { "companion object" } else { k.keyword() });
            if !(k == Kind::Companion && d.name == "Companion") {
                out.push(' ');
                out.push_str(&d.name);
            }
            type_params(&mut out);
            if d.params.iter().any(|_| true) {
                out.push_str(&params(&d.params));
            }
            if !d.supertypes.is_empty() {
                let sts: Vec<String> = d.supertypes.iter().map(|t| t.display(src)).collect();
                out.push_str(" : ");
                out.push_str(&sts.join(", "));
            }
        }
        Kind::EnumEntry => {
            let owner = file.owner_class(s.decl_id()).map(|c| file.decls[c].name.clone()).unwrap_or_default();
            out.push_str(&format!("enum entry {owner}.{}", d.name));
        }
        Kind::TypeAlias => {
            out.push_str("typealias ");
            out.push_str(&d.name);
            type_params(&mut out);
            if let Some(t) = &d.ty {
                out.push_str(" = ");
                out.push_str(&t.display(src));
            }
        }
        Kind::Function | Kind::Constructor => {
            out.push_str(if d.kind == Kind::Function { "fun " } else { "constructor " });
            if d.kind == Kind::Function {
                if !d.type_params.is_empty() {
                    type_params(&mut out);
                    out.push(' ');
                }
                if let Some(r) = &d.receiver {
                    out.push_str(&ty_text(r));
                    out.push('.');
                }
            }
            out.push_str(&d.name);
            out.push_str(&params(&d.params));
            if d.kind == Kind::Function {
                let ret = match &d.ty {
                    Some(t) => Some(ty_text(t)),
                    None => Some(shown_ty(an, m)).filter(|t| !t.is_empty()),
                };
                if let Some(r) = ret {
                    out.push_str(": ");
                    out.push_str(&r);
                }
            }
        }
        Kind::Property | Kind::Variable | Kind::Parameter => {
            if d.kind != Kind::Parameter {
                out.push_str(if d.mutable { "var " } else { "val " });
                if !d.type_params.is_empty() {
                    type_params(&mut out);
                    out.push(' ');
                }
                if let Some(r) = &d.receiver {
                    out.push_str(&ty_text(r));
                    out.push('.');
                }
            }
            out.push_str(&d.name);
            let ty = match &d.ty {
                Some(t) if !d.vararg => Some(ty_text(t)),
                _ => Some(shown_ty(an, m)).filter(|t| !t.is_empty()),
            };
            if let Some(t) = ty {
                out.push_str(": ");
                out.push_str(&t);
            }
        }
        _ => out.push_str(&d.name),
    }
    normalize_signature(&out)
}

fn normalize_signature(s: &str) -> String {
    if s.contains('\n') { s.to_owned() } else { normalize(s) }
}

/// Whether a type has no unknown parts.
pub fn fully_known(ty: &Ty) -> bool {
    match ty {
        Ty::Class { args, .. } => args.iter().all(|a| a.as_ref().is_some_and(fully_known)),
        Ty::Func { receiver, params, ret } => {
            receiver.as_deref().is_none_or(fully_known)
                && params.iter().all(|p| p.as_ref().is_some_and(fully_known))
                && ret.as_deref().is_some_and(fully_known)
        }
        _ => true,
    }
}

/// The type a completion item or signature shows: the inferred one when it is fully known,
/// else the declared one.
fn shown_ty(an: &Analyzer, m: &Member) -> String {
    let d = an.decl(m.sym);
    let inferred = an.value_ty(m.sym, &m.subst);
    match (inferred, &d.ty) {
        (Some(t), _) if fully_known(&t) => display_ty(an, &t),
        (_, Some(declared)) => declared.display(&an.unit(m.sym.unit_id()).text),
        (Some(t), None) => display_ty(an, &t),
        (None, None) => String::new(),
    }
}

/// A type as Kotlin source.
pub fn display_ty(an: &Analyzer, ty: &Ty) -> String {
    match ty {
        Ty::Class { sym, args, nullable } => {
            let mut s = an.decl(*sym).name.clone();
            if !args.is_empty() {
                let a: Vec<String> = args.iter().map(|a| a.as_ref().map_or_else(|| "*".to_owned(), |t| display_ty(an, t))).collect();
                if a.iter().any(|x| x != "*") || args.len() == an.decl(*sym).type_params.len() {
                    s.push_str(&format!("<{}>", a.join(", ")));
                }
            }
            if *nullable {
                s.push('?');
            }
            s
        }
        Ty::Func { receiver, params, ret } => {
            let p: Vec<String> = params.iter().map(|a| a.as_ref().map_or_else(|| "?".to_owned(), |t| display_ty(an, t))).collect();
            let r = ret.as_ref().map_or_else(|| "?".to_owned(), |t| display_ty(an, t));
            let recv = receiver.as_ref().map(|t| format!("{}.", display_ty(an, t))).unwrap_or_default();
            format!("{recv}({}) -> {r}", p.join(", "))
        }
        Ty::Static(s) => an.decl(*s).name.clone(),
        Ty::Package(p) => p.clone(),
    }
}

// ----- completion -----

#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionOptions {
    pub snippets: bool,
}

const KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "break",
    "by",
    "class",
    "companion",
    "const",
    "continue",
    "data",
    "do",
    "else",
    "enum",
    "false",
    "for",
    "fun",
    "if",
    "import",
    "in",
    "init",
    "inline",
    "interface",
    "internal",
    "is",
    "lateinit",
    "null",
    "object",
    "open",
    "operator",
    "override",
    "private",
    "protected",
    "public",
    "return",
    "sealed",
    "super",
    "suspend",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "val",
    "var",
    "when",
    "while",
];

/// The snippet a keyword inserts when it takes a condition or a head in parentheses, with the
/// cursor inside them. Other keywords insert their plain text.
fn keyword_scaffold(word: &str) -> Option<&'static str> {
    match word {
        "if" => Some("if ($0)"),
        "while" => Some("while ($0)"),
        "for" => Some("for ($1 in $2)"),
        "when" => Some("when ($0) {}"),
        _ => None,
    }
}

/// How completion items are grouped, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Local = 0,
    Member = 1,
    Extension = 2,
    File = 3,
    Imported = 4,
    Keyword = 5,
    NotImported = 6,
}

struct Collector<'a, 'b> {
    an: &'a Analyzer<'b>,
    u: UnitId,
    prefix: String,
    items: Vec<Value>,
    seen: HashSet<String>,
    opts: CompletionOptions,
    range: Value,
    /// Whether `(` already follows the cursor.
    paren_follows: bool,
    /// Only types (after `:` or `as`), so no calls get parentheses.
    types_only: bool,
    /// Only annotation classes (after `@`).
    annotations_only: bool,
    /// Whether some classes were left out, so the list is asked for again as more is typed.
    incomplete: bool,
    generation: u64,
}

/// The most JDK classes one completion offers: each one parses its file, and the rest of the
/// names come with the next keystrokes.
const JDK_OFFERS: usize = 150;

/// Whether `name` matches what was typed: the first letter, then the rest in order.
pub fn matches_prefix(name: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let mut chars = name.chars();
    let mut p = prefix.chars();
    let (Some(first), Some(pf)) = (chars.next(), p.next()) else { return false };
    if !first.eq_ignore_ascii_case(&pf) {
        return false;
    }
    let mut rest = chars.map(|c| c.to_ascii_lowercase());
    p.all(|c| {
        let c = c.to_ascii_lowercase();
        rest.any(|n| n == c)
    })
}

impl Collector<'_, '_> {
    fn add(&mut self, m: &Member, group: Group, import: Option<String>) {
        let d = self.an.decl(m.sym);
        if d.name.is_empty() || d.kind == Kind::Constructor || !matches_prefix(&d.name, &self.prefix) {
            return;
        }
        if self.types_only && !(d.kind.is_class() || d.kind == Kind::TypeAlias) {
            return;
        }
        if self.annotations_only && d.kind != Kind::Annotation {
            return;
        }
        let sig = signature(self.an, m);
        let key = format!("{}|{}", d.name, sig);
        if !self.seen.insert(key) {
            return;
        }
        let file = self.an.file(m.sym.unit_id());
        let kind = completion_kind(d, file.owner_class(m.sym.decl_id()).is_some());
        let mut label_detail = String::new();
        let mut description = String::new();
        let decls = &file.decls;
        match d.kind {
            Kind::Function => {
                let params: Vec<String> = d
                    .params
                    .iter()
                    .map(|&p| {
                        let pd = &decls[p];
                        let ty = pd.ty.as_ref().map(|t| t.display(&self.an.unit(m.sym.unit_id()).text)).unwrap_or_default();
                        format!("{}{}: {}", if pd.vararg { "vararg " } else { "" }, pd.name, ty)
                    })
                    .collect();
                label_detail = format!("({})", params.join(", "));
                description = shown_ty(self.an, m);
            }
            k if k.is_value() => {
                description = shown_ty(self.an, m);
            }
            _ => {}
        }
        if let Some(r) = &d.receiver
            && d.kind == Kind::Function
        {
            label_detail = format!("{label_detail} for {}", r.display(&self.an.unit(m.sym.unit_id()).text));
        }
        let package = self.an.package_of(m.sym);
        if !d.local && file.owner_class(m.sym.decl_id()).is_none() && !package.is_empty() {
            label_detail = format!("{label_detail} ({package})");
        }
        let name = escape_name(&d.name);
        let (new_text, snippet) = self.insert_text(d, &name, decls);
        let mut item = json!({
            "label": d.name,
            "kind": kind,
            "detail": sig,
            "labelDetails": { "detail": label_detail.trim_start(), "description": description },
            "sortText": format!("{}{}", group as u8, d.name),
            "filterText": d.name,
            "textEdit": { "range": self.range, "newText": new_text },
            "insertTextFormat": if snippet { 2 } else { 1 },
            "data": { "lib": m.sym.lib, "unit": m.sym.unit, "decl": m.sym.decl, "generation": self.generation },
        });
        if self.an.decl(m.sym).has("@Deprecated") {
            item["tags"] = json!([1]);
        }
        if let Some(fq) = import {
            let file = self.an.file(self.u);
            let text = &self.an.unit(self.u).text;
            let at = file.import_offset;
            let new_text = if file.imports.is_empty() && file.import_after_package {
                format!("\n\nimport {fq}")
            } else if at == 0 {
                format!("import {fq}\n\n")
            } else {
                format!("\nimport {fq}")
            };
            item["additionalTextEdits"] = json!([{ "range": text::range(text, at..at), "newText": new_text }]);
            item["labelDetails"]["description"] = json!(format!("import {fq}"));
        }
        self.items.push(item);
    }

    fn insert_text(&self, d: &Decl, name: &str, decls: &[Decl]) -> (String, bool) {
        if d.kind != Kind::Function || self.paren_follows || self.types_only {
            return (name.to_owned(), false);
        }
        let required: Vec<&Decl> = d.params.iter().map(|&p| &decls[p]).collect();
        if required.is_empty() {
            return (format!("{name}()"), false);
        }
        if !self.opts.snippets {
            return (format!("{name}("), false);
        }
        if required.len() == 1 && required[0].ty.as_ref().is_some_and(|t| t.func.is_some()) {
            return (format!("{name} {{ $0 }}"), true);
        }
        (format!("{name}($0)"), true)
    }

    fn keyword(&mut self, word: &str) {
        if self.annotations_only || !matches_prefix(word, &self.prefix) || !self.seen.insert(format!("keyword|{word}")) {
            return;
        }
        let scaffold = keyword_scaffold(word).filter(|_| self.opts.snippets && !self.paren_follows && !self.types_only);
        let (new_text, snippet) = match scaffold {
            Some(s) => (s.to_owned(), true),
            None => (word.to_owned(), false),
        };
        self.items.push(json!({
            "label": word,
            "kind": 14,
            "sortText": format!("{}{}", Group::Keyword as u8, word),
            "textEdit": { "range": self.range, "newText": new_text },
            "insertTextFormat": if snippet { 2 } else { 1 },
        }));
    }

    fn local(&mut self, name: &str, detail: String) {
        if self.annotations_only || !matches_prefix(name, &self.prefix) || !self.seen.insert(format!("{name}|local")) {
            return;
        }
        self.items.push(json!({
            "label": name,
            "kind": 6,
            "detail": detail,
            "sortText": format!("{}{}", Group::Local as u8, name),
            "textEdit": { "range": self.range, "newText": name },
        }));
    }
}

/// A name as Kotlin source: backquoted when it is a keyword or not a plain identifier.
pub fn escape_name(name: &str) -> String {
    let plain = name.chars().next().is_some_and(lexer::is_ident_start) && name.chars().all(lexer::is_ident_char);
    if plain && !HARD_KEYWORDS.contains(&name) { name.to_owned() } else { format!("`{name}`") }
}

/// The LSP `CompletionItemKind` of a declaration.
fn completion_kind(d: &Decl, member: bool) -> u8 {
    match d.kind {
        Kind::Class | Kind::Annotation => 7,
        Kind::Interface => 8,
        Kind::Object | Kind::Companion => 9,
        Kind::Enum => 13,
        Kind::EnumEntry => 20,
        Kind::TypeAlias => 7,
        Kind::Function if member => 2,
        Kind::Function => 3,
        Kind::Constructor => 4,
        Kind::Property if d.has("const") => 21,
        Kind::Property if member => 10,
        Kind::Property => 10,
        Kind::Parameter | Kind::Variable => 6,
    }
}

/// Completion at `offset` in `u`.
pub fn completion(an: &Analyzer, u: UnitId, offset: usize, opts: CompletionOptions, generation: u64) -> Value {
    let empty = json!({ "isIncomplete": false, "items": [] });
    let unit = an.unit(u);
    let src = &unit.text;
    let file = &unit.file;
    let lx = &file.lexed;
    if lx.in_comment(offset) || lx.in_string_text(offset) {
        return empty;
    }
    let mut p = offset.min(src.len());
    while p > 0 {
        let Some(c) = src[..p].chars().next_back() else { break };
        if !lexer::is_ident_char(c) {
            break;
        }
        p -= c.len_utf8();
    }
    let prefix = src[p..offset].to_owned();
    if prefix.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return empty;
    }
    let after = src[offset..].chars().take_while(|c| lexer::is_ident_char(*c)).map(char::len_utf8).sum::<usize>();
    let range = text::range(src, p..offset);
    let paren_follows = src[offset + after..].starts_with('(');
    let prev = lx.before(p);
    let prev_kind = prev.and_then(|i| lx.kind(i));
    let types_only = prev_kind == Some(Tok::Colon) && prev.is_some_and(|i| is_type_position(an, u, i))
        || prev.is_some_and(|i| lx.is(src, i, "as") || lx.is(src, i, "is"));
    let mut c = Collector {
        an,
        u,
        prefix: prefix.clone(),
        items: Vec::new(),
        seen: HashSet::new(),
        opts,
        range,
        paren_follows,
        types_only,
        annotations_only: prev_kind == Some(Tok::At),
        incomplete: false,
        generation,
    };
    // `package a.b` gets nothing; `import a.b.` gets packages and declarations.
    let line_start = src[..p].rfind('\n').map_or(0, |i| i + 1);
    let line = src[line_start..p].trim_start();
    if line.starts_with("package ") {
        return empty;
    }
    if let Some(path) = line.strip_prefix("import ") {
        import_completion(&mut c, path.trim());
        return finish(c);
    }
    // The arguments of an annotation: `@Name(arg = ...)`.
    if let Some(prev) = prev.filter(|_| matches!(prev_kind, Some(Tok::LParen | Tok::Comma))) {
        let open = if prev_kind == Some(Tok::LParen) { Some(prev) } else { enclosing_open_paren(lx, prev) };
        if let Some(open) = open {
            annotation_argument_completion(&mut c, open, prev + 1);
        }
    }
    match prev_kind {
        Some(Tok::Dot | Tok::SafeDot) => {
            let dot = prev.unwrap_or_default();
            let start = chain_start(src, lx, dot);
            if start < dot {
                // A qualified annotation name, `@pkg.Name`, takes annotations only.
                c.annotations_only = start > 0 && lx.kind(start - 1) == Some(Tok::At);
                if let Some(receiver) = an.infer(u, start, dot) {
                    let receiver = if prev_kind == Some(Tok::SafeDot) { strip_nullable(receiver) } else { receiver };
                    member_completion(&mut c, &receiver);
                }
                return finish(c);
            }
            return empty;
        }
        Some(Tok::ColonColon) => {
            let cc = prev.unwrap_or_default();
            let start = chain_start(src, lx, cc);
            if start < cc {
                if let Some(receiver) = an.infer(u, start, cc) {
                    let receiver = match receiver {
                        Ty::Static(s) => an.instance(s),
                        t => t,
                    };
                    for m in an.members(&receiver, u, false) {
                        c.add(&m, Group::Member, None);
                    }
                }
                c.paren_follows = true;
                return finish(c);
            }
            c.paren_follows = true;
        }
        _ => {}
    }
    scope_completion(&mut c, p);
    finish(c)
}

/// Whether the `:` at token `i` starts a type (of a declaration or parameter) rather than, say,
/// a supertype list or a named argument.
fn is_type_position(an: &Analyzer, u: UnitId, i: usize) -> bool {
    let lx = &an.file(u).lexed;
    let src = &an.unit(u).text;
    i > 0 && (lx.kind(i - 1) == Some(Tok::Ident) || lx.kind(i - 1) == Some(Tok::RParen)) && !lx.is(src, i - 1, "else")
}

fn finish(c: Collector) -> Value {
    const LIMIT: usize = 1500;
    let mut items = c.items;
    let truncated = items.len() > LIMIT;
    if truncated {
        items.sort_by(|a, b| a["sortText"].as_str().cmp(&b["sortText"].as_str()));
        items.truncate(LIMIT);
    }
    json!({ "isIncomplete": truncated || c.incomplete || c.prefix.is_empty(), "items": items })
}

fn member_completion(c: &mut Collector, receiver: &Ty) {
    let an = c.an;
    if let Ty::Package(p) = receiver {
        package_contents(c, p);
        return;
    }
    let own: HashSet<SymId> = an.members(receiver, c.u, false).into_iter().map(|m| m.sym).collect();
    for m in an.members(receiver, c.u, true) {
        let d = an.decl(m.sym);
        let group = if d.receiver.is_none() {
            Group::Member
        } else if own.contains(&m.sym) {
            Group::Extension
        } else {
            Group::NotImported
        };
        let import = (group == Group::NotImported).then(|| an.fq(m.sym));
        c.add(&m, group, import);
    }
}

fn package_contents(c: &mut Collector, package: &str) {
    let an = c.an;
    let prefix = format!("{package}.");
    let mut subpackages: Vec<&str> = Vec::new();
    for index in [an.proj, an.lib] {
        for p in index.packages.range(prefix.clone()..) {
            let Some(rest) = p.strip_prefix(&prefix) else { break };
            let seg = rest.split('.').next().unwrap_or(rest);
            if !subpackages.contains(&seg) {
                subpackages.push(seg);
            }
        }
    }
    for seg in subpackages {
        if matches_prefix(seg, &c.prefix) && c.seen.insert(format!("package|{seg}")) {
            c.items
                .push(json!({ "label": seg, "kind": 9, "sortText": format!("1{seg}"), "textEdit": { "range": c.range, "newText": seg } }));
        }
    }
    for s in package_syms(an, package) {
        if an.visible(c.u, s) {
            c.add(&Member { sym: s, subst: Subst::new() }, Group::Member, None);
        }
    }
}

/// The top-level declarations of `package` in the project and the libraries, JDK classes included.
fn package_syms(an: &Analyzer, package: &str) -> Vec<SymId> {
    let mut out: Vec<SymId> =
        [an.proj, an.lib].into_iter().flat_map(|index| index.by_package.get(package).into_iter().flatten().copied()).collect();
    out.extend(an.lib.jdk_in_package(package));
    out
}

fn import_completion(c: &mut Collector, path: &str) {
    let Some((package, _)) = path.rsplit_once('.') else {
        // The first segment: top-level packages.
        let mut roots: Vec<String> = Vec::new();
        for index in [c.an.proj, c.an.lib] {
            for p in &index.packages {
                let root = p.split('.').next().unwrap_or(p).to_owned();
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        for root in roots {
            if matches_prefix(&root, &c.prefix) {
                c.items.push(json!({ "label": root, "kind": 9, "textEdit": { "range": c.range, "newText": root } }));
            }
        }
        return;
    };
    c.paren_follows = true;
    package_contents(c, package);
    // `import a.B.` for nested classes and object members.
    for class in c.an.by_fq(package) {
        if c.an.decl(class).kind.is_class() {
            for m in c.an.members(&Ty::Static(class), c.u, false) {
                c.add(&m, Group::Member, None);
            }
        }
    }
}

fn scope_completion(c: &mut Collector, offset: usize) {
    let an = c.an;
    let u = c.u;
    let file = an.file(u);
    let chain = file.scope_chain(offset);
    for (ci, &sid) in chain.iter().enumerate() {
        let scope = &file.scopes[sid];
        if let ScopeKind::Lambda { it: true, brace } = scope.kind {
            let ty = an.lambda_ctx(u, brace).and_then(|c| c.params.first().cloned().flatten());
            c.local("it", format!("it: {}", ty.map(|t| display_ty(an, &t)).unwrap_or_else(|| "?".into())));
        }
        for &d in &scope.decls {
            let decl = &file.decls[d];
            if scope.kind == ScopeKind::File {
                break;
            }
            if decl.name.is_empty() || !(decl.kind == Kind::Parameter || decl.name_span.end <= offset || decl.kind.is_class()) {
                continue;
            }
            c.add(&Member { sym: u.sym(d), subst: Subst::new() }, Group::Local, None);
        }
        let receiver = match scope.kind {
            ScopeKind::Function(fd) => file.decls[fd]
                .receiver
                .as_ref()
                .and_then(|r| an.ty_of_ref(r, super::resolve::Ctx { unit: u, decl: Some(fd) }, &Subst::new())),
            ScopeKind::Lambda { brace, .. } => an.lambda_ctx(u, brace).and_then(|c| c.receiver),
            ScopeKind::Class(cd) => {
                let in_function = chain[..ci].iter().any(|&s| matches!(file.scopes[s].kind, ScopeKind::Function(_)));
                if !in_function {
                    for &p in &file.decls[cd].params {
                        if file.decls[p].kind == Kind::Parameter {
                            c.add(&Member { sym: u.sym(p), subst: Subst::new() }, Group::Local, None);
                        }
                    }
                }
                let class = if file.decls[cd].kind == Kind::EnumEntry { file.owner_class(cd).unwrap_or(cd) } else { cd };
                if !file.decls[class].name.is_empty() {
                    for m in an.members(&Ty::Static(u.sym(class)), u, false) {
                        c.add(&m, Group::Member, None);
                    }
                }
                an.this_ty(u, scope.range.start + 1, None).or_else(|| Some(an.instance(u.sym(class))))
            }
            _ => None,
        };
        if let Some(receiver) = receiver {
            for m in an.members(&receiver, u, false) {
                let group = if an.decl(m.sym).receiver.is_some() { Group::Extension } else { Group::Member };
                c.add(&m, group, None);
            }
        }
    }
    // The file, its package, imports and default imports.
    for &t in &file.top {
        if file.decls[t].receiver.is_none() {
            c.add(&Member { sym: u.sym(t), subst: Subst::new() }, Group::File, None);
        }
    }
    let mut packages: Vec<(&str, Group)> = vec![(&file.package, Group::File)];
    let star: Vec<String> = file.imports.iter().filter(|i| i.star).map(|i| i.fq()).collect();
    packages.extend(star.iter().map(|p| (p.as_str(), Group::Imported)));
    packages.extend(DEFAULT_IMPORTS.iter().map(|p| (*p, Group::Imported)));
    for import in file.imports.iter().filter(|i| !i.star) {
        for s in an.import_targets(import.fq()) {
            if an.decl(s).receiver.is_none() {
                c.add(&Member { sym: s, subst: Subst::new() }, Group::Imported, None);
            }
        }
    }
    let mut in_scope: HashSet<SymId> = HashSet::new();
    for (package, group) in &packages {
        for s in package_syms(an, package) {
            in_scope.insert(s);
            if an.visible(u, s) {
                c.add(&Member { sym: s, subst: Subst::new() }, *group, None);
            }
        }
    }
    for word in KEYWORDS {
        c.keyword(word);
    }
    // Declarations that need an import, once something is typed.
    if !c.prefix.is_empty() {
        let mut offered: HashMap<String, usize> = HashMap::new();
        for index in [an.proj, an.lib] {
            for (name, syms) in &index.by_name {
                if matches_prefix(name, &c.prefix) {
                    offer_by_import(c, file, name, syms, &in_scope, &mut offered);
                }
            }
        }
        // The JDK's classes are parsed as they are offered, so only a bounded number of them.
        let mut jdk_offered = 0;
        for name in an.lib.jdk_simple_names() {
            if !matches_prefix(name, &c.prefix) {
                continue;
            }
            if jdk_offered >= JDK_OFFERS {
                c.incomplete = true;
                break;
            }
            let before = c.items.len();
            offer_by_import(c, file, name, &an.lib.jdk_named(name), &in_scope, &mut offered);
            jdk_offered += c.items.len() - before;
        }
    }
}

/// Offers the declarations `syms`, named `name`, that need an import to be used, at most eight per name.
fn offer_by_import(
    c: &mut Collector,
    file: &File,
    name: &str,
    syms: &[SymId],
    in_scope: &HashSet<SymId>,
    offered: &mut HashMap<String, usize>,
) {
    let an = c.an;
    for &s in syms {
        let package = an.package_of(s);
        if in_scope.contains(&s) || package.is_empty() || (s.lib && is_internal_package(package)) || !an.visible(c.u, s) {
            continue;
        }
        let count = offered.entry(name.to_owned()).or_default();
        if *count >= 8 {
            continue;
        }
        *count += 1;
        let fq = an.fq(s);
        if file.imports.iter().any(|i| i.fq() == fq) {
            continue;
        }
        c.add(&Member { sym: s, subst: Subst::new() }, Group::NotImported, Some(fq));
    }
}

/// The documentation of a completion item, for `completionItem/resolve`.
pub fn resolve_item(an: &Analyzer, mut item: Value) -> Value {
    let data = &item["data"];
    let (Some(lib), Some(unit), Some(decl)) = (data["lib"].as_bool(), data["unit"].as_u64(), data["decl"].as_u64()) else { return item };
    let index = an.index_of(lib);
    let Some(u) = index.unit(UnitId { lib, unit: unit as u32 }) else { return item };
    if decl as usize >= u.file.decls.len() {
        return item;
    }
    let sym = SymId { lib, unit: unit as u32, decl: decl as u32 };
    if let Some(doc) = documentation(an, sym) {
        item["documentation"] = json!({ "kind": "markdown", "value": doc });
    }
    item
}

// ----- references and rename -----

/// The declarations a rename of `target` must change together: `expect` and `actual`
/// counterparts and overrides in the project.
pub fn rename_group(an: &Analyzer, target: SymId) -> Vec<SymId> {
    let d = an.decl(target);
    let mut group = vec![target];
    if d.local || matches!(d.kind, Kind::Parameter | Kind::Variable) {
        return group;
    }
    let fq = an.fq(target);
    for other in an.by_fq(&fq) {
        if other != target && !other.lib && same_shape(an, other, target) {
            group.push(other);
        }
    }
    let file = an.file(target.unit_id());
    if let Some(owner) = file.owner_class(target.decl_id())
        && matches!(d.kind, Kind::Function | Kind::Property)
    {
        let owner = target.unit_id().sym(owner);
        let owner_fq = an.fq(owner);
        // Classes in the project related to the owner: its supertypes and subtypes.
        let supers: HashSet<String> = an.closure(&an.instance(owner)).iter().map(|(c, _)| an.fq(*c)).collect();
        for (ui, unit) in an.proj.units.iter().enumerate() {
            let uid = UnitId { lib: false, unit: ui as u32 };
            for (ci, cd) in unit.file.decls.iter().enumerate() {
                if !cd.kind.is_class() || cd.name.is_empty() {
                    continue;
                }
                let class = uid.sym(ci);
                let class_fq = an.fq(class);
                let related = supers.contains(&class_fq) || an.closure(&an.instance(class)).iter().any(|(c, _)| an.fq(*c) == owner_fq);
                if !related || class_fq == owner_fq {
                    continue;
                }
                for &m in &cd.members {
                    let ms = uid.sym(m);
                    if unit.file.decls[m].name == d.name && same_shape(an, ms, target) && !group.contains(&ms) {
                        group.push(ms);
                    }
                }
            }
        }
    }
    group
}

fn same_shape(an: &Analyzer, a: SymId, b: SymId) -> bool {
    let (da, db) = (an.decl(a), an.decl(b));
    da.kind.is_class() == db.kind.is_class()
        && (da.kind == Kind::Function) == (db.kind == Kind::Function)
        && da.params.len() == db.params.len()
}

/// Every place in the project that refers to one of `group`, as (unit, byte range).
pub fn references(an: &Analyzer, group: &[SymId], include_declaration: bool) -> Vec<(UnitId, std::ops::Range<usize>)> {
    let Some(&first) = group.first() else { return Vec::new() };
    let d = an.decl(first);
    let name = d.name.clone();
    let mut out = Vec::new();
    // A function's parameters are named in calls anywhere; locals and private declarations
    // only in their file.
    let local = d.local || d.kind == Kind::Variable || d.is_private();
    for (ui, unit) in an.proj.units.iter().enumerate() {
        let uid = UnitId { lib: false, unit: ui as u32 };
        // Java files are read for their declarations only: their uses aren't resolved.
        if unit.is_java() || (local && !first.lib && uid != first.unit_id()) {
            continue;
        }
        let lx = &unit.file.lexed;
        for (i, t) in lx.tokens.iter().enumerate() {
            if t.kind != Tok::Ident || lx.text(&unit.text, i) != name {
                continue;
            }
            let targets = targets_at(an, uid, i);
            let hit = targets.iter().take(1).any(|t| t.sym().is_some_and(|s| group.contains(&s)))
                || targets
                    .iter()
                    .any(|t| t.sym().is_some_and(|s| group.contains(&s) && an.decl(s).name_span == unit.file.lexed.tokens[i].span()));
            if !hit {
                continue;
            }
            let is_decl = group.iter().any(|&s| s.unit_id() == uid && an.decl(s).name_span == t.span());
            if is_decl && !include_declaration {
                continue;
            }
            out.push((uid, t.span()));
        }
    }
    out
}

/// A new name as Kotlin source: backquoted when it isn't a plain identifier. `None` when it
/// can't be a name at all.
pub fn new_name_text(name: &str) -> Option<String> {
    let name = name.trim();
    let bare = name.strip_prefix('`').and_then(|n| n.strip_suffix('`')).unwrap_or(name);
    if bare.is_empty() || bare.contains(['`', '\n', '\r', '.', ';', '[', ']', '/', '<', '>', ':', '\\']) {
        return None;
    }
    Some(escape_name(bare))
}

// ----- symbols -----

/// The LSP `SymbolKind` of a declaration.
pub fn symbol_kind(d: &Decl, member: bool) -> u8 {
    match d.kind {
        Kind::Class | Kind::Annotation => 5,
        Kind::Interface => 11,
        Kind::Object | Kind::Companion => 19,
        Kind::Enum => 10,
        Kind::EnumEntry => 22,
        Kind::TypeAlias => 26,
        Kind::Function if member => 6,
        Kind::Function => 12,
        Kind::Constructor => 9,
        Kind::Property if d.has("const") => 14,
        Kind::Property if member => 7,
        Kind::Property => 13,
        Kind::Parameter | Kind::Variable => 13,
    }
}

/// The outline of a file.
pub fn document_symbols(an: &Analyzer, u: UnitId) -> Value {
    let unit = an.unit(u);
    fn symbol(unit: &super::index::Unit, id: usize, member: bool) -> Option<Value> {
        let d = &unit.file.decls[id];
        if d.name.is_empty() && d.kind != Kind::Constructor {
            return None;
        }
        let children: Vec<Value> = d.members.iter().filter_map(|&m| symbol(unit, m, true)).collect();
        let name = if d.kind == Kind::Constructor { "constructor".to_owned() } else { d.name.clone() };
        let span = d.span.start.min(d.name_span.start)..d.span.end.max(d.name_span.end);
        Some(json!({
            "name": name,
            "kind": symbol_kind(d, member),
            "range": text::range(&unit.text, span),
            "selectionRange": text::range(&unit.text, d.name_span.clone()),
            "children": children,
        }))
    }
    json!(unit.file.top.iter().filter_map(|&t| symbol(unit, t, false)).collect::<Vec<_>>())
}

/// Declarations of the project whose names match `query`.
pub fn workspace_symbols(an: &Analyzer, query: &str) -> Value {
    let mut out = Vec::new();
    for (name, s) in &an.proj.names {
        if !query.is_empty() && !matches_prefix(name, query) && !name.to_lowercase().contains(&query.to_lowercase()) {
            continue;
        }
        let unit = an.unit(s.unit_id());
        let d = an.decl(*s);
        let file = &unit.file;
        let owner = file.owner_class(s.decl_id());
        let container = match owner {
            Some(c) => file.fq_name(c),
            None => file.package.clone(),
        };
        out.push(json!({
            "name": name,
            "kind": symbol_kind(d, owner.is_some()),
            "location": { "uri": text::path_to_uri(&unit.path), "range": text::range(&unit.text, d.name_span.clone()) },
            "containerName": container,
        }));
        if out.len() >= 500 {
            break;
        }
    }
    json!(out)
}
