//! KDoc comments: completion of the block tags after `@`, the parameters after `@param`, and
//! declarations in links (`[Name]`, `[Type.member]`) and after `@see`, `@throws`, `@exception`
//! and `@sample`; the declarations those names refer to (hover, go to definition, references);
//! the hints for names that resolve to nothing; the exceptions a function's body throws; and the
//! "Generate KDoc" action for a declaration without a comment.

use std::collections::HashSet;
use std::ops::Range;

use serde_json::{Value, json};

use super::actions::{Request, indent_at, line_start, text_edit, touches};
use super::diagnostics::{Problem, Severity};
use super::features::{self, Target};
use super::index::{SymId, Unit, UnitId};
use super::lexer::{HARD_KEYWORDS, Tok};
use super::refactor::REWRITE;
use super::rename::{DocRef, doc_refs, resolve_ref, strip_ticks};
use super::resolve::{Analyzer, Member, Subst, Ty};
use super::syntax::{Decl, DeclId, File, Kind};
use crate::text;

/// The block tags, with what follows them.
const TAGS: [(&str, &str); 12] = [
    ("param", "param ${1:name} ${2:description}"),
    ("return", "return ${1:description}"),
    ("constructor", "constructor ${1:description}"),
    ("receiver", "receiver ${1:description}"),
    ("property", "property ${1:name} ${2:description}"),
    ("throws", "throws ${1:Exception} ${2:description}"),
    ("exception", "exception ${1:Exception} ${2:description}"),
    ("sample", "sample ${1:qualified.name}"),
    ("see", "see ${1:Name}"),
    ("author", "author ${1:name}"),
    ("since", "since ${1:version}"),
    ("suppress", "suppress"),
];

/// Adds an item: its label, kind, detail and the range it replaces.
type Add<'a> = dyn FnMut(&str, u8, Option<String>, Range<usize>) + 'a;

/// What the text before the cursor asks for.
enum Wanted<'a> {
    Tag,
    /// A parameter, property or type parameter name of the documented declaration.
    Param,
    /// A declaration: the path typed so far, as `qualifier.partial`.
    Link(&'a str),
}

fn path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '`' | '$')
}

