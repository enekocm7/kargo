//! Completion inside KDoc comments: the block tags after `@`, the parameters after `@param`, and
//! declarations in links (`[Name]`, `[Type.member]`) and after `@see`, `@throws`, `@exception`
//! and `@sample`.

use std::collections::HashSet;
use std::ops::Range;

use serde_json::{Value, json};

use super::features;
use super::index::UnitId;
use super::rename::{DocRef, resolve_ref, strip_ticks};
use super::resolve::{Analyzer, Subst, Ty};
use super::syntax::{DeclId, Kind};
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
