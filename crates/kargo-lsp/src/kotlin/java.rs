//! Java declarations, read the way [`super::syntax`] reads Kotlin ones.
//!
//! Java code is seen in three places: the JDK's sources (`lib/src.zip`, see [`super::jdk`]), the
//! sources jars of libraries, and the project's source roots. This reads a file's package,
//! imports and declarations (classes, interfaces, enums, records and annotations, with their
//! fields, methods, constructors and parameters), with types as written, so that
//! [`super::resolve`] and [`super::features`] treat a Java class like a Kotlin one. Three things
//! differ:
//!
//! - A name resolves through the file's imports and its package, and through `java.lang` (see
//!   [`super::index::default_imports`]), not through Kotlin's default imports.
//! - Java's primitive types, arrays and the classes Kotlin maps to its own (`int`,
//!   `java.lang.String`, `java.util.List`) are named the way Kotlin names them: see [`kotlin_name`].
//! - A member marked `static` is reached through its class, as a companion member is.
//!
//! Method bodies, initializers and field values are skipped, and a Java file keeps no tokens:
//! nothing is resolved inside one, only looked up.

use std::ops::Range;

use super::lexer::{self, Lexed, Tok, Token};
use super::syntax::{Decl, DeclId, File, Import, Kind, Scope, ScopeKind, TypeParam, TypeRef};

/// Java classes that Kotlin names differently: the platform types of the JVM, which Kotlin maps
/// to its own classes (`java.util.List` is Kotlin's `MutableList`, and so on).
const PLATFORM: &[(&str, &str)] = &[
    ("java.lang.Object", "kotlin.Any"),
    ("java.lang.String", "kotlin.String"),
    ("java.lang.CharSequence", "kotlin.CharSequence"),
    ("java.lang.Comparable", "kotlin.Comparable"),
    ("java.lang.Iterable", "kotlin.collections.Iterable"),
    ("java.lang.Enum", "kotlin.Enum"),
    ("java.lang.Throwable", "kotlin.Throwable"),
    ("java.lang.Number", "kotlin.Number"),
    ("java.lang.Cloneable", "kotlin.Cloneable"),
    ("java.lang.Annotation", "kotlin.Annotation"),
    ("java.lang.Boolean", "kotlin.Boolean"),
    ("java.lang.Byte", "kotlin.Byte"),
    ("java.lang.Short", "kotlin.Short"),
    ("java.lang.Integer", "kotlin.Int"),
    ("java.lang.Long", "kotlin.Long"),
    ("java.lang.Character", "kotlin.Char"),
    ("java.lang.Float", "kotlin.Float"),
    ("java.lang.Double", "kotlin.Double"),
    ("java.util.Iterator", "kotlin.collections.MutableIterator"),
    ("java.util.ListIterator", "kotlin.collections.MutableListIterator"),
    ("java.util.Collection", "kotlin.collections.MutableCollection"),
    ("java.util.List", "kotlin.collections.MutableList"),
    ("java.util.Set", "kotlin.collections.MutableSet"),
    ("java.util.Map", "kotlin.collections.MutableMap"),
    ("java.util.Map.Entry", "kotlin.collections.MutableMap.MutableEntry"),
];

/// The packages a Java file sees without importing them.
pub const JAVA_DEFAULT_IMPORTS: &[&str] = &["java.lang"];

/// The Kotlin name of a Java class that Kotlin maps to one of its own, if it is one.
pub fn kotlin_name(fq: &str) -> Option<&'static str> {
    PLATFORM.iter().find(|(java, _)| *java == fq).map(|(_, kotlin)| *kotlin)
}

/// Java's primitive types, by their Kotlin names.
fn primitive(name: &str) -> Option<&'static str> {
    Some(match name {
        "boolean" => "kotlin.Boolean",
        "byte" => "kotlin.Byte",
        "short" => "kotlin.Short",
        "int" => "kotlin.Int",
        "long" => "kotlin.Long",
        "char" => "kotlin.Char",
        "float" => "kotlin.Float",
        "double" => "kotlin.Double",
        "void" => "kotlin.Unit",
        _ => return None,
    })
}

/// The Kotlin array class of a primitive type (by its Kotlin name), if it has one.
fn primitive_array(kotlin: &str) -> Option<&'static str> {
    Some(match kotlin {
        "kotlin.Boolean" => "kotlin.BooleanArray",
        "kotlin.Byte" => "kotlin.ByteArray",
        "kotlin.Short" => "kotlin.ShortArray",
        "kotlin.Int" => "kotlin.IntArray",
        "kotlin.Long" => "kotlin.LongArray",
        "kotlin.Char" => "kotlin.CharArray",
        "kotlin.Float" => "kotlin.FloatArray",
        "kotlin.Double" => "kotlin.DoubleArray",
        _ => return None,
    })
}

/// Words that are modifiers before a declaration. `sealed` and `non-sealed` are left out:
/// nothing here depends on them.
const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "native",
    "synchronized",
    "transient",
    "volatile",
    "strictfp",
    "default",
];