/// The completion items at `offset` when it is inside a KDoc comment; `None` outside one.
pub fn completion(an: &Analyzer, unit: UnitId, offset: usize, snippets: bool) -> Option<Value> {
    let u = an.unit(unit);
    let (di, doc) = u.file.decls.iter().enumerate().find_map(|(i, d)| {
        let doc = d.doc.as_ref()?;
        (doc.start < offset && offset <= doc.end && u.text[doc.clone()].starts_with("/**")).then_some((i, doc.clone()))
    })?;
    let line_start = u.text[..offset].rfind('\n').map_or(0, |i| i + 1).max(doc.start);
    let before = &u.text[line_start..offset];
    let content = before.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '/');
    let tagged = |tags: &[&'static str]| tags.iter().find_map(|t| content.strip_prefix(t));
    let wanted = if content.strip_prefix('@').is_some_and(|r| r.chars().all(char::is_alphanumeric)) {
        Wanted::Tag
    } else if tagged(&["@param", "@property"]).is_some_and(|a| a.trim_start_matches('[').chars().all(|c| path_char(c) || c.is_whitespace()))
    {
        Wanted::Param
    } else if let Some(arg) = tagged(&["@see ", "@throws ", "@exception ", "@sample "]).filter(|a| a.chars().all(path_char)) {
        Wanted::Link(arg)
    } else if let Some(open) = before.rfind('[').filter(|&i| before[i + 1..].chars().all(path_char)) {
        Wanted::Link(&before[open + 1..])
    } else {
        return Some(json!({ "isIncomplete": false, "items": [] }));
    };
    let d = &u.file.decls[di];
    let throwing = tagged(&["@throws ", "@exception "]).is_some();
    let mut items: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |label: &str, kind: u8, detail: Option<String>, range: Range<usize>| {
        if seen.insert(label.to_owned()) {
            let mut item = json!({ "label": label, "kind": kind, "textEdit": { "range": text::range(&u.text, range), "newText": label } });
            if let Some(detail) = detail {
                item["detail"] = json!(detail);
            }
            items.push(item);
        }
    };
    let own = |range: &Range<usize>, add: &mut Add| {
        for &p in d.params.iter().chain(d.members.iter()) {
            let pd = &u.file.decls[p];
            add(&pd.name, features::completion_kind(pd, true), None, range.clone());
        }
        for tp in &d.type_params {
            add(strip_ticks(&tp.name), 25, Some("type parameter".into()), range.clone());
        }
    };
    match wanted {
        Wanted::Tag => {
            let start = offset - content.len() + 1;
            let range = text::range(&u.text, start..offset);
            for (tag, snippet) in TAGS {
                let (new_text, format) = if snippets { (snippet, 2) } else { (tag, 1) };
                items.push(json!({
                    "label": format!("@{tag}"),
                    "kind": 14,
                    "filterText": tag,
                    "insertTextFormat": format,
                    "textEdit": { "range": range, "newText": new_text },
                }));
            }
        }
        Wanted::Param => {
            let word = &before[before.rfind(|c: char| !path_char(c)).map_or(0, |i| i + 1)..];
            own(&(offset - word.len()..offset), &mut add);
        }
        Wanted::Link(path) => {
            let (qualifier, partial) = path.rsplit_once('.').unwrap_or(("", path));
            let range = offset - partial.len()..offset;
            if qualifier.is_empty() {
                // The exceptions the body throws come first.
                if throwing {
                    for name in thrown(&u.file, &u.text, d) {
                        add(&name, 7, Some("thrown in the body".into()), range.clone());
                    }
                }
                own(&range, &mut add);
                // The enclosing classes' members, the file's declarations, the package's and the imports'.
                let mut owner = d.parent;
                while let Some(o) = owner {
                    for &m in &u.file.decls[o].members {
                        let md = &u.file.decls[m];
                        add(&md.name, features::completion_kind(md, true), None, range.clone());
                    }
                    owner = u.file.decls[o].parent;
                }
                for &t in &u.file.top {
                    let td = &u.file.decls[t];
                    add(&td.name, features::completion_kind(td, false), None, range.clone());
                }
                for sym in an.proj.by_package.get(&u.file.package).into_iter().flatten() {
                    let sd = an.decl(*sym);
                    add(&sd.name, features::completion_kind(sd, false), Some(an.fq(*sym)), range.clone());
                }
                for import in &u.file.imports {
                    if let Some(name) = import.name() {
                        add(name, 7, Some(import.fq()), range.clone());
                    }
                }
            } else {
                let segs: Vec<(String, Range<usize>)> = qualifier.split('.').map(|s| (strip_ticks(s).to_owned(), 0..0)).collect();
                let last = segs.len() - 1;
                let r = DocRef { segs, tag: false };
                for sym in resolve_ref(an, unit, di as DeclId, &r, last) {
                    let sd = an.decl(sym);
                    let types: Vec<Ty> = if sd.kind.is_class() || sd.kind == Kind::TypeAlias {
                        vec![an.instance(sym), Ty::Static(sym)]
                    } else {
                        an.value_ty(sym, &Subst::new()).into_iter().collect()
                    };
                    for ty in types {
                        for m in an.members(&ty, unit, false) {
                            let md = an.decl(m.sym);
                            if !md.generated {
                                add(&md.name, features::completion_kind(md, true), None, range.clone());
                            }
                        }
                    }
                }
                // A package: `[app.model.`.
                let prefix = format!("{qualifier}.");
                for (package, syms) in &an.proj.by_package {
                    if package == qualifier {
                        for sym in syms {
                            let sd = an.decl(*sym);
                            add(&sd.name, features::completion_kind(sd, false), None, range.clone());
                        }
                    } else if let Some(rest) = package.strip_prefix(&prefix) {
                        add(rest.split('.').next().unwrap_or(rest), 9, Some("package".into()), range.clone());
                    }
                }
            }
        }
    }
    Some(json!({ "isIncomplete": false, "items": items }))
}

/// The KDoc comment that `offset` is in, with the declaration it documents.
fn doc_at(u: &Unit, offset: usize) -> Option<(DeclId, Range<usize>)> {
    u.file.decls.iter().enumerate().find_map(|(i, d)| {
        let doc = d.doc.as_ref()?;
        (doc.start <= offset && offset <= doc.end).then(|| (i, doc.clone()))
    })
}

/// A name written in a KDoc comment (`a.B.c`), as a reference.
fn name_ref(name: &str) -> DocRef {
    DocRef { segs: name.split('.').map(|s| (strip_ticks(s).to_owned(), 0..0)).collect(), tag: false }
}

/// The package or declaration a reference's segments up to `upto` name, as a qualified name.
fn path_of(r: &DocRef, upto: usize) -> String {
    r.segs[..=upto].iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(".")
}

