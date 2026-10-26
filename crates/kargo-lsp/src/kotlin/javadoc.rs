//! Documentation for libraries without sources, read from the Javadoc jar (`-javadoc.jar`)
//! published next to them. [`super::compiled`] writes it into the stubs it generates as doc
//! comments, so hover and completion show it like the documentation of a sources jar.
//!
//! Two layouts are read:
//! - the standard doclet's (JDK 8 to 21, and Dokka's `javadoc` format, which copies it): a page
//!   per class (`com/example/Outer.Inner.html`), each member under an anchor named after its
//!   erased signature (`greet(java.lang.String)`, or `greet-java.lang.String-` before JDK 9), with
//!   its description in a `block` and its tags in a `<dl>`;
//! - Dokka's HTML format: a page per class and per member, each naming what it documents in its
//!   `pageIds` (`module::com.example/Greeter/greet/#kotlin.Int/PointingToDeclaration/`), with the
//!   tags under `<h4>` headings and `@sample` code in `<pre>` blocks.
//!
//! Members are matched to compiled ones by name and parameter types (simple names, compared
//! without case so Java's `int` matches Kotlin's `Int`), and give Java methods compiled without
//! `-parameters` their real parameter names.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;

/// The documentation of one declaration, in Markdown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Doc {
    pub text: String,
    /// Parameter names and descriptions.
    pub params: Vec<(String, String)>,
    pub returns: Option<String>,
    /// Exception names and descriptions.
    pub throws: Vec<(String, String)>,
    pub see: Vec<String>,
    /// Code of `@sample`s.
    pub samples: Vec<String>,
}

impl Doc {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.params.is_empty()
            && self.returns.is_none()
            && self.throws.is_empty()
            && self.see.is_empty()
            && self.samples.is_empty()
    }

    /// The doc as the text of a `/** */` comment: Markdown, the samples' code, then `@param`,
    /// `@return`, `@throws` and `@see` lines (each on one line, as the KDoc reader wants them).
    pub fn body(&self) -> String {
        // A line of text starting with `@` would read as a tag.
        let mut lines: Vec<String> = self.text.lines().map(|l| if l.starts_with('@') { format!(" {l}") } else { l.to_owned() }).collect();
        for sample in &self.samples {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push("```kotlin".to_owned());
            lines.extend(sample.lines().map(str::to_owned));
            lines.push("```".to_owned());
        }
        let mut tags: Vec<String> = Vec::new();
        for (name, desc) in &self.params {
            tags.push(format!("@param {name} {}", flat(desc)));
        }
        if let Some(returns) = &self.returns {
            tags.push(format!("@return {}", flat(returns)));
        }
        for (name, desc) in &self.throws {
            tags.push(format!("@throws {name} {}", flat(desc)));
        }
        for see in &self.see {
            tags.push(format!("@see {}", flat(see)));
        }
        if !lines.is_empty() && !tags.is_empty() {
            lines.push(String::new());
        }
        lines.extend(tags.into_iter().map(|t| t.trim_end().to_owned()));
        lines.join("\n")
    }
}

/// `body` as a doc comment at `indent`. In a Java file (`java`) the text is HTML, as Javadoc is,
/// so `<`, `>` and `&` are written as entities.
pub fn comment(body: &str, indent: &str, java: bool) -> String {
    let mut out = format!("{indent}/**\n");
    for line in body.lines() {
        let line = if java { line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") } else { line.to_owned() };
        let line = line.replace("*/", "* /");
        if line.trim().is_empty() {
            out.push_str(&format!("{indent} *\n"));
        } else {
            out.push_str(&format!("{indent} * {line}\n"));
        }
    }
    out.push_str(&format!("{indent} */\n"));
    out
}

/// One line, its whitespace collapsed.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A documented member of a class (or a top-level declaration of a package).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its name; `<init>` for a constructor.
    pub name: String,
    /// The simple names of its parameter types (`String`, `int`, `String[]`), with the receiver
    /// first for a Kotlin extension. Empty for a field or property.
    pub types: Vec<String>,
    /// Its parameter names, when the page shows them.
    pub names: Vec<String>,
    /// A field or property rather than a method.
    pub field: bool,
    pub doc: Doc,
}

/// What a page documents: a class, or members of one.
#[derive(Debug, Default)]
struct Page {
    doc: Option<Doc>,
    members: Vec<Member>,
}

/// Where a page's documentation belongs: a class (`com.example.Outer.Inner`) or the top level
/// of a package.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Owner {
    Class(String),
    Package(String),
}

/// The documentation in a Javadoc jar.
#[derive(Debug, Default)]
pub struct Docs {
    classes: HashMap<String, Page>,
    packages: HashMap<String, Page>,
}

impl Docs {
    /// Reads the HTML pages of the Javadoc jar `jar`.
    pub fn read(jar: &Path) -> Result<Docs> {
        let file = std::fs::File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?;
        let mut zip = zip::ZipArchive::new(file).with_context(|| format!("failed to read {}", jar.display()))?;
        let mut pages = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
            if !name.ends_with(".html") {
                continue;
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            pages.push((name, String::from_utf8_lossy(&bytes).into_owned()));
        }
        Ok(Docs::from_pages(&pages))
    }

    /// The documentation of `pages`: path in the jar and HTML.
    pub fn from_pages(pages: &[(String, String)]) -> Docs {
        let parsed: Vec<(Owner, Page)> = pages.par_iter().filter_map(|(path, html)| parse_page(path, html)).collect();
        let mut docs = Docs::default();
        for (owner, page) in parsed {
            let into = match owner {
                Owner::Class(fq) => docs.classes.entry(fq).or_default(),
                Owner::Package(package) => docs.packages.entry(package).or_default(),
            };
            if into.doc.is_none() {
                into.doc = page.doc;
            }
            into.members.extend(page.members);
        }
        docs
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.packages.is_empty()
    }

    /// The documentation of class `fq` (`com.example.Outer.Inner`).
    pub fn class(&self, fq: &str) -> Option<&Doc> {
        self.classes.get(fq)?.doc.as_ref().filter(|d| !d.is_empty())
    }