/// The class a member belongs to.
struct Owner {
    id: DeclId,
    name: String,
    interface: bool,
}

/// Parses a Java file.
pub fn parse(src: &str) -> File {
    let lexed = lexer::lex(src);
    let mut p = Parser { src, lx: &lexed, decls: Vec::new(), top: Vec::new() };
    let mut file =
        File { scopes: vec![Scope { kind: ScopeKind::File, range: 0..src.len(), parent: None, decls: Vec::new() }], ..File::default() };
    let n = p.n();
    let mut i = p.skip_annotations(0);
    // `module-info.java` declares no types that Kargo would resolve.
    if p.is(i, "module") || (p.is(i, "open") && p.is(i + 1, "module")) {
        return file;
    }
    if p.is(i, "package") {
        let (name, next) = p.qualified(i + 1);
        file.package = name;
        i = next;
        if p.kind(i) == Some(Tok::Semi) {
            i += 1;
        }
    }
    while p.is(i, "import") {
        let start = lexed.tokens[i].start;
        i += 1;
        if p.is(i, "static") {
            i += 1;
        }
        let (name, mut next) = p.qualified(i);
        let mut star = false;
        if p.kind(next) == Some(Tok::Dot) && p.text_is(next + 1, "*") {
            star = true;
            next += 2;
        }
        i = next;
        if p.kind(i) == Some(Tok::Semi) {
            i += 1;
        }
        let end = lexed.tokens[i.saturating_sub(1).min(n.saturating_sub(1))].end;
        let path: Vec<String> = name.split('.').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        file.imports.push(Import { path, star, alias: None, tokens: Vec::new(), span: start..end });
    }
    while i < n {
        i = match p.kind(i) {
            Some(Tok::RBrace) => i + 1,
            _ => p.member(i, None),
        };
    }
    file.decls = p.decls;
    file.top = p.top;
    file
}