/// The declarations the name `name` in a comment of declaration `di` refers to. A name with
/// spaces (`@see` followed by text) refers to nothing.
fn named(an: &Analyzer, unit: UnitId, di: DeclId, name: &str) -> Vec<SymId> {
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Vec::new();
    }
    let r = name_ref(name);
    resolve_ref(an, unit, di, &r, r.segs.len() - 1)
}

/// The name under `offset` in a KDoc comment, its span, and what it refers to: a link's
/// declarations (`[Type.member]`), the declaration a `@param` names, or a package.
pub fn reference_at(an: &Analyzer, unit: UnitId, offset: usize) -> Option<(Range<usize>, Vec<Target>)> {
    let u = an.unit(unit);
    let (di, doc) = doc_at(u, offset)?;
    for r in doc_refs(&u.text, &doc) {
        for (j, (_, span)) in r.segs.iter().enumerate() {
            if span.start <= offset && offset <= span.end {
                let syms = resolve_ref(an, unit, di, &r, j);
                let targets = if !syms.is_empty() {
                    syms.into_iter().map(|sym| Target::Sym(Member { sym, subst: Subst::new() })).collect()
                } else {
                    let path = path_of(&r, j);
                    if !r.tag && an.has_package(&path) { vec![Target::Package(path)] } else { Vec::new() }
                };
                return Some((span.clone(), targets));
            }
        }
    }
    None
}

/// The declaration a name in a KDoc comment at `offset` refers to, for references and rename's
/// group of it.
pub fn group_at(an: &Analyzer, unit: UnitId, offset: usize) -> Option<Vec<SymId>> {
    let (_, targets) = reference_at(an, unit, offset)?;
    let sym = targets.iter().find_map(|t| t.sym())?;
    Some(features::rename_group(an, sym))
}

/// The KDoc of declaration `di` as Markdown: its `@see` names linked to their declarations, and
/// its `@sample` functions' bodies shown.
pub fn markdown(an: &Analyzer, unit: UnitId, di: DeclId, doc: &str) -> String {
    features::kdoc_to_markdown(doc, &|name| see_link(an, unit, di, name), &|name| sample_code(an, unit, di, name))
}

/// A `@see` name as a link to its declaration, or as code when it resolves to nothing.
fn see_link(an: &Analyzer, unit: UnitId, di: DeclId, name: &str) -> String {
    let Some(sym) = named(an, unit, di, name).into_iter().next() else {
        return if name.contains(char::is_whitespace) { name.to_owned() } else { format!("`{name}`") };
    };
    let target = an.unit(sym.unit_id());
    let line = text::position(&target.text, an.decl(sym).name_span.start)["line"].as_u64().unwrap_or(0);
    format!("[`{name}`]({}#L{})", text::path_to_uri(&target.path), line + 1)
}

/// The body of the function a `@sample` names, or `None` when it names nothing with a body.
fn sample_code(an: &Analyzer, unit: UnitId, di: DeclId, name: &str) -> Option<String> {
    let sym = named(an, unit, di, name).into_iter().next()?;
    body_text(an, sym.unit_id(), sym.decl_id())
}

/// The body of function `di` of `unit`, dedented, without the braces.
fn body_text(an: &Analyzer, unit: UnitId, di: DeclId) -> Option<String> {
    let u = an.unit(unit);
    let d = &u.file.decls[di];
    if d.kind != Kind::Function {
        return None;
    }
    let tokens = body_tokens(&u.file, d);
    if tokens.is_empty() {
        return None;
    }
    let lx = &u.file.lexed;
    let first = lx.tokens[tokens.start].start;
    // The first line keeps its indentation.
    let line = line_start(&u.text, first);
    let from = if u.text[line..first].trim().is_empty() { line } else { first };
    Some(dedent(&u.text[from..lx.tokens[tokens.end - 1].end]))
}