    /// The method or constructor (`<init>`) `name` of class `fq` whose parameters are `types`
    /// (simple names, `None` where unknown).
    pub fn method(&self, fq: &str, name: &str, types: &[Option<String>]) -> Option<&Member> {
        pick(&self.classes.get(fq)?.members, name, types, false)
    }

    /// The field or property `name` of class `fq`, or its getter.
    pub fn field(&self, fq: &str, name: &str) -> Option<&Doc> {
        let members = &self.classes.get(fq)?.members;
        pick(members, name, &[], true).or_else(|| getters(name).iter().find_map(|g| pick(members, g, &[], false))).map(|m| &m.doc)
    }

    /// The top-level function `name` of `package`: from Dokka's page of the package, or from
    /// the Javadoc of a file facade class (`UtilsKt`).
    pub fn top_function(&self, package: &str, name: &str, types: &[Option<String>]) -> Option<&Member> {
        if let Some(found) = self.packages.get(package).and_then(|p| pick(&p.members, name, types, false)) {
            return Some(found);
        }
        self.facades(package).into_iter().find_map(|p| pick(&p.members, name, types, false))
    }

    /// The top-level property `name` of `package`, or its getter.
    pub fn top_property(&self, package: &str, name: &str) -> Option<&Doc> {
        if let Some(found) = self.packages.get(package).and_then(|p| pick(&p.members, name, &[], true)) {
            return Some(&found.doc);
        }
        self.facades(package)
            .into_iter()
            .find_map(|p| pick(&p.members, name, &[], true).or_else(|| getters(name).iter().find_map(|g| pick(&p.members, g, &[], false))))
            .map(|m| &m.doc)
    }

    /// The file facade classes of `package` (`UtilsKt`), which hold its top-level declarations
    /// in Javadoc.
    fn facades(&self, package: &str) -> Vec<&Page> {
        let mut facades: Vec<(&String, &Page)> = self
            .classes
            .iter()
            .filter(|(fq, _)| {
                let (pkg, class) = fq.rsplit_once('.').unwrap_or(("", fq.as_str()));
                pkg == package && class.ends_with("Kt")
            })
            .collect();
        facades.sort_by(|a, b| a.0.cmp(b.0));
        facades.into_iter().map(|(_, p)| p).collect()
    }
}

/// The getters of property `name`: `getName`, or `isName` itself.
fn getters(name: &str) -> Vec<String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return Vec::new() };
    let mut out = vec![format!("get{}{}", first.to_uppercase(), chars.as_str())];
    if name.starts_with("is") && name[2..].starts_with(|c: char| c.is_uppercase()) {
        out.push(name.to_owned());
    }
    out
}

/// The member of `members` named `name` with parameters `types`: the only one with that name and
/// number of parameters, or else the first whose known types match.
fn pick<'a>(members: &'a [Member], name: &str, types: &[Option<String>], field: bool) -> Option<&'a Member> {
    let same: Vec<&Member> =
        members.iter().filter(|m| m.name == name && m.field == field && (field || m.types.len() == types.len())).collect();
    if same.len() <= 1 {
        return same.first().copied().filter(|m| !m.doc.is_empty() || !m.names.is_empty());
    }
    same.into_iter().find(|m| {
        m.types.iter().zip(types).all(|(documented, compiled)| compiled.as_ref().is_none_or(|c| documented.eq_ignore_ascii_case(c)))
    })
}

/// The simple name of a type as a page writes it: `java.util.List<T>` is `List`, `int[]` stays
/// `int[]`, `java.lang.String...` is `String[]`, `kotlin.collections.List[kotlin.String]?` is `List`.
pub fn simple_type(ty: &str) -> String {
    let mut ty = ty.trim().trim_end_matches('?').to_owned();
    let mut dims = String::new();
    loop {
        if let Some(rest) = ty.strip_suffix("[]") {
            dims.push_str("[]");
            ty = rest.trim_end().to_owned();
        } else if let Some(rest) = ty.strip_suffix("...") {
            dims.push_str("[]");
            ty = rest.trim_end().to_owned();
        } else {
            break;
        }
    }
    // Type arguments: `<...>` in Java, `[...]` in Dokka's ids.
    let base = ty.split(['<', '[']).next().unwrap_or(&ty).trim();
    let simple = base.rsplit(['.', '$', '/']).next().unwrap_or(base);
    format!("{simple}{dims}")
}

/// The simple name of a JVM field descriptor's type (`Ljava/util/Map$Entry;` is `Entry`, `[I` is
/// `int[]`), as [`simple_type`] gives it.
pub fn descriptor_type(desc: &str) -> String {
    let dims = desc.chars().take_while(|&c| c == '[').count();
    let base = &desc[dims..];
    let name = match base {
        "I" => "int",
        "J" => "long",
        "Z" => "boolean",
        "B" => "byte",
        "S" => "short",
        "C" => "char",
        "F" => "float",
        "D" => "double",
        "V" => "void",
        _ => base.trim_start_matches('L').trim_end_matches(';').rsplit(['/', '$']).next().unwrap_or(base),
    };
    format!("{name}{}", "[]".repeat(dims))
}

// ----- HTML -----

/// A tag of an HTML text.
struct Tag<'a> {
    start: usize,
    end: usize,
    /// Its name in lower case.
    name: String,
    closing: bool,
    attrs: &'a str,
}

impl Tag<'_> {
    fn attr(&self, key: &str) -> Option<String> {
        attr(self.attrs, key)
    }

    fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_whitespace().any(|c| c == class))
    }

    fn is(&self, name: &str) -> bool {
        !self.closing && self.name == name
    }
}

/// The first tag at or after `from`.
fn next_tag(html: &str, from: usize) -> Option<Tag<'_>> {
    let mut i = from;
    loop {
        let at = i + html.get(i..)?.find('<')?;
        if html[at..].starts_with("<!--") {
            i = html[at..].find("-->").map_or(html.len(), |e| at + e + 3);
            continue;
        }
        let end = at + html[at..].find('>')? + 1;
        let inner = &html[at + 1..end - 1];
        let (closing, body) = inner.strip_prefix('/').map_or((false, inner), |b| (true, b));
        let len = body.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(body.len());
        if len == 0 {
            // `<!DOCTYPE>` or a stray `<`.
            i = at + 1;
            continue;
        }
        return Some(Tag { start: at, end, name: body[..len].to_ascii_lowercase(), closing, attrs: &body[len..] });
    }
}