/// Marks what code outside the library can't use: the non-public types of a library's file, and
/// the members of its classes that aren't public or protected (interface members are public).
pub fn hide_non_public(file: &mut File) {
    for id in 0..file.decls.len() {
        let d = &file.decls[id];
        if d.kind == Kind::Parameter {
            continue;
        }
        let implicit =
            d.kind == Kind::EnumEntry || d.parent.is_some_and(|p| matches!(file.decls[p].kind, Kind::Interface | Kind::Annotation));
        let public = d.has("public") || d.has("protected") || (implicit && !d.has("private"));
        if !public {
            file.decls[id].hidden = true;
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    lx: &'a Lexed,
    decls: Vec<Decl>,
    top: Vec<DeclId>,
}

impl Parser<'_> {
    fn n(&self) -> usize {
        self.lx.tokens.len()
    }

    fn kind(&self, i: usize) -> Option<Tok> {
        self.lx.tokens.get(i).map(|t| t.kind)
    }

    fn text(&self, i: usize) -> &str {
        if i < self.n() { self.lx.text(self.src, i) } else { "" }
    }

    /// Whether token `i` is the identifier `word`.
    fn is(&self, i: usize, word: &str) -> bool {
        self.kind(i) == Some(Tok::Ident) && self.text(i) == word
    }

    /// Whether token `i` has the text `word`, whatever its kind.
    fn text_is(&self, i: usize, word: &str) -> bool {
        i < self.n() && &self.src[self.lx.tokens[i].span()] == word
    }

    fn span(&self, i: usize) -> Range<usize> {
        self.lx.tokens.get(i).map_or(0..0, Token::span)
    }

    /// The index after the group that opens at `i`, or the end of the file when it is unclosed.
    fn after_group(&self, i: usize) -> usize {
        match self.lx.matching.get(i).copied().flatten() {
            Some(close) => close + 1,
            None => self.n(),
        }
    }

    /// The index after the annotations at `i`.
    fn skip_annotations(&self, mut i: usize) -> usize {
        while self.kind(i) == Some(Tok::At) && !self.is(i + 1, "interface") {
            i += 1;
            if self.kind(i) != Some(Tok::Ident) {
                break;
            }
            i += 1;
            while self.kind(i) == Some(Tok::Dot) && self.kind(i + 1) == Some(Tok::Ident) {
                i += 2;
            }
            if self.kind(i) == Some(Tok::LParen) {
                i = self.after_group(i);
            }
        }
        i
    }

    /// The modifiers and annotations at `i`: the index after them, and the modifiers.
    fn modifiers(&self, mut i: usize) -> (usize, Vec<String>) {
        let mut mods = Vec::new();
        loop {
            i = self.skip_annotations(i);
            if self.kind(i) != Some(Tok::Ident) {
                return (i, mods);
            }
            let word = self.text(i);
            if word == "default" && self.kind(i + 1) == Some(Tok::Colon) {
                return (i, mods);
            }
            if MODIFIERS.contains(&word) {
                mods.push(word.to_owned());
                i += 1;
            } else if word == "non" && self.text_is(i + 1, "-") && self.is(i + 2, "sealed") {
                i += 3;
            } else {
                return (i, mods);
            }
        }
    }

    /// A dotted name at `i`, and the index after it.
    fn qualified(&self, mut i: usize) -> (String, usize) {
        let mut name = String::new();
        while self.kind(i) == Some(Tok::Ident) {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(self.text(i));
            i += 1;
            if self.kind(i) == Some(Tok::Dot) && self.kind(i + 1) == Some(Tok::Ident) {
                i += 1;
            } else {
                break;
            }
        }
        (name, i)
    }

    /// The kind of a type declaration starting at `i` (its keyword), and how many tokens the keyword takes.
    fn type_keyword(&self, i: usize) -> Option<(Kind, usize)> {
        if self.kind(i) == Some(Tok::At) && self.is(i + 1, "interface") {
            return Some((Kind::Annotation, 2));
        }
        if self.kind(i) != Some(Tok::Ident) {
            return None;
        }
        match self.text(i) {
            "class" => Some((Kind::Class, 1)),
            "interface" => Some((Kind::Interface, 1)),
            "enum" => Some((Kind::Enum, 1)),
            "record" if self.kind(i + 1) == Some(Tok::Ident) && matches!(self.kind(i + 2), Some(Tok::LParen | Tok::Lt)) => {
                Some((Kind::Class, 1))
            }
            _ => None,
        }
    }

    /// Skips a declaration this parser doesn't understand: up to its `;` or the end of its body.
    fn skip_declaration(&self, mut i: usize) -> usize {
        loop {
            match self.kind(i) {
                None => return self.n(),
                Some(Tok::Semi) => return i + 1,
                Some(Tok::LBrace) => return self.after_group(i),
                Some(Tok::LParen | Tok::LBracket) => i = self.after_group(i),
                Some(Tok::RBrace) => return i,
                _ => i += 1,
            }
        }
    }

    /// The index of the first `,` or `;` at this level from `i`, or the end of the file.
    fn skip_value(&self, mut i: usize) -> usize {
        loop {
            match self.kind(i) {
                None | Some(Tok::Comma | Tok::Semi) => return i,
                Some(Tok::LParen | Tok::LBrace | Tok::LBracket) => i = self.after_group(i),
                Some(_) => i += 1,
            }
        }
    }

    /// The number of tokens of the `...` of a varargs parameter at `i`, or 0. The lexer reads it
    /// as `..` (an operator) and `.`.
    fn ellipsis(&self, i: usize) -> usize {
        if self.text_is(i, "..") && self.kind(i + 1) == Some(Tok::Dot) {
            2
        } else if self.kind(i) == Some(Tok::Dot) && self.kind(i + 1) == Some(Tok::Dot) && self.kind(i + 2) == Some(Tok::Dot) {
            3
        } else {
            0
        }
    }

    /// A declaration in a class body (or at the top of a file) at `i`, and the index after it. Always advances.
    fn member(&mut self, i: usize, owner: Option<&Owner>) -> usize {
        let next = self.member_at(i, owner);
        if next > i { next } else { i + 1 }
    }

    fn member_at(&mut self, i: usize, owner: Option<&Owner>) -> usize {
        match self.kind(i) {
            Some(Tok::Semi) => return i + 1,
            Some(Tok::LBrace) => return self.after_group(i),
            _ => {}
        }
        let start = i;
        let (mut j, mods) = self.modifiers(i);
        if self.kind(j) == Some(Tok::LBrace) {
            // An initializer.
            return self.after_group(j);
        }
        let doc = self.lx.docs.get(&start).cloned();
        if let Some((kind, keyword)) = self.type_keyword(j) {
            return self.type_decl(start, j, kind, keyword, mods, owner, doc);
        }
        let mut tparams = Vec::new();
        if self.kind(j) == Some(Tok::Lt) {
            let (params, next) = self.type_params(j);
            tparams = params;
            j = next;
        }
        if let Some(owner) = owner
            && self.is(j, &owner.name)
        {
            match self.kind(j + 1) {
                // A constructor.
                Some(Tok::LParen) => return self.method(start, j, mods, doc, tparams, None, Some(owner)),
                // The compact constructor of a record: its body is skipped.
                Some(Tok::LBrace) => return self.after_group(j + 1),
                _ => {}
            }
        }
        let Some((ty, k)) = self.ty(j) else { return self.skip_declaration(j) };
        if self.kind(k) != Some(Tok::Ident) {
            return self.skip_declaration(k);
        }
        if self.kind(k + 1) == Some(Tok::LParen) {
            return self.method(start, k, mods, doc, tparams, Some(ty), owner);
        }
        self.fields(start, k, mods, doc, ty, owner)
    }

    /// A class, interface, enum, annotation or record whose keyword is at `at`; returns the index after it.
    #[allow(clippy::too_many_arguments)]
    fn type_decl(
        &mut self,
        start: usize,
        at: usize,
        kind: Kind,
        keyword: usize,
        mods: Vec<String>,
        owner: Option<&Owner>,
        doc: Option<Range<usize>>,
    ) -> usize {
        let record = self.text(at) == "record" && keyword == 1;
        let mut i = at + keyword;
        if self.kind(i) != Some(Tok::Ident) {
            return self.skip_declaration(i);
        }
        let name = self.text(i).to_owned();
        let id = self.decl(kind, &name, i, start, owner.map(|o| o.id), mods, doc);
        self.add_member(owner, id);
        i += 1;
        if self.kind(i) == Some(Tok::Lt) {
            let (params, next) = self.type_params(i);
            self.decls[id].type_params = params;
            i = next;
        }
        if record && self.kind(i) == Some(Tok::LParen) {
            // The components are the parameters of the canonical constructor and properties of the record.
            let ctor = self.decl(Kind::Constructor, &name, at + 1, start, Some(id), Vec::new(), None);
            self.decls[id].members.push(ctor);
            let (params, next) = self.params(i, ctor);
            self.decls[ctor].params = params.clone();
            for param in params {
                let mut property = Decl::new(Kind::Property, self.decls[param].name.clone(), self.decls[param].name_span.clone(), start);
                property.parent = Some(id);
                property.ty = self.decls[param].ty.clone();
                property.modifiers = vec!["private".into(), "final".into()];
                let p = self.push(property);
                self.decls[id].members.push(p);
            }
            i = next;
        }
        loop {
            if self.is(i, "extends") {
                let (types, next) = self.type_list(i + 1, &["implements", "permits"]);
                self.decls[id].supertypes.extend(types);
                i = next;
            } else if self.is(i, "implements") {
                let (types, next) = self.type_list(i + 1, &["permits"]);
                self.decls[id].supertypes.extend(types);
                i = next;
            } else if self.is(i, "permits") {
                let (_, next) = self.type_list(i + 1, &[]);
                i = next;
            } else {
                break;
            }
        }
        if self.kind(i) != Some(Tok::LBrace) {
            return self.skip_declaration(i);
        }
        let end = self.lx.matching.get(i).copied().flatten().unwrap_or(self.n());
        if let Some(close) = self.lx.tokens.get(end) {
            self.decls[id].span.end = close.end;
        }
        let owner = Owner { id, name, interface: matches!(kind, Kind::Interface | Kind::Annotation) };
        let mut j = i + 1;
        if kind == Kind::Enum {
            j = self.enum_constants(j, &owner);
        }
        while j < end {
            j = self.member(j, Some(&owner));
        }
        end + 1
    }

    /// The constants at the start of an enum body; returns the index after them and their `;`.
    fn enum_constants(&mut self, mut i: usize, owner: &Owner) -> usize {
        loop {
            i = self.skip_annotations(i);
            if self.kind(i) != Some(Tok::Ident) {
                break;
            }
            let start = i;
            let doc = self.lx.docs.get(&start).cloned();
            let name = self.text(i).to_owned();
            let entry = self.decl(Kind::EnumEntry, &name, i, start, Some(owner.id), Vec::new(), doc);
            self.add_member(Some(owner), entry);
            i += 1;
            if self.kind(i) == Some(Tok::LParen) {
                i = self.after_group(i);
            }
            if self.kind(i) == Some(Tok::LBrace) {
                i = self.after_group(i);
            }
            if self.kind(i) == Some(Tok::Comma) {
                i += 1;
            } else {
                break;
            }
        }
        if self.kind(i) == Some(Tok::Semi) { i + 1 } else { i }
    }

    /// The fields declared at `name` (one or more, separated by commas), with their type.
    fn fields(
        &mut self,
        start: usize,
        mut name: usize,
        mods: Vec<String>,
        doc: Option<Range<usize>>,
        ty: TypeRef,
        owner: Option<&Owner>,
    ) -> usize {
        let mut mods = mods;
        if owner.is_some_and(|o| o.interface) {
            for word in ["public", "static", "final"] {
                if !mods.iter().any(|m| m == word) {
                    mods.push(word.to_owned());
                }
            }
        }
        loop {
            let name_text = self.text(name).to_owned();
            let id = self.decl(Kind::Property, &name_text, name, start, owner.map(|o| o.id), mods.clone(), doc.clone());
            self.decls[id].ty = Some(ty.clone());
            self.decls[id].mutable = !mods.iter().any(|m| m == "final");
            self.add_member(owner, id);
            let mut i = name + 1;
            while self.kind(i) == Some(Tok::LBracket) {
                i = self.after_group(i);
            }
            if self.kind(i) == Some(Tok::Eq) {
                i = self.skip_value(i + 1);
            }
            match self.kind(i) {
                Some(Tok::Comma) if self.kind(i + 1) == Some(Tok::Ident) => name = i + 1,
                Some(Tok::Semi) => return i + 1,
                _ => return self.skip_declaration(i),
            }
        }
    }

    /// A method or constructor named at `name` (a constructor has no type), with its parameter
    /// list and body (skipped). Methods outside a class don't exist, so `owner` is set for them.
    #[allow(clippy::too_many_arguments)]
    fn method(
        &mut self,
        start: usize,
        name: usize,
        mods: Vec<String>,
        doc: Option<Range<usize>>,
        tparams: Vec<TypeParam>,
        ty: Option<TypeRef>,
        owner: Option<&Owner>,
    ) -> usize {
        let text = self.text(name).to_owned();
        let kind = if ty.is_none() { Kind::Constructor } else { Kind::Function };
        let id = self.decl(kind, &text, name, start, owner.map(|o| o.id), mods, doc);
        self.add_member(owner, id);
        self.decls[id].type_params = tparams;
        self.decls[id].ty = ty;
        let (params, mut i) = self.params(name + 1, id);
        self.decls[id].params = params;
        while self.kind(i) == Some(Tok::LBracket) {
            i = self.after_group(i);
        }
        if self.is(i, "throws") {
            while !matches!(self.kind(i), None | Some(Tok::LBrace | Tok::Semi)) {
                i += 1;
            }
        }
        i = match self.kind(i) {
            Some(Tok::LBrace) => self.after_group(i),
            // An annotation method's default value.
            Some(_) if self.is(i, "default") => self.skip_value(i) + 1,
            Some(Tok::Semi) => i + 1,
            _ => return self.skip_declaration(i),
        };
        if let Some(last) = i.checked_sub(1).and_then(|l| self.lx.tokens.get(l)) {
            self.decls[id].span.end = last.end;
        }
        i
    }

    /// The parameters of a list whose `(` is at `open`, owned by `owner`; returns them and the index after the `)`.
    fn params(&mut self, open: usize, owner: DeclId) -> (Vec<DeclId>, usize) {
        if self.kind(open) != Some(Tok::LParen) {
            return (Vec::new(), open);
        }
        let close = self.lx.matching.get(open).copied().flatten().unwrap_or(self.n());
        let mut out = Vec::new();
        let mut i = open + 1;
        while i < close {
            let start = i;
            let (j, mods) = self.modifiers(i);
            if let Some((mut ty, mut k)) = self.ty(j) {
                let ellipsis = self.ellipsis(k);
                let vararg = ellipsis > 0;
                k += ellipsis;
                if self.kind(k) == Some(Tok::Ident) {
                    let name = self.text(k).to_owned();
                    let id = self.decl(Kind::Parameter, &name, k, start, Some(owner), mods, None);
                    k += 1;
                    while self.kind(k) == Some(Tok::LBracket) {
                        ty = array_of(ty);
                        k = self.after_group(k);
                    }
                    self.decls[id].ty = Some(ty);
                    self.decls[id].vararg = vararg;
                    out.push(id);
                }
                i = k;
            } else {
                i = j;
            }
            // Past anything left of this parameter, to its `,` or the `)`.
            i = self.skip_value(i).min(close);
            if self.kind(i) == Some(Tok::Comma) {
                i += 1;
            }
        }
        (out, (close + 1).min(self.n()))
    }

    /// A list of types separated by commas, from `i` up to a keyword in `stop` or a body; returns them and the index after them.
    fn type_list(&self, mut i: usize, stop: &[&str]) -> (Vec<TypeRef>, usize) {
        let mut out = Vec::new();
        loop {
            if self.kind(i) == Some(Tok::LBrace) || stop.iter().any(|s| self.is(i, s)) {
                return (out, i);
            }
            let Some((ty, next)) = self.ty(i) else { return (out, i) };
            out.push(ty);
            i = next;
            if self.kind(i) == Some(Tok::Comma) {
                i += 1;
            }
        }
    }

    /// The type at `i` (after its annotations), and the index after it.
    fn ty(&self, i: usize) -> Option<(TypeRef, usize)> {
        let i = self.skip_annotations(i);
        let start = self.lx.tokens.get(i)?.start;
        let (mut ty, mut k) = match self.kind(i)? {
            Tok::Question => {
                // A wildcard: its bound, or `*` without one.
                if self.is(i + 1, "extends") || self.is(i + 1, "super") {
                    self.ty(i + 2)?
                } else {
                    let end = self.lx.tokens[i].end;
                    (TypeRef { name: "*".into(), span: start..end, ..TypeRef::default() }, i + 1)
                }
            }
            Tok::Ident => match primitive(self.text(i)) {
                Some(kotlin) => (TypeRef { name: kotlin.into(), span: self.span(i), ..TypeRef::default() }, i + 1),
                None => self.class_type(i)?,
            },
            _ => return None,
        };
        loop {
            let j = self.skip_annotations(k);
            if self.kind(j) == Some(Tok::LBracket) && self.kind(j + 1) == Some(Tok::RBracket) {
                ty = array_of(ty);
                ty.span.end = self.lx.tokens[j + 1].end;
                k = j + 2;
            } else {
                break;
            }
        }
        Some((ty, k))
    }

    /// A class type, possibly qualified and with type arguments: `Map.Entry<K, V>`.
    fn class_type(&self, mut i: usize) -> Option<(TypeRef, usize)> {
        let start = self.lx.tokens.get(i)?.start;
        let mut name = String::new();
        // The arguments of the last segment: `Map.Entry<K, V>` has them on `Entry`.
        let mut args: Vec<TypeRef>;
        loop {
            if self.kind(i) != Some(Tok::Ident) {
                return None;
            }
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(self.text(i));
            i += 1;
            if self.kind(i) == Some(Tok::Lt) {
                let (list, next) = self.type_args(i)?;
                args = list;
                i = next;
            } else {
                args = Vec::new();
            }
            if self.kind(i) == Some(Tok::Dot) && self.kind(i + 1) == Some(Tok::Ident) {
                i += 1;
            } else {
                break;
            }
        }
        let end = self.lx.tokens[i - 1].end;
        Some((TypeRef { name, args, span: start..end, ..TypeRef::default() }, i))
    }

    /// The type arguments whose `<` is at `lt`, and the index after the `>`.
    fn type_args(&self, lt: usize) -> Option<(Vec<TypeRef>, usize)> {
        let mut i = lt + 1;
        let mut out = Vec::new();
        loop {
            if self.kind(i) == Some(Tok::Gt) {
                return Some((out, i + 1));
            }
            let (ty, next) = self.ty(i)?;
            out.push(ty);
            i = next;
            match self.kind(i) {
                Some(Tok::Comma) => i += 1,
                Some(Tok::Gt) => {}
                _ => return None,
            }
        }
    }

    /// The type parameters whose `<` is at `lt`, and the index after the `>`.
    fn type_params(&self, lt: usize) -> (Vec<TypeParam>, usize) {
        let mut i = self.skip_annotations(lt + 1);
        let mut out = Vec::new();
        loop {
            if self.kind(i) == Some(Tok::Gt) {
                return (out, i + 1);
            }
            if self.kind(i) != Some(Tok::Ident) {
                return (out, i + 1);
            }
            let name = self.text(i).to_owned();
            let span = self.span(i);
            i += 1;
            let mut bound = None;
            if self.is(i, "extends") {
                if let Some((ty, next)) = self.ty(i + 1) {
                    bound = Some(ty);
                    i = next;
                }
                // Further bounds, after `&`.
                while self.text_is(i, "&") {
                    match self.ty(i + 1) {
                        Some((_, next)) => i = next,
                        None => break,
                    }
                }
            }
            out.push(TypeParam { name, bound, span });
            if self.kind(i) == Some(Tok::Comma) {
                i += 1;
            }
        }
    }

    /// Adds a declaration named at token `name_tok`, starting at token `start`. It joins its
    /// parent's members only when the caller says so (see [`Parser::add_member`]).
    #[allow(clippy::too_many_arguments)]
    fn decl(
        &mut self,
        kind: Kind,
        name: &str,
        name_tok: usize,
        start: usize,
        parent: Option<DeclId>,
        mods: Vec<String>,
        doc: Option<Range<usize>>,
    ) -> DeclId {
        let name_span = self.span(name_tok);
        let begin = self.lx.tokens.get(start).map_or(name_span.start, |t| t.start);
        let mut d = Decl::new(kind, name.to_owned(), name_span, begin);
        d.modifiers = mods;
        d.doc = doc;
        d.parent = parent;
        self.push(d)
    }

    fn push(&mut self, decl: Decl) -> DeclId {
        self.decls.push(decl);
        self.decls.len() - 1
    }

    fn add_member(&mut self, owner: Option<&Owner>, id: DeclId) {
        match owner {
            Some(o) => self.decls[o.id].members.push(id),
            None => self.top.push(id),
        }
    }
}

/// The array type of `ty`: a primitive array when `ty` is a primitive, else `kotlin.Array<ty>`.
fn array_of(ty: TypeRef) -> TypeRef {
    match primitive_array(&ty.name) {
        Some(array) => TypeRef { name: array.into(), args: Vec::new(), ..ty },
        None => TypeRef { name: "kotlin.Array".into(), span: ty.span.clone(), args: vec![ty], ..TypeRef::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl_named<'a>(file: &'a File, name: &str) -> &'a Decl {
        file.decls.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("no declaration named {name}"))
    }

    fn id_of(file: &File, name: &str) -> DeclId {
        file.decls.iter().position(|d| d.name == name).unwrap()
    }

    #[test]
    fn reads_package_imports_and_types() {
        let src = "package java.util;\nimport java.io.File;\nimport java.util.function.*;\nimport static java.lang.Math.max;\n\n/** A list. */\npublic interface List<E> extends Collection<E>, Iterable<E> {\n    int size();\n}\n";
        let file = parse(src);
        assert_eq!(file.package, "java.util");
        assert_eq!(file.imports.len(), 3);
        assert_eq!(file.imports[0].fq(), "java.io.File");
        assert!(file.imports[1].star && file.imports[1].fq() == "java.util.function");
        assert_eq!(file.imports[2].fq(), "java.lang.Math.max");
        assert_eq!(file.fq_name(id_of(&file, "List")), "java.util.List");
        let list = decl_named(&file, "List");
        assert_eq!(list.kind, Kind::Interface);
        assert_eq!(list.type_params[0].name, "E");
        assert_eq!(list.supertypes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Collection", "Iterable"]);
        assert!(list.doc.is_some());
        let size = decl_named(&file, "size");
        assert_eq!(size.kind, Kind::Function);
        assert_eq!(size.ty.as_ref().unwrap().name, "kotlin.Int");
        assert_eq!(size.parent, Some(id_of(&file, "List")));
        assert!(!file.decls[list.members[0]].hidden, "interface members are public");
    }

    #[test]
    fn reads_members_with_generics_varargs_and_statics() {
        let src = r#"
package a.b;

public final class Box<T extends Comparable<? super T> & java.io.Serializable> {
    public static final int LIMIT = compute("{", '}');
    private final Map<String, List<T>> index = new HashMap<>();
    protected T[] items;
    static int[] counts, totals;

    public Box() { this.items = null; }

    public <R> R map(Function<? super T, ? extends R> f, String... names) throws IOException {
        if (true) { return null; }
        return f.apply(null);
    }

    static { System.out.println("}"); }

    public static <K, V> Map.Entry<K, V> entry(K key, V value) { return null; }

    class Inner {
        void go() {}
    }

    @Override
    public String toString() { return "x" + "{"; }
}
"#;
        let file = parse(src);
        let box_ = decl_named(&file, "Box");
        assert_eq!(box_.type_params[0].bound.as_ref().unwrap().name, "Comparable");
        assert!(box_.members.len() >= 7, "{:?}", box_.members.iter().map(|&m| file.decls[m].name.clone()).collect::<Vec<_>>());
        let limit = decl_named(&file, "LIMIT");
        assert!(limit.has("static") && limit.has("final") && !limit.mutable);
        assert_eq!(limit.ty.as_ref().unwrap().name, "kotlin.Int");
        let index = decl_named(&file, "index");
        let ty = index.ty.as_ref().unwrap();
        assert_eq!(ty.name, "Map");
        assert_eq!(ty.args[1].name, "List");
        assert_eq!(ty.args[1].args[0].name, "T");
        assert_eq!(index.modifiers, ["private", "final"]);
        let items = decl_named(&file, "items");
        assert_eq!(items.ty.as_ref().unwrap().name, "kotlin.Array");
        assert_eq!(items.ty.as_ref().unwrap().args[0].name, "T");
        let counts = decl_named(&file, "counts");
        assert_eq!(counts.ty.as_ref().unwrap().name, "kotlin.IntArray");
        assert!(decl_named(&file, "totals").has("static"));
        let ctor = file.decls.iter().find(|d| d.kind == Kind::Constructor).expect("constructor");
        assert_eq!(ctor.name, "Box");
        let map = decl_named(&file, "map");
        assert_eq!(map.type_params.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["R"]);
        assert_eq!(map.ty.as_ref().unwrap().name, "R");
        assert_eq!(map.params.len(), 2);
        let names = &file.decls[map.params[1]];
        assert!(names.vararg);
        assert_eq!(names.ty.as_ref().unwrap().name, "String");
        let entry = decl_named(&file, "entry");
        assert!(entry.has("static"));
        assert_eq!(entry.type_params.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["K", "V"]);
        assert_eq!(entry.ty.as_ref().unwrap().name, "Map.Entry");
        let inner = decl_named(&file, "Inner");
        assert_eq!(inner.kind, Kind::Class);
        assert!(decl_named(&file, "go").parent.is_some());
    }

    #[test]
    fn reads_enums_records_and_annotations() {
        let src = r#"
package x;

public enum Color implements Named {
    RED("r") { @Override String label() { return "red"; } },
    GREEN("g"), BLUE("b");

    private final String code;
    Color(String code) { this.code = code; }
    public String label() { return code; }
}

record Point(int x, int y) implements Comparable<Point> {
    Point {
        if (x < 0) throw new IllegalArgumentException();
    }
    public int sum() { return x + y; }
}

@interface Marker {
    String value() default "{";
    int[] sizes() default {1, 2};
}
"#;
        let file = parse(src);
        let color = decl_named(&file, "Color");
        assert_eq!(color.kind, Kind::Enum);
        let entries: Vec<&str> =
            color.members.iter().filter(|&&m| file.decls[m].kind == Kind::EnumEntry).map(|&m| file.decls[m].name.as_str()).collect();
        assert_eq!(entries, ["RED", "GREEN", "BLUE"]);
        assert!(decl_named(&file, "label").parent.is_some_and(|p| file.decls[p].name == "Color"));
        let point = decl_named(&file, "Point");
        assert_eq!(point.kind, Kind::Class);
        // The record's component `x` is a property, and also a parameter of its constructor.
        let x = file.decls.iter().find(|d| d.name == "x" && d.kind == Kind::Property).expect("the component property");
        assert_eq!(x.kind, Kind::Property);
        assert_eq!(x.ty.as_ref().unwrap().name, "kotlin.Int");
        assert!(decl_named(&file, "sum").parent == Some(id_of(&file, "Point")));
        assert_eq!(decl_named(&file, "Marker").kind, Kind::Annotation);
        assert!(file.decls.iter().any(|d| d.name == "sizes"));
    }

    #[test]
    fn reads_platform_types_and_hides_non_public_members() {
        let src = "package p;\nclass Hidden {}\npublic class Shown {\n  static void run() {}\n  void internal() {}\n  public String name() { return null; }\n}\n";
        let mut file = parse(src);
        hide_non_public(&mut file);
        assert!(decl_named(&file, "Hidden").hidden);
        assert!(!decl_named(&file, "Shown").hidden);
        assert!(decl_named(&file, "internal").hidden);
        assert!(!decl_named(&file, "name").hidden);
        assert_eq!(decl_named(&file, "run").ty.as_ref().unwrap().name, "kotlin.Unit");
        assert_eq!(kotlin_name("java.util.List"), Some("kotlin.collections.MutableList"));
        assert_eq!(kotlin_name("java.lang.Integer"), Some("kotlin.Int"));
        assert_eq!(kotlin_name("java.io.File"), None);
    }

    #[test]
    fn skips_module_declarations() {
        let file = parse("module java.base {\n  exports java.lang;\n}\n");
        assert!(file.decls.is_empty() && file.package.is_empty());
    }

    #[test]
    fn keeps_going_past_malformed_code() {
        let file = parse("package p;\n class A { int x = ; void f( { } } class B { String s; }\n");
        assert!(file.top.iter().any(|&t| file.decls[t].name == "B"));
        assert!(file.decls.iter().any(|d| d.name == "s"));
    }

    #[test]
    fn parses_every_prefix_of_a_file_without_panicking() {
        // A file is read while it is being typed, so any prefix of it must parse.
        let src = "package p;\nimport java.util.*;\n/** Doc. */\n@Deprecated\npublic final class Box<T extends Comparable<? super T>> extends Base implements Map.Entry<String, List<T>> {\n    static { run(\"}\"); }\n    private final Map<String, List<T>> index = new HashMap<>();\n    public <R> R map(Function<? super T, ? extends R> f, String... names) throws IOException { return null; }\n    enum E { A(1) { int x() { return 1; } }, B; }\n    record P(int x, int y) { P { } }\n    @interface M { String value() default \"{\"; }\n}\n";
        for end in 0..=src.len() {
            if src.is_char_boundary(end) {
                let mut file = parse(&src[..end]);
                hide_non_public(&mut file);
            }
        }
    }
}

/// Javadoc, already Markdown by [`super::features::kdoc_to_markdown`], with its inline tags
/// (`{@code x}`, `{@link X}`) made code spans and the HTML it is written in made Markdown.
pub fn javadoc_markdown(markdown: &str) -> String {
    html(&inline_tags(markdown))
}

/// `{@code x}`, `{@link X}`, `{@linkplain X label}` and the like, as code spans.
fn inline_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("{@") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[at..]);
            return out;
        };
        let body = after[..close].trim();
        let (tag, arg) = body.split_once(char::is_whitespace).map_or((body, ""), |(t, a)| (t, a.trim()));
        match tag {
            "code" | "literal" | "value" => out.push_str(&format!("`{arg}`")),
            "link" | "linkplain" => {
                // `{@link java.io.File}` shows `File`; `{@link #get(int) the getter}` shows its label.
                let (target, label) = arg.split_once(char::is_whitespace).map_or((arg, ""), |(t, l)| (t, l.trim()));
                let simple = target.split('(').next().unwrap_or(target).rsplit(['.', '#']).next().unwrap_or(target);
                let shown = if label.is_empty() { simple } else { label };
                out.push_str(&format!("`{shown}`"));
            }
            "inheritDoc" => {}
            _ => out.push_str(arg),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The HTML tags of JDK docs, as Markdown; other tags are dropped and their text kept.
fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(&entities(&rest[..at]));
        let Some(end) = rest[at..].find('>') else {
            out.push_str(&entities(&rest[at..]));
            return out;
        };
        let tag = rest[at + 1..at + end].trim().trim_end_matches('/').trim().to_ascii_lowercase();
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split_whitespace().next().unwrap_or_default();
        out.push_str(match name {
            "p" => "\n\n",
            "br" | "ul" | "ol" | "table" | "tr" => "\n",
            "li" if closing => "",
            "li" => "\n- ",
            "td" | "th" => " ",
            "pre" => "\n```\n",
            "code" | "tt" => "`",
            "em" | "i" => "*",
            "b" | "strong" => "**",
            _ => "",
        });
        rest = &rest[at + end + 1..];
    }
    out.push_str(&entities(rest));
    out
}

fn entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod javadoc_tests {
    use super::*;

    #[test]
    fn turns_javadoc_into_markdown() {
        assert_eq!(javadoc_markdown("Returns {@code null} if none."), "Returns `null` if none.");
        assert_eq!(javadoc_markdown("See {@link java.io.File} and {@linkplain #get(int) the getter}."), "See `File` and `the getter`.");
        assert_eq!(javadoc_markdown("One.<p>Two &lt;T&gt; <code>x</code>"), "One.\n\nTwo <T> `x`");
        assert_eq!(javadoc_markdown("A list:<ul><li>one</li><li>two</li></ul>"), "A list:\n\n- one\n- two\n");
        assert_eq!(javadoc_markdown("Unclosed {@code x"), "Unclosed {@code x");
    }
}