/// `code` without the blank lines around it and the indentation all its lines share.
fn dedent(code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut out: Vec<&str> = lines.iter().map(|l| l.get(indent..).unwrap_or_default().trim_end()).collect();
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// The tokens of a function's body: those of its expression body, or those between its braces.
fn body_tokens(file: &File, d: &Decl) -> Range<usize> {
    let lx = &file.lexed;
    let Some(last) = lx.before(d.span.end) else { return 0..0 };
    match d.init {
        Some(init) if init <= last => return init..last + 1,
        _ => {}
    }
    match lx.matching.get(last).copied().flatten() {
        Some(open) if lx.kind(last) == Some(Tok::RBrace) && open < last => open + 1..last,
        _ => 0..0,
    }
}

/// The exceptions a function's body throws (`throw Name(...)`), as written, in order.
pub(super) fn thrown(file: &File, src: &str, d: &Decl) -> Vec<String> {
    let lx = &file.lexed;
    let mut out: Vec<String> = Vec::new();
    if !matches!(d.kind, Kind::Function | Kind::Constructor) {
        return out;
    }
    for i in body_tokens(file, d) {
        if !lx.is(src, i, "throw") {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while lx.kind(j) == Some(Tok::Ident) {
            path.push(lx.text(src, j));
            j += 1;
            if lx.kind(j) != Some(Tok::Dot) {
                break;
            }
            j += 1;
        }
        // `throw if (...)` and the like are keywords, not exceptions.
        let keyword = path.len() == 1 && HARD_KEYWORDS.contains(&path[0]);
        if !path.is_empty() && !keyword && lx.kind(j) == Some(Tok::LParen) {
            let name = path.join(".");
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Hints about the KDoc comments of a Kotlin file, for the names that resolve to nothing: a
/// `@param` or `@property` name that is not a parameter, property or type parameter of its
/// declaration, and a link or a `@see`, `@throws` or `@sample` name that resolves to no
/// declaration or package. They are hints, not errors: the compiler decides what is wrong.
pub fn problems(an: &Analyzer, unit: UnitId) -> Vec<Problem> {
    let u = an.unit(unit);
    let file = &u.file;
    let mut out = Vec::new();
    for (di, d) in file.decls.iter().enumerate() {
        let Some(doc) = &d.doc else { continue };
        for r in doc_refs(&u.text, doc) {
            let last = r.segs.len() - 1;
            let span = r.segs[0].1.start..r.segs[last].1.end;
            if r.tag {
                if resolve_ref(an, unit, di, &r, 0).is_empty() && !is_type_param(d, &r.segs[0].0) {
                    let name = &r.segs[0].0;
                    out.push(Problem::new(span, Severity::Hint, format!("`{name}` is not a parameter or property of this declaration")));
                }
                continue;
            }
            let path = path_of(&r, last);
            let known = !resolve_ref(an, unit, di, &r, last).is_empty()
                || an.has_package(&path)
                || (r.segs.len() == 1 && is_type_param(d, &r.segs[0].0));
            if !known {
                out.push(Problem::new(span, Severity::Hint, format!("unresolved reference `{path}`")));
            }
        }
    }
    out
}

fn is_type_param(d: &Decl, name: &str) -> bool {
    d.type_params.iter().any(|p| strip_ticks(&p.name) == name)
}

/// "Generate KDoc" on a function, constructor or class without a comment, on its first line: a
/// `@param` for each parameter, a `@return` for a function that doesn't return `Unit`, and a
/// `@throws` for each exception its body throws.
pub(super) fn generate_actions(an: &Analyzer, req: &Request, lines: &Range<usize>, out: &mut Vec<Value>) {
    let unit = an.unit(req.unit);
    let (file, src) = (&unit.file, &unit.text);
    for d in &file.decls {
        if d.doc.is_some()
            || d.generated
            || !matches!(d.kind, Kind::Function | Kind::Constructor | Kind::Class)
            || !touches(lines, &(d.span.start..d.name_span.end))
        {
            continue;
        }
        let at = line_start(src, d.span.start);
        if !src[at..d.span.start].trim().is_empty() {
            continue;
        }
        let mut tags: Vec<String> = d.params.iter().map(|&p| format!("@param {}", file.decls[p].name)).collect();
        let returns = match &d.ty {
            Some(ty) => ty.name != "Unit",
            None => d.init.is_some(),
        };
        if d.kind == Kind::Function && returns {
            tags.push("@return".into());
        }
        tags.extend(thrown(file, src, d).into_iter().map(|name| format!("@throws {name}")));
        if tags.is_empty() {
            continue;
        }
        let indent = indent_at(src, d.span.start);
        let mut comment = format!("{indent}/**\n");
        for tag in tags {
            comment.push_str(&format!("{indent} * {tag}\n"));
        }
        comment.push_str(&format!("{indent} */\n"));
        let uri = text::path_to_uri(&unit.path);
        out.push(json!({
            "title": "Generate KDoc",
            "kind": REWRITE,
            "edit": { "changes": { uri: [text_edit(src, at..at, comment)] } },
        }));
    }
}