/// The tags of `html[from..to]`.
fn tags(html: &str, from: usize, to: usize) -> impl Iterator<Item = Tag<'_>> {
    let mut pos = from;
    std::iter::from_fn(move || {
        let tag = next_tag(html, pos).filter(|t| t.start < to)?;
        pos = tag.end;
        Some(tag)
    })
}

/// The value of attribute `key` (any case) in a tag's attribute text, entities decoded.
fn attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let key = key.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(&key) {
        let at = from + found;
        from = at + key.len();
        let before = lower[..at].chars().next_back();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = attrs[at + key.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        let value = if quote == '"' || quote == '\'' {
            let body = &rest[1..];
            &body[..body.find(quote).unwrap_or(body.len())]
        } else {
            &rest[..rest.find(char::is_whitespace).unwrap_or(rest.len())]
        };
        return Some(decode(value));
    }
    None
}

/// Where the element that `open` starts ends: the start of its closing tag, and the end of it.
fn element_end(html: &str, open: &Tag) -> (usize, usize) {
    let mut depth = 1;
    for tag in tags(html, open.end, html.len()) {
        if tag.name != open.name {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return (tag.start, tag.end);
            }
        } else {
            depth += 1;
        }
    }
    (html.len(), html.len())
}

/// The inner HTML of the element that `open` starts.
fn inner<'a>(html: &'a str, open: &Tag) -> &'a str {
    &html[open.end..element_end(html, open).0]
}

/// The first element in `html[from..to]` for which `want` holds, and its inner HTML.
fn find<'a>(html: &'a str, from: usize, to: usize, want: impl Fn(&Tag) -> bool) -> Option<(Tag<'a>, &'a str)> {
    let tag = tags(html, from, to).find(|t| !t.closing && want(t))?;
    let text = inner(html, &tag);
    Some((tag, text))
}

/// Decodes the character references of HTML text; zero-width spaces are dropped.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &after[..e];
            let c = match name {
                "nbsp" => ' ',
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                n if n.starts_with("#x") || n.starts_with("#X") => char::from_u32(u32::from_str_radix(&n[2..], 16).ok()?)?,
                n if n.starts_with('#') => char::from_u32(n[1..].parse().ok()?)?,
                _ => return None,
            };
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                if c != '\u{200B}' {
                    out.push(if c == '\u{A0}' { ' ' } else { c });
                }
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The classes of elements that are page chrome, not documentation: Dokka's copy buttons.
const CHROME: &[&str] = &["top-right-position", "copy-popup-wrapper", "copy-icon"];

/// The text of `html` with its tags dropped and its entities decoded; `collapse` makes each run
/// of whitespace one space, as a browser shows it.
fn text(html: &str, collapse: bool) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some(tag) = next_tag(html, pos) {
        out.push_str(&decode(&html[pos..tag.start]));
        pos = if !tag.closing && (CHROME.iter().any(|c| tag.has_class(c)) || matches!(tag.name.as_str(), "script" | "style")) {
            element_end(html, &tag).1
        } else {
            tag.end
        };
    }
    out.push_str(&decode(&html[pos..]));
    if collapse { flat(&out) } else { out }
}

/// HTML documentation as Markdown: paragraphs, lists, code spans and blocks, emphasis. Links
/// keep their text.
pub fn markdown(html: &str) -> String {
    let mut out = String::new();
    let mut pre = 0usize;
    let mut code = 0usize;
    let mut pos = 0;
    let push_text = |out: &mut String, raw: &str, pre: usize| {
        let decoded = decode(raw);
        if pre > 0 {
            out.push_str(&decoded);
            return;
        }
        let mut last_space = out.ends_with([' ', '\n']) || out.is_empty();
        for c in decoded.chars() {
            if c.is_whitespace() {
                if !last_space {
                    out.push(' ');
                    last_space = true;
                }
            } else {
                out.push(c);
                last_space = false;
            }
        }
    };
    while let Some(tag) = next_tag(html, pos) {
        push_text(&mut out, &html[pos..tag.start], pre);
        pos = tag.end;
        if !tag.closing && (CHROME.iter().any(|c| tag.has_class(c)) || matches!(tag.name.as_str(), "script" | "style" | "button")) {
            pos = element_end(html, &tag).1;
            continue;
        }
        if tag.name == "pre" {
            if tag.closing {
                pre = pre.saturating_sub(1);
                if pre == 0 {
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    out.push_str("\n```\n\n");
                }
            } else {
                if pre == 0 {
                    // `<pre><code class="lang-kotlin">` names its language.
                    let lang = [Some(&tag), next_tag(html, tag.end).filter(|t| t.is("code")).as_ref()]
                        .into_iter()
                        .flatten()
                        .find_map(|t| {
                            t.attr("class")?
                                .split_whitespace()
                                .find_map(|c| c.strip_prefix("lang-").or_else(|| c.strip_prefix("language-")).map(str::to_owned))
                        })
                        .unwrap_or_default();
                    out.push_str(&format!("\n\n```{lang}\n"));
                    // The newline right after `<pre>` isn't part of the code.
                    let rest = &html[pos..];
                    pos += rest.len() - rest.trim_start_matches(['\r', '\n']).len();
                }
                pre += 1;
            }
            continue;
        }
        if pre > 0 {
            continue;
        }
        let closing = tag.closing;
        match tag.name.as_str() {
            "p" | "div" | "section" | "table" => out.push_str("\n\n"),
            "br" | "ul" | "ol" | "dl" | "tr" | "dt" | "dd" => out.push('\n'),
            "li" if !closing => out.push_str("\n- "),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => out.push_str(if closing { "**\n\n" } else { "\n\n**" }),
            "code" | "tt" | "kbd" | "samp" => {
                if closing {
                    code = code.saturating_sub(1);
                    if code == 0 {
                        out.push('`');
                    }
                } else {
                    if code == 0 {
                        out.push('`');
                    }
                    code += 1;
                }
            }
            "em" | "i" | "var" => out.push('*'),
            "b" | "strong" => out.push_str("**"),
            "td" | "th" => out.push(' '),
            _ => {}
        }
    }
    push_text(&mut out, &html[pos..], pre);
    tidy(&out)
}

/// Trims the lines outside code blocks and keeps at most one blank line between paragraphs.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            lines.push(line.trim().to_owned());
            continue;
        }
        if fence {
            lines.push(line.trim_end().to_owned());
            continue;
        }
        let line = line.trim();
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line.to_owned());
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Splits `` `name` - description `` (a parameter or exception in Javadoc's tags) in two.
fn name_and_description(markdown: &str) -> (String, String) {
    let md = markdown.trim();
    if let Some(rest) = md.strip_prefix('`')
        && let Some(close) = rest.find('`')
    {
        let desc = rest[close + 1..].trim_start();
        let desc = desc.strip_prefix('-').unwrap_or(desc).trim();
        return (rest[..close].trim().to_owned(), desc.to_owned());
    }
    match md.split_once(" - ") {
        Some((name, desc)) => (name.trim().to_owned(), desc.trim().to_owned()),
        None => {
            let (name, desc) = md.split_once(char::is_whitespace).unwrap_or((md, ""));
            (name.to_owned(), desc.trim().to_owned())
        }
    }
}

// ----- pages -----

fn parse_page(path: &str, html: &str) -> Option<(Owner, Page)> {
    if html.contains("pageIds=") || html.contains("pageids=") { dokka_page(html) } else { javadoc_page(path, html) }
}

/// The package and class (`Outer.Inner`) a standard doclet page documents, from its path in the
/// jar. A module's directory (`java.base/`) has a dot, which no package segment has.
fn class_of_path(path: &str) -> Option<(String, String)> {
    let path = path.trim_start_matches("./");
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.strip_suffix(".html")?;
    if stem.contains('-') || !stem.starts_with(|c: char| c.is_alphabetic() || c == '_') {
        return None;
    }
    const NOT_CLASSES: &[&str] = &["index", "allclasses", "allpackages", "overview", "search", "help", "deprecated", "constant"];
    if NOT_CLASSES.iter().any(|n| stem.eq_ignore_ascii_case(n)) {
        return None;
    }
    let segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    if segments.iter().any(|s| matches!(*s, "class-use" | "doc-files" | "src-html" | "index-files" | "resources" | "legal")) {
        return None;
    }
    let package: Vec<&str> = segments.iter().copied().skip_while(|s| s.contains('.')).collect();
    Some((package.join("."), stem.to_owned()))
}

/// The member an anchor of a standard doclet page names: `name(types)` (JDK 9 and later) or
/// `name-types-` (JDK 8, arrays as `:A`) for a method or constructor, the bare name for a field.
fn parse_anchor(id: &str) -> Option<(String, Option<Vec<String>>)> {
    if let Some(open) = id.find('(') {
        let close = id.rfind(')')?;
        let inner = id[open + 1..close].trim();
        let types = if inner.is_empty() { Vec::new() } else { inner.split(',').map(|t| t.trim().to_owned()).collect() };
        return Some((id[..open].to_owned(), Some(types)));
    }
    if id.len() > 1
        && id.ends_with('-')
        && let Some(dash) = id.find('-')
    {
        let inner = &id[dash + 1..id.len() - 1];
        let types = if inner.is_empty() { Vec::new() } else { inner.split('-').map(|t| t.replace(":A", "[]")).collect() };
        return Some((id[..dash].to_owned(), Some(types)));
    }
    let mut chars = id.chars();
    let identifier = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    identifier.then(|| (id.to_owned(), None))
}

/// The parameters in a signature's text, after `name(`: `int value` in Java, `times: Int = 1` in Kotlin.
fn signature_params(signature: &str, name: &str) -> Option<Vec<String>> {
    let mut from = 0;
    let open = loop {
        let at = from + signature[from..].find(&format!("{name}("))?;
        let before = signature[..at].chars().next_back();
        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            break at + name.len();
        }
        from = at + 1;
    };
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = open + 1;
    for (i, c) in signature[open..].char_indices() {
        let i = open + i;
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => {
                depth -= 1;
                if depth == 0 {
                    parts.push(&signature[start..i]);
                    break;
                }
            }
            ',' if depth == 1 => {
                parts.push(&signature[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if parts.len() == 1 && parts[0].trim().is_empty() {
        return Some(Vec::new());
    }
    Some(parts.into_iter().map(|p| p.trim().to_owned()).collect())
}

/// The tags of a standard doclet `<dl>`: parameters, return value, exceptions and see-also links.
fn javadoc_tags(html: &str, doc: &mut Doc) {
    let mut label = String::new();
    let mut pos = 0;
    while let Some((tag, body)) = find(html, pos, html.len(), |t| t.name == "dt" || t.name == "dd") {
        pos = element_end(html, &tag).1;
        if tag.name == "dt" {
            label = text(body, true).trim_end_matches(':').trim().to_ascii_lowercase();
            continue;
        }
        match label.as_str() {
            "parameters" => {
                let (name, desc) = name_and_description(&markdown(body));
                doc.params.push((name, desc));
            }
            "returns" => doc.returns = Some(markdown(body)),
            "throws" => {
                let (name, desc) = name_and_description(&markdown(body));
                doc.throws.push((name, desc));
            }
            "see also" => {
                let links: Vec<String> = tags(body, 0, body.len())
                    .filter(|t| t.is("a"))
                    .map(|t| text(inner(body, &t), true))
                    .filter(|t| !t.is_empty())
                    .collect();
                if links.is_empty() {
                    doc.see.push(text(body, true));
                } else {
                    doc.see.extend(links);
                }
            }
            _ => {}
        }
    }
}

/// A class page of the standard doclet: the class's description, then its members' details.
fn javadoc_page(path: &str, html: &str) -> Option<(Owner, Page)> {
    let (package, class) = class_of_path(path)?;
    let fq = if package.is_empty() { class.clone() } else { format!("{package}.{class}") };
    let simple = class.rsplit('.').next().unwrap_or(&class).to_owned();
    let anchors: Vec<(usize, String)> = tags(html, 0, html.len())
        .filter(|t| !t.closing)
        .filter_map(|t| Some((t.start, t.attr("id").or_else(|| t.attr("name").filter(|_| t.name == "a"))?)))
        .collect();
    let mut page = Page::default();

    // The class's description comes before the summaries.
    let summary = anchors.iter().find(|(_, id)| id.ends_with("summary")).map_or(html.len(), |(at, _)| *at);
    if let Some((block, body)) = find(html, 0, summary, |t| t.name == "div" && t.has_class("block")) {
        let mut doc = Doc { text: markdown(body), ..Doc::default() };
        let after = element_end(html, &block).1;
        if let Some((_, dl)) = find(html, after, summary, |t| t.name == "dl") {
            javadoc_tags(dl, &mut doc);
        }
        page.doc = Some(doc);
    }

    for (i, (at, id)) in anchors.iter().enumerate() {
        let Some((name, types)) = parse_anchor(id) else { continue };
        let end = anchors.get(i + 1).map_or(html.len(), |(next, _)| *next);
        // A detail starts with a heading of the member's name; any other anchor is page structure.
        let Some((_, heading)) = find(html, *at, end, |t| t.name == "h3" || t.name == "h4") else { continue };
        let heading = text(heading, true);
        let constructor = name == "<init>" || name == simple;
        if heading != name && !(constructor && heading == simple) {
            continue;
        }
        let mut doc = Doc::default();
        let block = find(html, *at, end, |t| t.name == "div" && t.has_class("block"));
        let signature = find(html, *at, block.as_ref().map_or(end, |(b, _)| b.start), |t| {
            t.name == "pre" || (t.name == "div" && t.has_class("member-signature"))
        });
        let mut after = *at;
        if let Some((tag, body)) = &block {
            doc.text = markdown(body);
            after = element_end(html, tag).1;
        }
        if let Some((_, dl)) = find(html, after, end, |t| t.name == "dl") {
            javadoc_tags(dl, &mut doc);
        }
        let params = signature.and_then(|(_, s)| signature_params(&text(s, true), &heading)).unwrap_or_default();
        let member = match types {
            None => Member { name, types: Vec::new(), names: Vec::new(), field: true, doc },
            Some(types) => Member {
                name: if constructor { "<init>".to_owned() } else { name },
                // `java.util.List<T> items`: the name is the last word.
                names: if params.len() == types.len() {
                    params.iter().map(|p| p.rsplit(char::is_whitespace).next().unwrap_or(p).to_owned()).collect()
                } else {
                    Vec::new()
                },
                types: types.iter().map(|t| simple_type(t)).collect(),
                field: false,
                doc,
            },
        };
        page.members.push(member);
    }
    (page.doc.is_some() || !page.members.is_empty()).then_some((Owner::Class(fq), page))
}

/// A page of Dokka's HTML format: a class (its cover) or a member (each overload's signature and
/// documentation), named by its `pageIds`.
fn dokka_page(html: &str) -> Option<(Owner, Page)> {
    let (main, body) = find(html, 0, html.len(), |t| t.attr("pageIds").is_some())?;
    let ids = main.attr("pageIds")?;
    let dri = ids.split_once("::").map_or(ids.as_str(), |(_, rest)| rest);
    let mut parts = dri.split('/');
    let package = parts.next().unwrap_or_default();
    let package = if package == "[root]" { "" } else { package };
    let classes = parts.next().unwrap_or_default();
    let callable = parts.next().unwrap_or_default();
    let fq = if package.is_empty() { classes.to_owned() } else { format!("{package}.{classes}") };
    let owner = if classes.is_empty() { Owner::Package(package.to_owned()) } else { Owner::Class(fq) };
    let mut page = Page::default();
    if callable.is_empty() {
        if classes.is_empty() {
            return None;
        }
        // The class's own documentation is in its cover; its members' summaries follow it.
        let (_, cover) = find(body, 0, body.len(), |t| t.name == "div" && t.has_class("cover"))?;
        page.doc = overloads(cover).into_iter().next().map(|(_, chunk)| dokka_doc(chunk));
        return Some((owner, page));
    }
    let simple = classes.rsplit('.').next().unwrap_or(classes);
    let constructor = !classes.is_empty() && callable == simple;
    for (signature, chunk) in overloads(body) {
        let doc = dokka_doc(chunk);
        let params = signature_params(&signature, if constructor { "constructor" } else { callable });
        let member = match params {
            None => Member { name: callable.to_owned(), types: Vec::new(), names: Vec::new(), field: true, doc },
            Some(params) => {
                // An extension's receiver comes first, as on the JVM: `fun String.shout()`.
                let head = &signature[..signature.find(&format!("{callable}(")).unwrap_or(0)];
                let receiver = head.trim_end().strip_suffix('.').map(|r| {
                    let r = r.rsplit(char::is_whitespace).next().unwrap_or(r);
                    simple_type(r.rsplit_once('>').map_or(r, |(_, after)| after))
                });
                let mut types: Vec<String> = receiver.into_iter().collect();
                let mut names = Vec::new();
                for param in params {
                    // `times: Int = 1` and `vararg names: String`.
                    let param = param.trim();
                    let (name, ty) = param.split_once(':').unwrap_or((param, ""));
                    let ty = ty.split('=').next().unwrap_or(ty);
                    let vararg = name.split_whitespace().any(|w| w == "vararg");
                    names.push(name.split_whitespace().last().unwrap_or(name).to_owned());
                    types.push(format!("{}{}", simple_type(ty), if vararg { "[]" } else { "" }));
                }
                Member { name: if constructor { "<init>".to_owned() } else { callable.to_owned() }, types, names, field: false, doc }
            }
        };
        page.members.push(member);
    }
    (!page.members.is_empty()).then_some((owner, page))
}

/// The overloads on a Dokka page: each signature (`symbol` block) as text, and the HTML up to the
/// next one.
fn overloads(html: &str) -> Vec<(String, &str)> {
    let mut symbols = Vec::new();
    let mut pos = 0;
    while let Some((tag, body)) = find(html, pos, html.len(), |t| t.name == "div" && t.has_class("symbol")) {
        let end = element_end(html, &tag).1;
        symbols.push((tag.start, end, text(body, true)));
        pos = end;
    }
    (0..symbols.len())
        .map(|i| {
            let to = symbols.get(i + 1).map_or(html.len(), |s| s.0);
            (symbols[i].2.clone(), &html[symbols[i].1..to])
        })
        .collect()
}

/// The documentation after a signature on a Dokka page: the description, then a section per tag
/// under an `<h4>` heading.
fn dokka_doc(html: &str) -> Doc {
    let headings: Vec<(usize, usize, String)> = tags(html, 0, html.len())
        .filter(|t| t.is("h4"))
        .map(|t| {
            let (_, end) = element_end(html, &t);
            (t.start, end, text(inner(html, &t), true).to_ascii_lowercase())
        })
        .collect();
    let mut doc = Doc { text: markdown(&html[..headings.first().map_or(html.len(), |h| h.0)]), ..Doc::default() };
    for (i, (_, start, title)) in headings.iter().enumerate() {
        let section = &html[*start..headings.get(i + 1).map_or(html.len(), |h| h.0)];
        match title.as_str() {
            "parameters" => doc.params.extend(rows(section)),
            "throws" => doc.throws.extend(rows(section)),
            "see also" => doc.see.extend(rows(section).into_iter().map(|(name, _)| name)),
            "return" => doc.returns = Some(markdown(section)).filter(|r| !r.is_empty()),
            "samples" => {
                let samples = tags(section, 0, section.len()).filter(|t| t.is("pre")).map(|t| text(inner(section, &t), false));
                doc.samples.extend(samples.map(|s| s.trim_matches(['\r', '\n']).to_owned()).filter(|s| !s.trim().is_empty()));
            }
            _ => {}
        }
    }
    doc
}

/// The rows of a Dokka table: the name (underlined, or a link) and the description.
fn rows(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((tag, row)) = find(html, pos, html.len(), |t| t.name == "div" && t.has_class("table-row")) {
        pos = element_end(html, &tag).1;
        let name = find(row, 0, row.len(), |t| t.name == "u").or_else(|| find(row, 0, row.len(), |t| t.name == "a"));
        let Some((_, name)) = name else { continue };
        let desc = find(row, 0, row.len(), |t| t.name == "div" && t.has_class("title")).map(|(_, d)| markdown(d)).unwrap_or_default();
        out.push((text(name, true), desc));
    }
    out
}

#[cfg(test)]
pub(crate) mod testing {
    /// A class page as JDK 17's javadoc writes it, for `p.JavaThing`: the class's description,
    /// `of(int)` and `wrap(java.util.List)` with their tags, and the field `DEFAULT`.
    pub const JDK17_PAGE: &str = r##"<!DOCTYPE HTML>
<html lang="en"><head><title>JavaThing</title></head><body class="class-declaration-page">
<nav><a href="#method-summary">Method</a></nav>
<main role="main">
<div class="header"><h1 title="Class JavaThing" class="title">Class JavaThing</h1></div>
<section class="class-description" id="class-description">
<hr>
<div class="type-signature"><span class="modifiers">public class </span><span class="element-name type-name-label">JavaThing</span></div>
<div class="block">A thing written in Java, made with <code>of</code>.
 <p>Holds a value &amp; nothing else.</div>
<dl class="notes">
<dt>See Also:</dt>
<dd><ul class="see-list"><li><a href="JavaThing.Builder.html"><code>JavaThing.Builder</code></a></li></ul></dd>
</dl>
</section>
<section class="summary">
<section class="method-summary" id="method-summary"><h2>Method Summary</h2>
<div class="col-last"><div class="block">Makes a thing.</div></div>
</section>
</section>
<section class="details">
<section class="field-details" id="field-detail"><h2>Field Details</h2>
<ul class="member-list"><li><section class="detail" id="DEFAULT"><h3>DEFAULT</h3>
<div class="member-signature"><span class="modifiers">public static final</span>&nbsp;<span class="return-type">int</span>&nbsp;<span class="element-name">DEFAULT</span></div>
<div class="block">The value a thing has by default.</div>
</section></li></ul></section>
<section class="constructor-details" id="constructor-detail"><h2>Constructor Details</h2>
<ul class="member-list"><li><section class="detail" id="&lt;init&gt;()"><h3>JavaThing</h3>
<div class="member-signature"><span class="modifiers">public</span>&nbsp;<span class="element-name">JavaThing</span>()</div>
<div class="block">An empty thing.</div>
</section></li></ul></section>
<section class="method-details" id="method-detail"><h2>Method Details</h2>
<ul class="member-list">
<li><section class="detail" id="of(int)"><h3>of</h3>
<div class="member-signature"><span class="modifiers">public static</span>&nbsp;<span class="return-type"><a href="JavaThing.html">JavaThing</a></span>&nbsp;<span class="element-name">of</span><wbr><span class="parameters">(int&nbsp;value)</span></div>
<div class="block">Makes a thing holding <code>value</code>.</div>
<dl class="notes">
<dt>Parameters:</dt>
<dd><code>value</code> - the value, at least <code>0</code></dd>
<dt>Returns:</dt>
<dd>a new thing</dd>
<dt>Throws:</dt>
<dd><code><a href="https://docs.oracle.com/IllegalArgumentException.html">IllegalArgumentException</a></code> - if <code>value &lt; 0</code></dd>
</dl>
</section></li>
<li><section class="detail" id="wrap(java.util.List)"><h3>wrap</h3>
<div class="member-signature"><span class="modifiers">public</span>&nbsp;<span class="type-parameters">&lt;T&gt;</span>&nbsp;<span class="return-type">void</span>&nbsp;<span class="element-name">wrap</span><wbr><span class="parameters">(java.util.List&lt;T&gt;&nbsp;items)</span></div>
<div class="block">Wraps the items.
<pre>
  thing.wrap(List.of(1, 2));
</pre></div>
</section></li>
</ul></section>
</section>
</main></body></html>
"##;

    /// The same kind of page as JDK 8's javadoc writes it, for `p.JavaThing.Builder`.
    pub const JDK8_PAGE: &str = r##"<html><body>
<a name="navbar.top"><!-- --></a>
<div class="description"><ul class="blockList"><li class="blockList">
<pre>public static class <span class="typeNameLabel">JavaThing.Builder</span></pre>
<div class="block">Builds things.</div>
</li></ul></div>
<a name="method.summary"><!-- --></a>
<h3>Method Summary</h3>
<a name="method.detail"><!-- --></a>
<h3>Method Detail</h3>
<a name="build--"><!-- --></a>
<ul class="blockListLast"><li class="blockList">
<h4>build</h4>
<pre>public&nbsp;<a href="JavaThing.html">JavaThing</a>&nbsp;build()</pre>
<div class="block">Builds the thing.</div>
<dl><dt><span class="returnLabel">Returns:</span></dt><dd>the thing</dd></dl>
</li></ul>
</body></html>
"##;

    /// Dokka's HTML pages for the Kotlin class `p.Greeter` and its `greet`, and for the
    /// top-level `shout` of package `p`.
    pub const DOKKA_CLASS: &str = r##"<html><body><div id="main">
<div class="main-content" data-page-type="Classlike" id="content" pageIds="lib::p/Greeter///PointingToDeclaration//1">
<div class="breadcrumbs"><a href="../index.html">lib</a></div>
<div class="cover "><h1 class="cover"><span><span>Greeter</span></span></h1>
<div class="platform-hinted " data-platform-hinted="data-platform-hinted"><div class="content sourceset-dependent-content" data-active="" data-togglable=":dokkaHtml/main">
<div class="symbol monospace"><span class="token keyword">class </span><a href="index.html">Greeter</a>(<span class="parameter ">name<span class="token operator">: </span>String</span>)</div>
<p class="paragraph">Greets people by <a href="name.html">name</a>.</p></div></div></div>
<div class="tabbedcontent"><div class="symbol monospace">fun greet(times: Int = 1): String</div><div class="brief "><p class="paragraph">Greets.</p></div></div>
</div></div></body></html>"##;

    pub const DOKKA_MEMBER: &str = r##"<html><body>
<div class="main-content" data-page-type="Member" id="content" pageIds="lib::p/Greeter/greet/#kotlin.Int/PointingToDeclaration//1">
<div class="cover "><h1 class="cover"><span><span>greet</span></span></h1></div>
<div class="platform-hinted " data-platform-hinted="data-platform-hinted"><div class="content sourceset-dependent-content" data-active="" data-togglable=":dokkaHtml/main">
<div class="symbol monospace"><span class="token keyword">fun </span><a href="greet.html"><span class="token function">greet</span></a><span class="token punctuation">(</span><span class="parameter ">times<span class="token operator">: </span><a href="https://kotlinlang.org/int.html">Int</a><span class="token operator"> = </span><span class="token constant">1</span></span><span class="token punctuation">)</span><span class="token operator">: </span><a href="https://kotlinlang.org/string.html">String</a></div>
<p class="paragraph">Greets the person <span data-unresolved-link="p/Greeter/times/#/PointingToDeclaration/">times</span> times.</p>
<span class="kdoc-tag"><h4 class="">Parameters</h4><div class="table"><div class="table-row" data-filterable-current=":dokkaHtml/main"><div class="main-subrow keyValue "><div class=""><span class="inline-flex"><div><u><span>times</span></u></div><span class="anchor-icon"></span></span></div><div><div class="title"><p class="paragraph">how many times, from 1</p></div></div></div></div></div></span>
<h4 class="">Return</h4><p class="paragraph">the greeting</p>
<h4 class="">Samples</h4><div class="sample-container"><pre><code class="block lang-kotlin" theme="idea">val g = Greeter(<span class="token string">"Ann"</span>)
println(g.greet(2))</code></pre><span class="top-right-position"><span class="copy-icon"></span><div class="copy-popup-wrapper popup-to-left"><span class="copy-popup-icon"></span><span>Content copied to clipboard</span></div></span></div>
</div></div>
</div></body></html>"##;

    pub const DOKKA_TOP: &str = r##"<html><body>
<div class="main-content" data-page-type="Member" id="content" pageIds="lib::p//shout/kotlin.String#/PointingToDeclaration//1">
<div class="cover "><h1 class="cover"><span><span>shout</span></span></h1></div>
<div class="platform-hinted "><div class="content sourceset-dependent-content">
<div class="symbol monospace">fun <a href="https://kotlinlang.org/string.html">String</a>.<a href="shout.html">shout</a>(): <a href="https://kotlinlang.org/string.html">String</a></div>
<p class="paragraph">The string in upper case, with <code class="lang-kotlin">"!"</code> after it.</p>
</div></div></div></body></html>"##;

    /// A Javadoc jar of the sample library, with every kind of page.
    pub fn sample_pages() -> Vec<(String, String)> {
        vec![
            ("p/JavaThing.html".to_owned(), JDK17_PAGE.to_owned()),
            ("p/JavaThing.Builder.html".to_owned(), JDK8_PAGE.to_owned()),
            ("lib/p/-greeter/index.html".to_owned(), DOKKA_CLASS.to_owned()),
            ("lib/p/-greeter/greet.html".to_owned(), DOKKA_MEMBER.to_owned()),
            ("lib/p/shout.html".to_owned(), DOKKA_TOP.to_owned()),
            ("p/package-summary.html".to_owned(), "<div class=\"block\">The package.</div>".to_owned()),
            ("index-all.html".to_owned(), "<a id=\"I:A\">A</a>".to_owned()),
        ]
    }

    /// The sample pages as a Javadoc jar at `path`.
    pub fn write_jar(path: &std::path::Path) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, html) in sample_pages() {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(html.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(types: &[&str]) -> Vec<Option<String>> {
        types.iter().map(|t| Some((*t).to_owned())).collect()
    }

    #[test]
    fn converts_html_to_markdown() {
        assert_eq!(
            markdown("Returns <code>null</code> if\n   none.<p>Second &lt;T&gt; &amp; <b>bold</b>."),
            "Returns `null` if none.\n\nSecond <T> & **bold**."
        );
        assert_eq!(markdown("A list:<ul><li>one</li><li><a href=\"x\">two</a></li></ul>"), "A list:\n\n- one\n- two");
        assert_eq!(markdown("Code:<pre>\n  val x = 1\n  x &lt; 2\n</pre>after"), "Code:\n\n```\n  val x = 1\n  x < 2\n```\n\nafter");
        assert_eq!(markdown("<pre><code class=\"block lang-kotlin\">f()</code></pre>"), "```kotlin\nf()\n```");
        assert_eq!(markdown("<code><a href=\"#x\"><code>x</code></a></code>&#8203;y"), "`x`y");
    }

    #[test]
    fn splits_parameter_tags() {
        assert_eq!(name_and_description("`value` - the value"), ("value".to_owned(), "the value".to_owned()));
        assert_eq!(name_and_description("count - how many"), ("count".to_owned(), "how many".to_owned()));
    }

    #[test]
    fn reads_anchors_and_signatures() {
        assert_eq!(parse_anchor("of(int)"), Some(("of".to_owned(), Some(vec!["int".to_owned()]))));
        assert_eq!(parse_anchor("put(java.lang.String,int[])").unwrap().1.unwrap(), ["java.lang.String", "int[]"]);
        assert_eq!(parse_anchor("build--"), Some(("build".to_owned(), Some(Vec::new()))));
        assert_eq!(parse_anchor("put-java.lang.String-int:A-").unwrap().1.unwrap(), ["java.lang.String", "int[]"]);
        assert_eq!(parse_anchor("DEFAULT"), Some(("DEFAULT".to_owned(), None)));
        assert_eq!(parse_anchor("method.summary"), None);
        let params = signature_params("public <T> void wrap(java.util.List<T> items, int... more)", "wrap").unwrap();
        assert_eq!(params, ["java.util.List<T> items", "int... more"]);
        assert!(signature_params("public void run()", "run").unwrap().is_empty());
        assert_eq!(simple_type("java.util.Map.Entry<K, V>"), "Entry");
        assert_eq!(simple_type("java.lang.String..."), "String[]");
        assert_eq!(simple_type("kotlin.collections.List[kotlin.String]?"), "List");
        assert_eq!(descriptor_type("[Ljava/util/Map$Entry;"), "Entry[]");
        assert_eq!(descriptor_type("I"), "int");
    }

    #[test]
    fn reads_standard_doclet_pages() {
        let docs = Docs::from_pages(&testing::sample_pages());
        let class = docs.class("p.JavaThing").unwrap();
        assert_eq!(class.text, "A thing written in Java, made with `of`.\n\nHolds a value & nothing else.");
        assert_eq!(class.see, ["JavaThing.Builder"]);

        let of = docs.method("p.JavaThing", "of", &some(&["int"])).unwrap();
        assert_eq!(of.names, ["value"]);
        assert_eq!(of.doc.text, "Makes a thing holding `value`.");
        assert_eq!(of.doc.params, [("value".to_owned(), "the value, at least `0`".to_owned())]);
        assert_eq!(of.doc.returns.as_deref(), Some("a new thing"));
        assert_eq!(of.doc.throws, [("IllegalArgumentException".to_owned(), "if `value < 0`".to_owned())]);
        assert!(docs.method("p.JavaThing", "of", &some(&["long", "int"])).is_none(), "the number of parameters must match");

        let wrap = docs.method("p.JavaThing", "wrap", &[None]).unwrap();
        assert_eq!(wrap.names, ["items"]);
        assert_eq!(wrap.doc.text, "Wraps the items.\n\n```\n  thing.wrap(List.of(1, 2));\n```");
        assert_eq!(docs.method("p.JavaThing", "<init>", &[]).unwrap().doc.text, "An empty thing.");
        assert_eq!(docs.field("p.JavaThing", "DEFAULT").unwrap().text, "The value a thing has by default.");

        assert_eq!(docs.class("p.JavaThing.Builder").unwrap().text, "Builds things.");
        let build = docs.method("p.JavaThing.Builder", "build", &[]).unwrap();
        assert_eq!(build.doc.text, "Builds the thing.");
        assert_eq!(build.doc.returns.as_deref(), Some("the thing"));
        assert!(docs.class("p.package-summary").is_none() && docs.class("index-all").is_none());
    }

    #[test]
    fn reads_dokka_pages() {
        let docs = Docs::from_pages(&testing::sample_pages());
        assert_eq!(docs.class("p.Greeter").unwrap().text, "Greets people by name.");
        let greet = docs.method("p.Greeter", "greet", &some(&["int"])).unwrap();
        assert_eq!(greet.names, ["times"]);
        assert_eq!(greet.doc.text, "Greets the person times times.");
        assert_eq!(greet.doc.params, [("times".to_owned(), "how many times, from 1".to_owned())]);
        assert_eq!(greet.doc.returns.as_deref(), Some("the greeting"));
        assert_eq!(greet.doc.samples, ["val g = Greeter(\"Ann\")\nprintln(g.greet(2))"]);
        let shout = docs.top_function("p", "shout", &some(&["String"])).unwrap();
        assert_eq!(shout.types, ["String"]);
        assert_eq!(shout.doc.text, "The string in upper case, with `\"!\"` after it.");
    }

    #[test]
    fn writes_doc_comments() {
        let doc = Doc {
            text: "Makes <T>.\n@odd".to_owned(),
            params: vec![("value".to_owned(), "the\nvalue".to_owned())],
            returns: Some("a thing */".to_owned()),
            samples: vec!["f()".to_owned()],
            ..Doc::default()
        };
        assert_eq!(doc.body(), "Makes <T>.\n @odd\n\n```kotlin\nf()\n```\n\n@param value the value\n@return a thing */");
        assert_eq!(comment("A <b> & c\n\nd */", "    ", true), "    /**\n     * A &lt;b&gt; &amp; c\n     *\n     * d * /\n     */\n");
    }
}
