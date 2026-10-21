//! The declarations and scopes of a Kotlin file.
//!
//! This is not a full Kotlin parser: it reads the package, the imports and every declaration
//! (classes, objects, functions, properties, type aliases, enum entries, constructors, parameters,
//! local variables, lambda parameters) with their types as written, and the scopes they are
//! visible in. Expressions are left as tokens; [`super::resolve`] reads them on demand. It never
//! fails, so it works on code that is being typed.

use std::ops::Range;

use super::lexer::{self, Lexed, Tok};

pub type DeclId = usize;
pub type ScopeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Class,
    Interface,
    Object,
    /// A `companion object`.
    Companion,
    Enum,
    Annotation,
    EnumEntry,
    TypeAlias,
    Function,
    Constructor,
    /// A member or top-level property, or a `val`/`var` parameter of a primary constructor.
    Property,
    /// A function, constructor, lambda, `catch` or setter parameter.
    Parameter,
    /// A local `val` or `var`, or a `for` loop variable.
    Variable,
}

impl Kind {
    pub fn is_class(self) -> bool {
        matches!(self, Kind::Class | Kind::Interface | Kind::Object | Kind::Companion | Kind::Enum | Kind::Annotation)
    }

    pub fn is_value(self) -> bool {
        matches!(self, Kind::Property | Kind::Parameter | Kind::Variable | Kind::EnumEntry)
    }

    /// The keyword that declares it, for signatures.
    pub fn keyword(self) -> &'static str {
        match self {
            Kind::Class => "class",
            Kind::Interface => "interface",
            Kind::Object | Kind::Companion => "object",
            Kind::Enum => "enum class",
            Kind::Annotation => "annotation class",
            Kind::EnumEntry => "enum entry",
            Kind::TypeAlias => "typealias",
            Kind::Function => "fun",
            Kind::Constructor => "constructor",
            Kind::Property | Kind::Variable => "val",
            Kind::Parameter => "",
        }
    }
}

/// A type as written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeRef {
    /// The name as written: `List`, `kotlin.collections.List`; `*` for a star projection; empty
    /// for a function type.
    pub name: String,
    pub args: Vec<TypeRef>,
    pub nullable: bool,
    /// For a function type.
    pub func: Option<Box<FuncType>>,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FuncType {
    pub receiver: Option<TypeRef>,
    pub params: Vec<TypeRef>,
    pub ret: TypeRef,
}

impl TypeRef {
    /// The last segment of the name.
    pub fn simple_name(&self) -> &str {
        self.name.rsplit('.').next().unwrap_or_default()
    }

    /// The type as source text on one line.
    pub fn display(&self, src: &str) -> String {
        if self.span.is_empty() {
            return self.render();
        }
        normalize(src.get(self.span.clone()).unwrap_or_default())
    }

    /// A type that is not written in the source (a generated member's) as text.
    fn render(&self) -> String {
        let mut out = self.name.clone();
        if !self.args.is_empty() {
            let args: Vec<String> = self.args.iter().map(TypeRef::render).collect();
            out.push_str(&format!("<{}>", args.join(", ")));
        }
        if self.nullable {
            out.push('?');
        }
        out
    }

    /// A type named `name` that is not in the source.
    pub fn named(name: &str, args: Vec<TypeRef>) -> TypeRef {
        TypeRef { name: name.to_owned(), args, ..TypeRef::default() }
    }
}

/// Collapses whitespace runs, for signatures.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() && !matches!(c, ')' | '>' | ',' | '.' | '?') && !out.ends_with(['(', '<', '.']) {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<TypeRef>,
    pub span: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub kind: Kind,
    pub name: String,
    /// The name token (with its backquotes); for an anonymous declaration, its keyword.
    pub name_span: Range<usize>,
    /// The whole declaration, from its first modifier or annotation.
    pub span: Range<usize>,
    /// Modifiers and annotations (`@Name`).
    pub modifiers: Vec<String>,
    /// `var` rather than `val`.
    pub mutable: bool,
    /// `@Deprecated(level = HIDDEN)`.
    pub hidden: bool,
    pub type_params: Vec<TypeParam>,
    pub receiver: Option<TypeRef>,
    /// Parameters (of a function, constructor, or a class's primary constructor).
    pub params: Vec<DeclId>,
    /// The declared type, return type or aliased type.
    pub ty: Option<TypeRef>,
    /// The token where the initializer, expression body or delegate starts; for a `for` loop
    /// variable, the iterated expression.
    pub init: Option<usize>,
    /// Whether `init` is a delegate (`by`), the iterated expression of a `for` loop, or the
    /// `n`th component of a destructuring declaration.
    pub init_kind: InitKind,
    pub supertypes: Vec<TypeRef>,
    /// Members of a class: nested declarations, enum entries, `val`/`var` constructor parameters.
    pub members: Vec<DeclId>,
    pub parent: Option<DeclId>,
    /// Declared inside a function body or lambda.
    pub local: bool,
    /// A parameter with a default value, or `vararg`.
    pub has_default: bool,
    pub vararg: bool,
    pub doc: Option<Range<usize>>,
    /// The scope of a function's body (with its parameters), a class's body or a lambda's.
    pub body: Option<ScopeId>,
    /// Members the compiler generates for a class (a data class's `copy`, an enum's `values`),
    /// which are not in `members`. They are declarations of `generated` set.
    pub synthetic: Vec<DeclId>,
    /// Not written in the source: generated for a class, whose name it takes as its own.
    pub generated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitKind {
    /// `init` is the initializer or expression body.
    #[default]
    Value,
    /// `init` is the delegate after `by`.
    Delegate,
    /// A `for` loop variable: `init` is the iterated expression.
    Iterated,
    /// Component `n` (from 1) of an element of the iterated expression `init`.
    IteratedComponent(usize),
    /// Component `n` (from 1) of a destructuring declaration of `init`.
    Component(usize),
    /// Parameter `n` (from 0) of the lambda whose `{` is token `init`.
    LambdaParam(usize),
    /// Component `m` (from 1) of parameter `n` of the lambda whose `{` is token `init`.
    LambdaComponent(usize, usize),
}

impl Decl {
    /// A declaration with no modifiers, type or members yet; `start` is where it begins.
    pub fn new(kind: Kind, name: String, name_span: Range<usize>, start: usize) -> Self {
        Self {
            kind,
            name,
            name_span: name_span.clone(),
            span: start..name_span.end,
            modifiers: Vec::new(),
            mutable: false,
            hidden: false,
            type_params: Vec::new(),
            receiver: None,
            params: Vec::new(),
            ty: None,
            init: None,
            init_kind: InitKind::Value,
            supertypes: Vec::new(),
            members: Vec::new(),
            parent: None,
            local: false,
            has_default: false,
            vararg: false,
            doc: None,
            body: None,
            synthetic: Vec::new(),
            generated: false,
        }
    }

    pub fn has(&self, modifier: &str) -> bool {
        self.modifiers.iter().any(|m| m == modifier)
    }

    pub fn is_extension(&self) -> bool {
        self.receiver.is_some()
    }

    /// Not visible outside its file (`private`) or module (`internal`).
    pub fn is_private(&self) -> bool {
        self.has("private")
    }

    pub fn is_internal(&self) -> bool {
        self.has("internal")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    File,
    /// A class's header and body.
    Class(DeclId),
    /// A function's or constructor's parameters and body.
    Function(DeclId),
    Block,
    /// A lambda; `it` when it declares no parameters. `brace` is the token of its `{`.
    Lambda {
        brace: usize,
        it: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub kind: ScopeKind,
    pub range: Range<usize>,
    pub parent: Option<ScopeId>,
    /// Local declarations (and parameters) in order.
    pub decls: Vec<DeclId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub path: Vec<String>,
    pub star: bool,
    pub alias: Option<String>,
    /// The tokens of the path segments.
    pub tokens: Vec<usize>,
    pub span: Range<usize>,
}

impl Import {
    pub fn fq(&self) -> String {
        self.path.join(".")
    }

    /// The name it brings into scope.
    pub fn name(&self) -> Option<&str> {
        if self.star { None } else { self.alias.as_deref().or(self.path.last().map(String::as_str)) }
    }
}

#[derive(Debug, Default)]
pub struct File {
    pub lexed: Lexed,
    pub package: String,
    /// The tokens of the package name's segments.
    pub package_tokens: Vec<usize>,
    pub imports: Vec<Import>,
    pub decls: Vec<Decl>,
    pub top: Vec<DeclId>,
    pub scopes: Vec<Scope>,
    /// Where a new import goes: after the last import, or the package.
    pub import_offset: usize,
    /// Whether an import inserted there needs a blank line before it (right after the package).
    pub import_after_package: bool,
}

impl File {
    /// The innermost scope containing `offset`.
    pub fn scope_at(&self, offset: usize) -> ScopeId {
        let mut best = 0;
        for (i, s) in self.scopes.iter().enumerate().skip(1) {
            if s.range.start < offset && offset <= s.range.end {
                let b = &self.scopes[best].range;
                if best == 0 || (s.range.end - s.range.start) <= (b.end - b.start) {
                    best = i;
                }
            }
        }
        best
    }

    /// The scope chain from the innermost scope at `offset` outwards.
    pub fn scope_chain(&self, offset: usize) -> Vec<ScopeId> {
        let mut out = Vec::new();
        let mut s = Some(self.scope_at(offset));
        while let Some(id) = s {
            out.push(id);
            s = self.scopes[id].parent;
        }
        out
    }

    /// The declarations enclosing `offset` (innermost first) and declared at `offset`.
    pub fn decl_at_name(&self, offset: usize) -> Option<DeclId> {
        self.decls.iter().position(|d| d.name_span.start <= offset && offset <= d.name_span.end && !d.name.is_empty())
    }

    /// The fully qualified name of a non-local declaration: the package, then enclosing classes.
    pub fn fq_name(&self, id: DeclId) -> String {
        let mut parts = vec![self.decls[id].name.as_str()];
        let mut p = self.decls[id].parent;
        while let Some(pid) = p {
            let d = &self.decls[pid];
            if d.kind.is_class() || d.kind == Kind::EnumEntry {
                parts.push(&d.name);
            }
            p = d.parent;
        }
        if !self.package.is_empty() {
            parts.push(&self.package);
        }
        parts.reverse();
        parts.join(".")
    }

    /// The class a member belongs to.
    pub fn owner_class(&self, id: DeclId) -> Option<DeclId> {
        let parent = self.decls[id].parent?;
        (self.decls[parent].kind.is_class() || self.decls[parent].kind == Kind::EnumEntry).then_some(parent)
    }

    /// The companion object of a class.
    pub fn companion(&self, class: DeclId) -> Option<DeclId> {
        self.decls[class].members.iter().copied().find(|&m| self.decls[m].kind == Kind::Companion)
    }
}

const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "abstract",
    "open",
    "final",
    "sealed",
    "data",
    "enum",
    "annotation",
    "inner",
    "value",
    "inline",
    "noinline",
    "crossinline",
    "override",
    "lateinit",
    "const",
    "external",
    "expect",
    "actual",
    "suspend",
    "operator",
    "infix",
    "tailrec",
    "vararg",
    "companion",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Top,
    Class(DeclId),
    Local,
}

pub fn parse(src: &str) -> File {
    let lexed = lexer::lex(src);
    let mut p = Parser { src, lx: &lexed, decls: Vec::new(), scopes: Vec::new(), top: Vec::new() };
    let file_scope = p.scope(ScopeKind::File, 0..src.len(), None);
    let mut i = 0;
    let n = lexed.tokens.len();
    // `@file:JvmName("X")`
    while p.kind(i) == Some(Tok::At) && p.is(i + 1, "file") && p.kind(i + 2) == Some(Tok::Colon) {
        i = p.skip_annotation(i).0;
    }
    let mut package = String::new();
    let mut package_tokens = Vec::new();
    let mut import_offset = 0;
    let mut import_after_package = false;
    if p.is(i, "package") {
        i += 1;
        while p.kind(i) == Some(Tok::Ident) {
            package_tokens.push(i);
            i += 1;
            if p.kind(i) == Some(Tok::Dot) && p.kind(i + 1) == Some(Tok::Ident) && !lexed.tokens[i + 1].nl {
                i += 1;
            } else {
                break;
            }
        }
        package = package_tokens.iter().map(|&t| lexed.text(src, t)).collect::<Vec<_>>().join(".");
        if let Some(&last) = package_tokens.last() {
            import_offset = lexed.tokens[last].end;
            import_after_package = true;
        }
        if p.kind(i) == Some(Tok::Semi) {
            i += 1;
        }
    }
    let mut imports = Vec::new();
    while p.is(i, "import") {
        let start = lexed.tokens[i].start;
        i += 1;
        let mut import = Import { path: Vec::new(), star: false, alias: None, tokens: Vec::new(), span: start..start };
        loop {
            match p.kind(i) {
                Some(Tok::Ident) if import.path.is_empty() || !lexed.tokens[i].nl => {
                    import.path.push(lexed.text(src, i).to_owned());
                    import.tokens.push(i);
                    i += 1;
                }
                Some(Tok::Op) if &src[lexed.tokens[i].span()] == "*" => {
                    import.star = true;
                    i += 1;
                    break;
                }
                _ => break,
            }
            if p.kind(i) == Some(Tok::Dot) && !lexed.tokens[i].nl {
                i += 1;
            } else {
                break;
            }
        }
        if p.is(i, "as") && p.kind(i + 1) == Some(Tok::Ident) {
            import.alias = Some(lexed.text(src, i + 1).to_owned());
            i += 2;
        }
        if p.kind(i) == Some(Tok::Semi) {
            i += 1;
        }
        import.span.end = lexed.tokens[i.saturating_sub(1).min(n.saturating_sub(1))].end;
        import_offset = import.span.end;
        import_after_package = false;
        imports.push(import);
    }
    p.statements(i, n, file_scope, Owner::Top);
    p.synthesize();
    let (decls, scopes, top) = (p.decls, p.scopes, p.top);
    File { lexed, package, package_tokens, imports, decls, top, scopes, import_offset, import_after_package }
}

struct Parser<'a> {
    src: &'a str,
    lx: &'a Lexed,
    decls: Vec<Decl>,
    scopes: Vec<Scope>,
    top: Vec<DeclId>,
}

impl Parser<'_> {
    fn n(&self) -> usize {
        self.lx.tokens.len()
    }

    fn kind(&self, i: usize) -> Option<Tok> {
        self.lx.kind(i)
    }

    fn is(&self, i: usize, word: &str) -> bool {
        self.lx.is(self.src, i, word)
    }

    fn text(&self, i: usize) -> &str {
        self.lx.text(self.src, i)
    }

    fn raw(&self, i: usize) -> &str {
        &self.src[self.lx.tokens[i].span()]
    }

    fn start(&self, i: usize) -> usize {
        self.lx.tokens.get(i).map_or(self.src.len(), |t| t.start)
    }

    fn end(&self, i: usize) -> usize {
        self.lx.tokens.get(i).map_or(self.src.len(), |t| t.end)
    }

    /// The end of the token before `i`.
    fn end_before(&self, i: usize) -> usize {
        if i == 0 { 0 } else { self.end(i - 1) }
    }

    /// The token closing the bracket at `i`, or `None` when it is never closed.
    fn close(&self, i: usize) -> Option<usize> {
        self.lx.matching.get(i).copied().flatten().filter(|&c| c > i)
    }

    /// The token after the bracket group starting at `i`; `limit` if it is never closed.
    fn after_group(&self, i: usize, limit: usize) -> usize {
        self.close(i).map_or(limit, |c| c + 1).min(limit.max(i + 1))
    }

    fn is_name(&self, i: usize) -> bool {
        self.kind(i) == Some(Tok::Ident) && !lexer::HARD_KEYWORDS.contains(&self.raw(i))
    }

    fn scope(&mut self, kind: ScopeKind, range: Range<usize>, parent: Option<ScopeId>) -> ScopeId {
        self.scopes.push(Scope { kind, range, parent, decls: Vec::new() });
        self.scopes.len() - 1
    }

    fn add(&mut self, mut decl: Decl, scope: ScopeId, owner: Owner) -> DeclId {
        let id = self.decls.len();
        match owner {
            Owner::Top => {
                self.top.push(id);
                self.scopes[scope].decls.push(id);
            }
            Owner::Class(c) => {
                decl.parent = Some(c);
                self.decls_push_member(c, id);
            }
            Owner::Local => {
                decl.local = true;
                self.scopes[scope].decls.push(id);
            }
        }
        if decl.parent.is_none() {
            decl.parent = self.enclosing_decl(scope);
        }
        self.decls.push(decl);
        id
    }

    fn decls_push_member(&mut self, class: DeclId, id: DeclId) {
        self.decls[class].members.push(id);
    }

    /// The declaration owning `scope` or the nearest one around it.
    fn enclosing_decl(&self, scope: ScopeId) -> Option<DeclId> {
        let mut s = Some(scope);
        while let Some(id) = s {
            match self.scopes[id].kind {
                ScopeKind::Class(d) | ScopeKind::Function(d) => return Some(d),
                _ => s = self.scopes[id].parent,
            }
        }
        None
    }

    /// Skips an annotation at `i` (`@`). Returns the next token, the annotation's name and
    /// whether it hides the declaration (`@Deprecated(level = HIDDEN)`).
    fn skip_annotation(&self, i: usize) -> (usize, String, bool) {
        let mut j = i + 1;
        // Use-site target: `@get:JvmName`, `@file:...`.
        if self.kind(j) == Some(Tok::Ident) && self.kind(j + 1) == Some(Tok::Colon) && !self.lx.tokens[j + 1].nl {
            j += 2;
        }
        if self.kind(j) == Some(Tok::LBracket) {
            return (self.after_group(j, self.n()), String::new(), false);
        }
        let mut name = String::new();
        while self.kind(j) == Some(Tok::Ident) {
            name = self.text(j).to_owned();
            j += 1;
            if self.kind(j) == Some(Tok::Dot) && self.kind(j + 1) == Some(Tok::Ident) {
                j += 1;
            } else {
                break;
            }
        }
        if self.kind(j) == Some(Tok::Lt)
            && let Some((_, k)) = self.type_args(j)
        {
            j = k;
        }
        let mut hidden = false;
        if self.kind(j) == Some(Tok::LParen) && !self.lx.tokens[j].nl {
            let end = self.after_group(j, self.n());
            if name == "Deprecated" {
                let text = &self.src[self.start(j)..self.end_before(end)];
                hidden = text.contains("HIDDEN");
            }
            j = end;
        }
        (j, name, hidden)
    }

    /// Whether a declaration starts at `i`: its modifiers and the index of its keyword.
    fn decl_start(&self, i: usize, owner: Owner) -> Option<(Vec<String>, bool, usize)> {
        if i > 0 && matches!(self.kind(i - 1), Some(Tok::Dot | Tok::SafeDot | Tok::ColonColon)) {
            return None;
        }
        let mut mods = Vec::new();
        let mut hidden = false;
        let mut j = i;
        loop {
            match self.kind(j)? {
                Tok::At => {
                    let (next, name, h) = self.skip_annotation(j);
                    if next == j {
                        return None;
                    }
                    hidden |= h;
                    mods.push(format!("@{name}"));
                    j = next;
                }
                Tok::Ident => {
                    let word = self.raw(j);
                    let next = self.kind(j + 1);
                    let ok = match word {
                        "fun" if self.is(j + 1, "interface") => {
                            mods.push("fun".to_owned());
                            j += 1;
                            continue;
                        }
                        "class" | "interface" => self.is_name(j + 1),
                        "typealias" => self.is_name(j + 1),
                        "object" => matches!(next, Some(Tok::Ident | Tok::Colon | Tok::LBrace)),
                        "fun" => matches!(next, Some(Tok::Ident | Tok::Lt | Tok::LParen)),
                        "val" | "var" => matches!(next, Some(Tok::Ident | Tok::Lt | Tok::LParen)),
                        "init" => owner != Owner::Local && next == Some(Tok::LBrace) && mods.is_empty(),
                        "constructor" => matches!(owner, Owner::Class(_)) && next == Some(Tok::LParen),
                        _ => {
                            if MODIFIERS.contains(&word) && matches!(next, Some(Tok::Ident | Tok::At)) && !self.lx.tokens[j + 1].nl
                                || MODIFIERS.contains(&word) && next == Some(Tok::Ident) && !mods.is_empty()
                            {
                                mods.push(word.to_owned());
                                j += 1;
                                continue;
                            }
                            return None;
                        }
                    };
                    if !ok {
                        return None;
                    }
                    return Some((mods, hidden, j));
                }
                _ => return None,
            }
        }
    }

    fn statements(&mut self, start: usize, end: usize, scope: ScopeId, owner: Owner) {
        let mut i = start;
        while i < end.min(self.n()) {
            let before = i;
            match self.kind(i) {
                Some(Tok::LBrace | Tok::TemplateOpen) => i = self.block(i, end, scope),
                Some(Tok::At) => {
                    i = match self.decl_start(i, owner) {
                        Some((mods, hidden, kw)) => self.declaration(i, mods, hidden, kw, end, scope, owner),
                        None => i + 1,
                    };
                }
                Some(Tok::Ident) => {
                    if let Some((mods, hidden, kw)) = self.decl_start(i, owner) {
                        i = self.declaration(i, mods, hidden, kw, end, scope, owner);
                    } else {
                        i = match self.raw(i) {
                            "for" if self.kind(i + 1) == Some(Tok::LParen) => self.for_loop(i, end, scope),
                            "catch" if self.kind(i + 1) == Some(Tok::LParen) => self.catch(i, end, scope),
                            "get" | "set"
                                if owner != Owner::Local
                                    && self.kind(i + 1) == Some(Tok::LParen)
                                    && !matches!(self.kind(i.wrapping_sub(1)), Some(Tok::Dot | Tok::SafeDot)) =>
                            {
                                self.accessor(i, end, scope)
                            }
                            _ => i + 1,
                        };
                    }
                }
                _ => i += 1,
            }
            if i <= before {
                i = before + 1;
            }
        }
    }

    /// A `{` (or `${`) in code: a lambda or a block.
    fn block(&mut self, i: usize, end: usize, scope: ScopeId) -> usize {
        let close = self.close(i);
        let stop = close.unwrap_or(end).min(end.max(i + 1));
        let range = self.start(i)..close.map_or_else(|| self.end_before(stop), |c| self.end(c));
        if !self.is_lambda(i) {
            let s = self.scope(ScopeKind::Block, range, Some(scope));
            self.statements(i + 1, stop, s, Owner::Local);
            return close.map_or(stop, |c| c + 1);
        }
        // Lambda parameters: `{ a, (b, c): Pair<X, Y> -> ... }`.
        let mut params: Vec<(String, Range<usize>, Option<TypeRef>, InitKind)> = Vec::new();
        let mut j = i + 1;
        let mut arrow = None;
        let mut index = 0;
        loop {
            if self.is_name(j) {
                let span = self.lx.tokens[j].span();
                let name = self.text(j).to_owned();
                j += 1;
                let mut ty = None;
                if self.kind(j) == Some(Tok::Colon)
                    && let Some((t, k)) = self.parse_type(j + 1)
                {
                    ty = Some(t);
                    j = k;
                }
                params.push((name, span, ty, InitKind::LambdaParam(index)));
            } else if self.kind(j) == Some(Tok::LParen) {
                let Some(c) = self.close(j) else { break };
                let mut n = 0;
                for k in j + 1..c {
                    if self.is_name(k) && matches!(self.kind(k + 1), Some(Tok::Comma | Tok::RParen | Tok::Colon)) {
                        n += 1;
                        params.push((self.text(k).to_owned(), self.lx.tokens[k].span(), None, InitKind::LambdaComponent(index, n)));
                    }
                }
                j = c + 1;
                if self.kind(j) == Some(Tok::Colon)
                    && let Some((_, k)) = self.parse_type(j + 1)
                {
                    j = k;
                }
            } else {
                break;
            }
            index += 1;
            match self.kind(j) {
                Some(Tok::Comma) => j += 1,
                Some(Tok::Arrow) => {
                    arrow = Some(j);
                    break;
                }
                _ => break,
            }
        }
        let body = match arrow {
            Some(a) if j < stop => a + 1,
            _ => {
                params.clear();
                i + 1
            }
        };
        let s = self.scope(ScopeKind::Lambda { brace: i, it: arrow.is_none() }, range, Some(scope));
        for (name, span, ty, init_kind) in params {
            let mut d = Decl::new(Kind::Parameter, name, span.clone(), span.start);
            d.ty = ty;
            d.init = Some(i);
            d.init_kind = init_kind;
            self.add(d, s, Owner::Local);
        }
        self.statements(body, stop, s, Owner::Local);
        close.map_or(stop, |c| c + 1)
    }

    fn is_lambda(&self, i: usize) -> bool {
        if self.kind(i) == Some(Tok::TemplateOpen) || i == 0 {
            return false;
        }
        let prev = i - 1;
        match self.kind(prev) {
            Some(Tok::Arrow) => false,
            Some(Tok::Ident) => !matches!(self.raw(prev), "else" | "try" | "finally" | "do" | "init" | "when"),
            Some(Tok::RParen) => {
                let open = self.lx.matching[prev].unwrap_or(prev);
                !(open > 0 && matches!(self.raw(open - 1), "if" | "while" | "for" | "catch" | "when"))
            }
            _ => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn declaration(&mut self, i: usize, mods: Vec<String>, hidden: bool, kw: usize, end: usize, scope: ScopeId, owner: Owner) -> usize {
        let next = match self.raw(kw) {
            "class" | "interface" | "object" => self.class(i, mods, hidden, kw, end, scope, owner),
            "fun" => self.function(i, mods, hidden, kw, end, scope, owner),
            "val" | "var" => self.property(i, mods, hidden, kw, end, scope, owner),
            "typealias" => self.typealias(i, mods, kw, scope, owner),
            "constructor" => self.constructor(i, mods, kw, end, scope, owner),
            "init" => self.block(kw + 1, end, scope),
            _ => kw + 1,
        };
        next.max(kw + 1)
    }

    fn new_decl(&self, kind: Kind, i: usize, name_tok: Option<usize>, fallback: usize, mods: Vec<String>, hidden: bool) -> Decl {
        let (name, span) = match name_tok {
            Some(t) => (self.text(t).to_owned(), self.lx.tokens[t].span()),
            None => (String::new(), self.lx.tokens[fallback].span()),
        };
        let mut d = Decl::new(kind, name, span, self.start(i));
        d.modifiers = mods;
        d.hidden = hidden;
        d.doc = self.lx.docs.get(&i).cloned();
        d
    }

    #[allow(clippy::too_many_arguments)]
    fn class(&mut self, i: usize, mods: Vec<String>, hidden: bool, kw: usize, end: usize, scope: ScopeId, owner: Owner) -> usize {
        let word = self.raw(kw);
        let kind = match word {
            "interface" => Kind::Interface,
            "object" if mods.iter().any(|m| m == "companion") => Kind::Companion,
            "object" => Kind::Object,
            _ if mods.iter().any(|m| m == "enum") => Kind::Enum,
            _ if mods.iter().any(|m| m == "annotation") => Kind::Annotation,
            _ => Kind::Class,
        };
        let mut j = kw + 1;
        let name_tok = if self.is_name(j) {
            j += 1;
            Some(j - 1)
        } else {
            None
        };
        let mut d = self.new_decl(kind, i, name_tok, kw, mods, hidden);
        if name_tok.is_none() && kind == Kind::Companion {
            d.name = "Companion".to_owned();
        }
        if self.kind(j) == Some(Tok::Lt) {
            let (params, k) = self.type_params(j);
            d.type_params = params;
            j = k;
        }
        // An anonymous object is not visible by name.
        let anonymous = d.name.is_empty();
        let id = if anonymous {
            self.decls.push(Decl { parent: self.enclosing_decl(scope), local: owner == Owner::Local, ..d });
            self.decls.len() - 1
        } else {
            self.add(d, scope, owner)
        };
        let header_start = self.start(kw);
        let class_scope = self.scope(ScopeKind::Class(id), header_start..self.src.len(), Some(scope));
        self.decls[id].body = Some(class_scope);
        // Primary constructor: `(...)`, or `internal constructor(...)` across lines.
        let mut k = j;
        while self.kind(k) == Some(Tok::At) || self.kind(k) == Some(Tok::Ident) && MODIFIERS.contains(&self.raw(k)) {
            k = if self.kind(k) == Some(Tok::At) { self.skip_annotation(k).0 } else { k + 1 };
        }
        let explicit = self.is(k, "constructor");
        if explicit {
            k += 1;
        } else {
            k = j;
        }
        if self.kind(k) == Some(Tok::LParen) && (explicit || !self.lx.tokens[k].nl) {
            j = self.params(k, end, id, class_scope, true);
        }
        if self.kind(j) == Some(Tok::Colon) {
            j += 1;
            while let Some((ty, mut k)) = self.parse_type(j) {
                self.decls[id].supertypes.push(ty);
                if self.kind(k) == Some(Tok::LParen) {
                    k = self.after_group(k, end);
                }
                if self.is(k, "by") {
                    k += 1;
                    while k < end && !matches!(self.kind(k), Some(Tok::Comma | Tok::LBrace) | None) && !self.is(k, "where") {
                        k = if matches!(self.kind(k), Some(Tok::LParen | Tok::LBracket)) { self.after_group(k, end) } else { k + 1 };
                        if self.kind(k).is_some() && self.lx.tokens[k].nl {
                            break;
                        }
                    }
                }
                j = k;
                if self.kind(j) == Some(Tok::Comma) {
                    j += 1;
                } else {
                    break;
                }
            }
        }
        if self.is(j, "where") {
            j = self.skip_where(j + 1);
        }
        if self.kind(j) == Some(Tok::LBrace) {
            let close = self.close(j);
            let stop = close.unwrap_or(end).min(end.max(j + 1));
            let body_end = close.map_or_else(|| self.end_before(stop), |c| self.end(c));
            self.scopes[class_scope].range.end = body_end;
            let mut k = j + 1;
            if kind == Kind::Enum {
                k = self.enum_entries(k, stop, id, class_scope);
            }
            self.statements(k, stop, class_scope, Owner::Class(id));
            let next = close.map_or(stop, |c| c + 1);
            self.decls[id].span.end = body_end;
            next
        } else {
            let header_end = self.end_before(j);
            self.scopes[class_scope].range.end = header_end;
            self.decls[id].span.end = header_end;
            j
        }
    }

    /// Adds the members the compiler generates: `copy`, `componentN`, `toString`, `equals` and
    /// `hashCode` of data classes, `toString`, `equals` and `hashCode` of value classes, and
    /// `entries`, `values`, `valueOf`, `name` and `ordinal` of enum classes. They take the class's
    /// name span, so that go to definition goes to the class.
    fn synthesize(&mut self) {
        for class in 0..self.decls.len() {
            let c = &self.decls[class];
            if c.name.is_empty() || c.generated {
                continue;
            }
            let owner = TypeRef::named(&c.name, c.type_params.iter().map(|t| TypeRef::named(&t.name, Vec::new())).collect());
            let data = c.kind == Kind::Class && c.has("data");
            let value = c.kind == Kind::Class && (c.has("value") || c.has("@JvmInline"));
            let enumeration = c.kind == Kind::Enum;
            if !(data || value || enumeration) {
                continue;
            }
            let props: Vec<(String, Option<TypeRef>)> = c
                .params
                .iter()
                .map(|&p| &self.decls[p])
                .filter(|p| p.kind == Kind::Property)
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect();
            let any = |nullable| TypeRef { nullable, ..TypeRef::named("Any", Vec::new()) };
            if data {
                let params = props.iter().map(|(n, t)| (n.clone(), t.clone(), true)).collect();
                self.generate(class, Kind::Function, "copy", &[], Some(owner.clone()), params);
                for (n, (_, ty)) in props.iter().enumerate() {
                    self.generate(class, Kind::Function, &format!("component{}", n + 1), &["operator"], ty.clone(), Vec::new());
                }
            }
            if data || value {
                self.generate(class, Kind::Function, "toString", &["override"], Some(TypeRef::named("String", Vec::new())), Vec::new());
                let other = vec![("other".to_owned(), Some(any(true)), false)];
                self.generate(class, Kind::Function, "equals", &["override"], Some(TypeRef::named("Boolean", Vec::new())), other);
                self.generate(class, Kind::Function, "hashCode", &["override"], Some(TypeRef::named("Int", Vec::new())), Vec::new());
            }
            if enumeration {
                let entries = TypeRef::named("kotlin.enums.EnumEntries", vec![owner.clone()]);
                self.generate(class, Kind::Property, "entries", &["static"], Some(entries), Vec::new());
                let array = TypeRef::named("Array", vec![owner.clone()]);
                self.generate(class, Kind::Function, "values", &["static"], Some(array), Vec::new());
                let name = vec![("value".to_owned(), Some(TypeRef::named("String", Vec::new())), false)];
                self.generate(class, Kind::Function, "valueOf", &["static"], Some(owner), name);
                self.generate(class, Kind::Property, "name", &[], Some(TypeRef::named("String", Vec::new())), Vec::new());
                self.generate(class, Kind::Property, "ordinal", &[], Some(TypeRef::named("Int", Vec::new())), Vec::new());
            }
        }
    }

    /// One generated member of `class`.
    fn generate(
        &mut self,
        class: DeclId,
        kind: Kind,
        name: &str,
        mods: &[&str],
        ty: Option<TypeRef>,
        params: Vec<(String, Option<TypeRef>, bool)>,
    ) {
        let span = self.decls[class].name_span.clone();
        let id = self.decls.len();
        let mut d = Decl::new(kind, name.to_owned(), span.clone(), span.start);
        d.modifiers = mods.iter().map(|m| (*m).to_owned()).collect();
        d.ty = ty;
        d.parent = Some(class);
        d.generated = true;
        d.params = (id + 1..id + 1 + params.len()).collect();
        self.decls.push(d);
        for (pname, pty, has_default) in params {
            let mut p = Decl::new(Kind::Parameter, pname, span.clone(), span.start);
            p.ty = pty;
            p.has_default = has_default;
            p.parent = Some(id);
            p.generated = true;
            self.decls.push(p);
        }
        self.decls[class].synthetic.push(id);
    }

    fn skip_where(&self, mut j: usize) -> usize {
        while matches!(self.kind(j), Some(Tok::Ident | Tok::Colon | Tok::Comma | Tok::Dot | Tok::Question)) && !self.is(j, "fun") {
            if self.kind(j) == Some(Tok::Ident) && self.lx.tokens[j].nl && self.kind(j - 1) != Some(Tok::Comma) {
                break;
            }
            match self.parse_type(j) {
                Some((_, k)) if self.kind(j) == Some(Tok::Ident) && self.kind(j + 1) != Some(Tok::Colon) => j = k,
                _ => j += 1,
            }
        }
        j
    }

    fn enum_entries(&mut self, mut k: usize, stop: usize, class: DeclId, scope: ScopeId) -> usize {
        while k < stop {
            let start = k;
            while self.kind(k) == Some(Tok::At) {
                k = self.skip_annotation(k).0;
            }
            if !self.is_name(k)
                || !matches!(self.kind(k + 1), Some(Tok::Comma | Tok::Semi | Tok::LParen | Tok::LBrace | Tok::RBrace) | None)
            {
                return start;
            }
            let d = self.new_decl(Kind::EnumEntry, start, Some(k), k, Vec::new(), false);
            let entry = self.add(d, scope, Owner::Class(class));
            k += 1;
            if self.kind(k) == Some(Tok::LParen) {
                k = self.after_group(k, stop);
            }
            if self.kind(k) == Some(Tok::LBrace) {
                let close = self.close(k);
                let inner_stop = close.unwrap_or(stop).min(stop);
                let range = self.start(k)..close.map_or_else(|| self.end_before(inner_stop), |c| self.end(c));
                let s = self.scope(ScopeKind::Class(entry), range, Some(scope));
                self.decls[entry].body = Some(s);
                self.statements(k + 1, inner_stop, s, Owner::Class(entry));
                k = close.map_or(inner_stop, |c| c + 1);
            }
            self.decls[entry].span.end = self.end_before(k);
            match self.kind(k) {
                Some(Tok::Comma) => k += 1,
                Some(Tok::Semi) => return k + 1,
                _ => return k,
            }
        }
        k
    }

    /// Parameters in parentheses at `lp`, added to `owner` and its scope. Returns the token after them.
    fn params(&mut self, lp: usize, end: usize, owner: DeclId, scope: ScopeId, constructor: bool) -> usize {
        let close = self.close(lp).unwrap_or(end.min(self.n()));
        let mut j = lp + 1;
        while j < close {
            let start = j;
            let mut mods = Vec::new();
            let mut hidden = false;
            loop {
                if self.kind(j) == Some(Tok::At) {
                    let (next, name, h) = self.skip_annotation(j);
                    hidden |= h;
                    mods.push(format!("@{name}"));
                    j = next.max(j + 1);
                } else if self.kind(j) == Some(Tok::Ident)
                    && MODIFIERS.contains(&self.raw(j))
                    && matches!(self.kind(j + 1), Some(Tok::Ident | Tok::At))
                {
                    mods.push(self.raw(j).to_owned());
                    j += 1;
                } else {
                    break;
                }
            }
            let mut property = None;
            if constructor && (self.is(j, "val") || self.is(j, "var")) {
                property = Some(self.is(j, "var"));
                j += 1;
            }
            if !self.is_name(j) {
                // Skip what can't be read up to the next parameter.
                while j < close && self.kind(j) != Some(Tok::Comma) {
                    j = if matches!(self.kind(j), Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::Lt))
                        && self.kind(j) != Some(Tok::Lt)
                    {
                        self.after_group(j, close)
                    } else {
                        j + 1
                    };
                }
                j += 1;
                continue;
            }
            let name_tok = j;
            j += 1;
            let kind = if property.is_some() { Kind::Property } else { Kind::Parameter };
            let vararg = mods.iter().any(|m| m == "vararg");
            let mut d = self.new_decl(kind, start, Some(name_tok), name_tok, mods, hidden);
            d.mutable = property == Some(true);
            d.vararg = vararg;
            if self.kind(j) == Some(Tok::Colon)
                && let Some((ty, k)) = self.parse_type(j + 1)
            {
                d.ty = Some(ty);
                j = k;
            }
            if self.kind(j) == Some(Tok::Eq) {
                d.has_default = true;
                d.init = Some(j + 1);
                while j < close && self.kind(j) != Some(Tok::Comma) {
                    j = if matches!(self.kind(j), Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen)) {
                        self.after_group(j, close)
                    } else {
                        j + 1
                    };
                }
            }
            d.span.end = self.end_before(j);
            d.parent = Some(owner);
            let id = self.decls.len();
            self.decls.push(d);
            self.decls[owner].params.push(id);
            self.scopes[scope].decls.push(id);
            if property.is_some() {
                self.decls[owner].members.push(id);
            }
            if self.kind(j) == Some(Tok::Comma) {
                j += 1;
            }
        }
        (close + 1).min(self.n().max(close))
    }

    #[allow(clippy::too_many_arguments)]
    fn function(&mut self, i: usize, mods: Vec<String>, hidden: bool, kw: usize, end: usize, scope: ScopeId, owner: Owner) -> usize {
        let mut j = kw + 1;
        let mut type_params = Vec::new();
        if self.kind(j) == Some(Tok::Lt) {
            let (params, k) = self.type_params(j);
            type_params = params;
            j = k;
        }
        let mut receiver = None;
        let mut name_tok = None;
        if (self.kind(j) != Some(Tok::LParen) || self.kind(self.after_group(j, end)).is_some_and(|k| matches!(k, Tok::Dot | Tok::Question)))
            && let Some((ty, segs, k)) = self.parse_type_segments(j)
        {
            // `String?.name` lexes `?.` as one token.
            let safe = self.kind(k) == Some(Tok::SafeDot);
            if (self.kind(k) == Some(Tok::Dot) || safe) && self.is_name(k + 1) {
                receiver = Some(TypeRef { nullable: ty.nullable || safe, ..ty });
                name_tok = Some(k + 1);
                j = k + 2;
            } else if ty.args.is_empty() && !ty.nullable && ty.func.is_none() {
                // `fun a.b.C.name(`: the last segment is the name.
                let last = *segs.last().unwrap_or(&j);
                if segs.len() > 1 {
                    let first = segs[0];
                    let prefix: Vec<&str> = segs[..segs.len() - 1].iter().map(|&s| self.text(s)).collect();
                    receiver = Some(TypeRef {
                        name: prefix.join("."),
                        span: self.start(first)..self.end(segs[segs.len() - 2]),
                        ..TypeRef::default()
                    });
                }
                name_tok = Some(last);
                j = k;
            } else {
                j = k;
            }
        }
        let mut d = self.new_decl(Kind::Function, i, name_tok, kw, mods, hidden);
        d.type_params = type_params;
        d.receiver = receiver;
        let anonymous = name_tok.is_none();
        let id = if anonymous {
            self.decls.push(Decl { parent: self.enclosing_decl(scope), local: true, ..d });
            self.decls.len() - 1
        } else {
            self.add(d, scope, owner)
        };
        let fscope = self.scope(ScopeKind::Function(id), self.start(j)..self.src.len(), Some(scope));
        self.decls[id].body = Some(fscope);
        if self.kind(j) == Some(Tok::LParen) {
            j = self.params(j, end, id, fscope, false);
        }
        if self.kind(j) == Some(Tok::Colon)
            && let Some((ty, k)) = self.parse_type(j + 1)
        {
            self.decls[id].ty = Some(ty);
            j = k;
        }
        if self.is(j, "where") {
            j = self.skip_where(j + 1);
        }
        let next = match self.kind(j) {
            Some(Tok::LBrace) => {
                let close = self.close(j);
                let stop = close.unwrap_or(end).min(end.max(j + 1));
                self.statements(j + 1, stop, fscope, Owner::Local);
                close.map_or(stop, |c| c + 1)
            }
            Some(Tok::Eq) => {
                let e = self.expr_end(j + 1, end);
                self.decls[id].init = Some(j + 1);
                self.statements(j + 1, e, fscope, Owner::Local);
                e
            }
            _ => j,
        };
        let body_end = self.end_before(next);
        self.scopes[fscope].range.end = body_end.max(self.scopes[fscope].range.start);
        self.decls[id].span.end = body_end;
        next
    }

    #[allow(clippy::too_many_arguments)]
    fn property(&mut self, i: usize, mods: Vec<String>, hidden: bool, kw: usize, end: usize, scope: ScopeId, owner: Owner) -> usize {
        let mutable = self.is(kw, "var");
        let mut j = kw + 1;
        let mut type_params = Vec::new();
        if self.kind(j) == Some(Tok::Lt) {
            let (params, k) = self.type_params(j);
            type_params = params;
            j = k;
        }
        let kind = if owner == Owner::Local { Kind::Variable } else { Kind::Property };
        if self.kind(j) == Some(Tok::LParen) {
            // `val (a, b) = pair`
            let close = self.close(j).unwrap_or(end.min(self.n()));
            let mut n = 0;
            for k in j + 1..close {
                if self.is_name(k) && matches!(self.kind(k + 1), Some(Tok::Comma | Tok::RParen | Tok::Colon)) {
                    n += 1;
                    let mut d = self.new_decl(kind, k, Some(k), k, mods.clone(), hidden);
                    d.mutable = mutable;
                    d.init_kind = InitKind::Component(n);
                    d.doc = None;
                    if self.kind(k + 1) == Some(Tok::Colon)
                        && let Some((ty, _)) = self.parse_type(k + 2)
                    {
                        d.ty = Some(ty);
                    }
                    let after = (close + 1..close + 4).find(|&e| self.kind(e) == Some(Tok::Eq));
                    d.init = after.map(|e| e + 1);
                    self.add(d, scope, owner);
                }
            }
            let mut k = close + 1;
            if self.kind(k) == Some(Tok::Colon)
                && let Some((_, e)) = self.parse_type(k + 1)
            {
                k = e;
            }
            return if self.kind(k) == Some(Tok::Eq) { k + 1 } else { k };
        }
        let mut receiver = None;
        let mut name_tok = None;
        if let Some((ty, segs, k)) = self.parse_type_segments(j) {
            // `String?.name` lexes `?.` as one token.
            let safe = self.kind(k) == Some(Tok::SafeDot);
            if (self.kind(k) == Some(Tok::Dot) || safe) && self.is_name(k + 1) {
                receiver = Some(TypeRef { nullable: ty.nullable || safe, ..ty });
                name_tok = Some(k + 1);
                j = k + 2;
            } else if ty.args.is_empty() && !ty.nullable && ty.func.is_none() {
                let last = *segs.last().unwrap_or(&j);
                if segs.len() > 1 {
                    let prefix: Vec<&str> = segs[..segs.len() - 1].iter().map(|&s| self.text(s)).collect();
                    receiver = Some(TypeRef {
                        name: prefix.join("."),
                        span: self.start(segs[0])..self.end(segs[segs.len() - 2]),
                        ..TypeRef::default()
                    });
                }
                name_tok = Some(last);
                j = k;
            } else {
                j = k;
            }
        }
        if name_tok.is_none() {
            return j;
        }
        let mut d = self.new_decl(kind, i, name_tok, kw, mods, hidden);
        d.mutable = mutable;
        d.type_params = type_params;
        d.receiver = receiver;
        if self.kind(j) == Some(Tok::Colon)
            && let Some((ty, k)) = self.parse_type(j + 1)
        {
            d.ty = Some(ty);
            j = k;
        }
        if self.is(j, "where") {
            j = self.skip_where(j + 1);
        }
        let next = if self.kind(j) == Some(Tok::Eq) {
            d.init = Some(j + 1);
            j + 1
        } else if self.is(j, "get")
            && self.kind(j + 1) == Some(Tok::LParen)
            && self.kind(j + 2) == Some(Tok::RParen)
            && self.kind(j + 3) == Some(Tok::Eq)
        {
            // `val x get() = ...`: the getter's expression gives the type. The walker reads the
            // getter as an accessor.
            d.init = Some(j + 4);
            j
        } else if self.is(j, "by") {
            d.init = Some(j + 1);
            d.init_kind = InitKind::Delegate;
            j + 1
        } else {
            j
        };
        let span_end = match d.init {
            Some(s) => self.end_before(self.expr_end(s, end)),
            None => self.end_before(j),
        };
        d.span.end = span_end;
        self.add(d, scope, owner);
        next
    }

    fn typealias(&mut self, i: usize, mods: Vec<String>, kw: usize, scope: ScopeId, owner: Owner) -> usize {
        let mut j = kw + 1;
        let name_tok = j;
        j += 1;
        let mut d = self.new_decl(Kind::TypeAlias, i, Some(name_tok), kw, mods, false);
        if self.kind(j) == Some(Tok::Lt) {
            let (params, k) = self.type_params(j);
            d.type_params = params;
            j = k;
        }
        if self.kind(j) == Some(Tok::Eq)
            && let Some((ty, k)) = self.parse_type(j + 1)
        {
            d.ty = Some(ty);
            j = k;
        }
        d.span.end = self.end_before(j);
        self.add(d, scope, owner);
        j
    }

    fn constructor(&mut self, i: usize, mods: Vec<String>, kw: usize, end: usize, scope: ScopeId, owner: Owner) -> usize {
        let Owner::Class(class) = owner else { return kw + 1 };
        let mut d = self.new_decl(Kind::Constructor, i, None, kw, mods, false);
        d.name = self.decls[class].name.clone();
        let id = self.add(d, scope, owner);
        let fscope = self.scope(ScopeKind::Function(id), self.start(kw + 1)..self.src.len(), Some(scope));
        self.decls[id].body = Some(fscope);
        let mut j = self.params(kw + 1, end, id, fscope, false);
        if self.kind(j) == Some(Tok::Colon) && (self.is(j + 1, "this") || self.is(j + 1, "super")) {
            j += 2;
            if self.kind(j) == Some(Tok::LParen) {
                let stop = self.after_group(j, end);
                self.statements(j + 1, stop, fscope, Owner::Local);
                j = stop;
            }
        }
        let next = if self.kind(j) == Some(Tok::LBrace) {
            let close = self.close(j);
            let stop = close.unwrap_or(end).min(end.max(j + 1));
            self.statements(j + 1, stop, fscope, Owner::Local);
            close.map_or(stop, |c| c + 1)
        } else {
            j
        };
        let body_end = self.end_before(next);
        self.scopes[fscope].range.end = body_end;
        self.decls[id].span.end = body_end;
        next
    }

    /// `get() = ...`, `set(value) { ... }`: the setter's parameter is visible in its body.
    fn accessor(&mut self, i: usize, end: usize, scope: ScopeId) -> usize {
        let lp = i + 1;
        let close = self.close(lp).unwrap_or(lp);
        let mut j = close + 1;
        if self.kind(j) == Some(Tok::Colon)
            && let Some((_, k)) = self.parse_type(j + 1)
        {
            j = k;
        }
        let body_end = match self.kind(j) {
            Some(Tok::LBrace) => self.close(j).map_or(end, |c| c + 1),
            Some(Tok::Eq) => self.expr_end(j + 1, end),
            _ => return j,
        };
        let s = self.scope(ScopeKind::Block, self.start(lp)..self.end_before(body_end), Some(scope));
        if self.is_name(lp + 1) {
            let d = self.new_decl(Kind::Parameter, lp + 1, Some(lp + 1), lp + 1, Vec::new(), false);
            self.add(d, s, Owner::Local);
        }
        let body_start = j + 1;
        self.statements(body_start, body_end.min(end), s, Owner::Local);
        body_end
    }

    fn for_loop(&mut self, i: usize, end: usize, scope: ScopeId) -> usize {
        let lp = i + 1;
        let Some(rp) = self.close(lp) else { return lp };
        let mut j = lp + 1;
        while self.kind(j) == Some(Tok::At) {
            j = self.skip_annotation(j).0;
        }
        let mut vars: Vec<(usize, Option<TypeRef>, InitKind)> = Vec::new();
        if self.kind(j) == Some(Tok::LParen) {
            let c = self.close(j).unwrap_or(rp);
            let mut n = 0;
            for k in j + 1..c {
                if self.is_name(k) && matches!(self.kind(k + 1), Some(Tok::Comma | Tok::RParen | Tok::Colon)) {
                    n += 1;
                    vars.push((k, None, InitKind::IteratedComponent(n)));
                }
            }
            j = c + 1;
        } else if self.is_name(j) {
            let name = j;
            j += 1;
            let mut ty = None;
            if self.kind(j) == Some(Tok::Colon)
                && let Some((t, k)) = self.parse_type(j + 1)
            {
                ty = Some(t);
                j = k;
            }
            vars.push((name, ty, InitKind::Iterated));
        }
        let iterated = if self.is(j, "in") { Some(j + 1) } else { None };
        let body = rp + 1;
        let (body_end, next, braces) = if self.kind(body) == Some(Tok::LBrace) {
            let close = self.close(body);
            let stop = close.unwrap_or(end).min(end.max(body + 1));
            (close.map_or_else(|| self.end_before(stop), |c| self.end(c)), close.map_or(stop, |c| c + 1), true)
        } else {
            let e = self.expr_end(body, end);
            (self.end_before(e), body, false)
        };
        let s = self.scope(ScopeKind::Block, self.start(lp)..body_end, Some(scope));
        for (tok, ty, kind) in vars {
            let mut d = self.new_decl(Kind::Variable, tok, Some(tok), tok, Vec::new(), false);
            d.ty = ty;
            d.init = iterated;
            d.init_kind = kind;
            self.add(d, s, Owner::Local);
        }
        if let Some(start) = iterated {
            self.statements(start, rp, scope, Owner::Local);
        }
        if braces {
            let stop = self.close(body).unwrap_or(end).min(end.max(body + 1));
            self.statements(body + 1, stop, s, Owner::Local);
        }
        next
    }

    fn catch(&mut self, i: usize, end: usize, scope: ScopeId) -> usize {
        let lp = i + 1;
        let Some(rp) = self.close(lp) else { return lp };
        let body = rp + 1;
        if self.kind(body) != Some(Tok::LBrace) {
            return body;
        }
        let close = self.close(body);
        let stop = close.unwrap_or(end).min(end.max(body + 1));
        let range_end = close.map_or_else(|| self.end_before(stop), |c| self.end(c));
        let s = self.scope(ScopeKind::Block, self.start(lp)..range_end, Some(scope));
        if self.is_name(lp + 1) {
            let mut d = self.new_decl(Kind::Parameter, lp + 1, Some(lp + 1), lp + 1, Vec::new(), false);
            if self.kind(lp + 2) == Some(Tok::Colon)
                && let Some((ty, _)) = self.parse_type(lp + 3)
            {
                d.ty = Some(ty);
            }
            self.add(d, s, Owner::Local);
        }
        self.statements(body + 1, stop, s, Owner::Local);
        close.map_or(stop, |c| c + 1)
    }

    /// Where the expression starting at `s` ends (exclusive), by Kotlin's rules for line breaks.
    fn expr_end(&self, s: usize, end: usize) -> usize {
        expr_end(self.src, self.lx, s, end)
    }

    fn type_params(&self, lt: usize) -> (Vec<TypeParam>, usize) {
        let mut out = Vec::new();
        let mut j = lt + 1;
        while let Some(kind) = self.kind(j) {
            match kind {
                Tok::Gt => return (out, j + 1),
                Tok::At => j = self.skip_annotation(j).0,
                Tok::Ident if matches!(self.raw(j), "in" | "out" | "reified") && self.kind(j + 1) == Some(Tok::Ident) => j += 1,
                Tok::Ident => {
                    let name = self.text(j).to_owned();
                    let span = self.lx.tokens[j].span();
                    j += 1;
                    let mut bound = None;
                    if self.kind(j) == Some(Tok::Colon)
                        && let Some((ty, k)) = self.parse_type(j + 1)
                    {
                        bound = Some(ty);
                        j = k;
                    }
                    out.push(TypeParam { name, bound, span });
                }
                Tok::Comma => j += 1,
                _ => return (out, j),
            }
        }
        (out, j)
    }

    fn type_args(&self, lt: usize) -> Option<(Vec<TypeRef>, usize)> {
        let mut args = Vec::new();
        let mut j = lt + 1;
        loop {
            while self.kind(j) == Some(Tok::Ident) && matches!(self.raw(j), "in" | "out") && self.kind(j + 1) != Some(Tok::Comma) {
                j += 1;
            }
            let (ty, k) = self.parse_type(j)?;
            args.push(ty);
            j = k;
            match self.kind(j)? {
                Tok::Comma => j += 1,
                Tok::Gt => return Some((args, j + 1)),
                _ => return None,
            }
        }
    }

    fn parse_type(&self, j: usize) -> Option<(TypeRef, usize)> {
        self.parse_type_segments(j).map(|(t, _, k)| (t, k))
    }

    /// A type at `j`, the tokens of its name's segments, and the token after it.
    fn parse_type_segments(&self, mut j: usize) -> Option<(TypeRef, Vec<usize>, usize)> {
        while self.kind(j) == Some(Tok::At) {
            j = self.skip_annotation(j).0;
        }
        if self.is(j, "suspend") && matches!(self.kind(j + 1), Some(Tok::LParen | Tok::Ident)) {
            j += 1;
        }
        let start = j;
        let (mut ty, segs, mut k) = match self.kind(j)? {
            Tok::LParen => {
                let close = self.close(j)?;
                if self.kind(close + 1) == Some(Tok::Arrow) {
                    let params = self.func_params(j + 1, close);
                    let (ret, k) = self.parse_type(close + 2)?;
                    let func = FuncType { receiver: None, params, ret };
                    let ty = TypeRef { func: Some(Box::new(func)), span: self.start(start)..self.end_before(k), ..TypeRef::default() };
                    return Some((ty, Vec::new(), k));
                }
                let (inner, _) = self.parse_type(j + 1)?;
                let mut ty = inner;
                ty.span = self.start(start)..self.end(close);
                (ty, Vec::new(), close + 1)
            }
            Tok::Op if self.raw(j) == "*" => {
                (TypeRef { name: "*".into(), span: self.lx.tokens[j].span(), ..TypeRef::default() }, vec![j], j + 1)
            }
            Tok::Ident => {
                let mut segs = vec![j];
                let mut k = j + 1;
                while self.kind(k) == Some(Tok::Dot) && self.kind(k + 1) == Some(Tok::Ident) && !self.lx.tokens[k].nl {
                    // `A.()` is a function type with receiver `A`.
                    segs.push(k + 1);
                    k += 2;
                }
                let name = segs.iter().map(|&s| self.text(s)).collect::<Vec<_>>().join(".");
                let mut ty = TypeRef { name, ..TypeRef::default() };
                if self.kind(k) == Some(Tok::Lt)
                    && !self.lx.tokens[k].nl
                    && let Some((args, e)) = self.type_args(k)
                {
                    ty.args = args;
                    k = e;
                }
                (ty, segs, k)
            }
            _ => return None,
        };
        while self.kind(k) == Some(Tok::Question) && !self.lx.tokens[k].nl {
            ty.nullable = true;
            k += 1;
        }
        // `T & Any`
        if self.kind(k) == Some(Tok::Op) && self.raw(k) == "&" && self.is(k + 1, "Any") {
            k += 2;
        }
        ty.span = self.start(start)..self.end_before(k);
        // A function type with a receiver: `A.() -> B`, `A.(B) -> C`.
        if self.kind(k) == Some(Tok::Dot)
            && self.kind(k + 1) == Some(Tok::LParen)
            && let Some(close) = self.close(k + 1)
            && self.kind(close + 1) == Some(Tok::Arrow)
            && let Some((ret, e)) = self.parse_type(close + 2)
        {
            let params = self.func_params(k + 2, close);
            let func = FuncType { receiver: Some(ty), params, ret };
            let ty = TypeRef { func: Some(Box::new(func)), span: self.start(start)..self.end_before(e), ..TypeRef::default() };
            return Some((ty, Vec::new(), e));
        }
        Some((ty, segs, k))
    }

    /// The parameter types of a function type between `from` and `close`: `(Int, name: String)`.
    fn func_params(&self, from: usize, close: usize) -> Vec<TypeRef> {
        let mut out = Vec::new();
        let mut j = from;
        while j < close {
            if self.is_name(j) && self.kind(j + 1) == Some(Tok::Colon) {
                j += 2;
            }
            match self.parse_type(j) {
                Some((ty, k)) if k > j => {
                    out.push(ty);
                    j = k;
                }
                _ => j += 1,
            }
            if self.kind(j) == Some(Tok::Comma) {
                j += 1;
            }
        }
        out
    }
}

/// Where the expression starting at token `s` ends (exclusive): at a `;`, a `,` or closing
/// bracket that isn't its own, or a line break that Kotlin doesn't continue the expression over.
pub fn expr_end(src: &str, lx: &Lexed, s: usize, end: usize) -> usize {
    let end = end.min(lx.tokens.len());
    let mut k = s;
    while k < end {
        let t = lx.tokens[k];
        if k > s && t.nl && !continues(src, lx, k) {
            break;
        }
        match t.kind {
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose | Tok::Semi | Tok::Comma => break,
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::TemplateOpen | Tok::StrOpen => {
                k = match lx.matching[k] {
                    Some(c) if c > k => c + 1,
                    _ => end,
                };
            }
            _ => k += 1,
        }
    }
    k.min(end).max(s)
}

/// Whether the expression goes on over the line break before token `k`.
fn continues(src: &str, lx: &Lexed, k: usize) -> bool {
    let t = lx.tokens[k];
    let text = &src[t.span()];
    if matches!(t.kind, Tok::Dot | Tok::SafeDot | Tok::ColonColon)
        || t.kind == Tok::Op && matches!(text, "?:" | "&&" | "||" | ".." | "..<" | "==" | "!=")
        || t.kind == Tok::Ident && matches!(text, "else" | "catch" | "finally" | "as")
    {
        return true;
    }
    let p = lx.tokens[k - 1];
    match p.kind {
        Tok::Dot | Tok::SafeDot | Tok::ColonColon | Tok::Eq | Tok::Arrow | Tok::Comma | Tok::Colon | Tok::Lt => true,
        Tok::Op => !matches!(&src[p.span()], "++" | "--" | "!!"),
        Tok::Ident => matches!(&src[p.span()], "in" | "is" | "as" | "if" | "else" | "return" | "throw" | "when"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(f: &File, ids: &[DeclId]) -> Vec<String> {
        ids.iter().map(|&i| f.decls[i].name.clone()).collect()
    }

    #[test]
    fn reads_package_imports_and_top_level_declarations() {
        let src = r#"
@file:JvmName("Main")
package com.example.app

import kotlin.math.max
import java.util.*
import com.example.other.Thing as Other

/** The entry point. */
fun main(args: Array<String>) {
    val greeting = "hi"
    println(greeting)
}

private val count: Int = 1
typealias Names = List<String>
fun <T> List<T>.second(): T = this[1]
val String.lastChar: Char get() = this[length - 1]
"#;
        let f = parse(src);
        assert_eq!(f.package, "com.example.app");
        assert_eq!(f.imports.len(), 3);
        assert_eq!(f.imports[0].fq(), "kotlin.math.max");
        assert!(f.imports[1].star);
        assert_eq!(f.imports[2].name(), Some("Other"));
        assert_eq!(names(&f, &f.top), ["main", "count", "Names", "second", "lastChar"]);
        let main = &f.decls[f.top[0]];
        assert_eq!(main.kind, Kind::Function);
        assert!(src[main.doc.clone().unwrap()].contains("entry point"));
        assert_eq!(names(&f, &main.params), ["args"]);
        assert_eq!(f.decls[main.params[0]].ty.as_ref().unwrap().name, "Array");
        let second = &f.decls[f.top[3]];
        assert_eq!(second.receiver.as_ref().unwrap().name, "List");
        assert_eq!(second.type_params[0].name, "T");
        assert_eq!(second.ty.as_ref().unwrap().name, "T");
        let last_char = &f.decls[f.top[4]];
        assert_eq!(last_char.receiver.as_ref().unwrap().name, "String");
        assert_eq!(last_char.ty.as_ref().unwrap().name, "Char");
        assert_eq!(f.fq_name(f.top[0]), "com.example.app.main");
        // `greeting` is local to main's body.
        let offset = src.find("println(greeting)").unwrap();
        let chain = f.scope_chain(offset);
        let local = chain.iter().flat_map(|&s| f.scopes[s].decls.iter()).find(|&&d| f.decls[d].name == "greeting");
        assert!(local.is_some());
        assert!(f.decls[*local.unwrap()].local);
    }

    #[test]
    fn reads_classes_members_and_constructors() {
        let src = r#"
data class User(val name: String, var age: Int = 0, id: Long) : Person(name), Comparable<User> {
    val label get() = "$name ($age)"
    constructor(name: String) : this(name, 0, 1L)
    fun greet(other: User): String { return "hi ${other.name}" }
    companion object {
        const val MAX = 10
        fun create(): User = User("x")
    }
    inner class Nested
    object Single
}
enum class Color(val rgb: Int) { RED(1), GREEN(2) { override fun toString() = "g" }, BLUE(3); fun hex() = rgb }
interface Shape { fun area(): Double }
"#;
        let f = parse(src);
        assert_eq!(names(&f, &f.top), ["User", "Color", "Shape"]);
        let user = &f.decls[f.top[0]];
        assert_eq!(user.kind, Kind::Class);
        assert!(user.has("data"));
        assert_eq!(names(&f, &user.params), ["name", "age", "id"]);
        assert_eq!(names(&f, &user.members), ["name", "age", "label", "User", "greet", "Companion", "Nested", "Single"]);
        assert_eq!(user.supertypes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Person", "Comparable"]);
        assert_eq!(user.supertypes[1].args[0].name, "User");
        let companion = f.companion(f.top[0]).unwrap();
        assert_eq!(names(&f, &f.decls[companion].members), ["MAX", "create"]);
        assert_eq!(f.fq_name(f.decls[companion].members[1]), "User.Companion.create");
        let color = &f.decls[f.top[1]];
        assert_eq!(color.kind, Kind::Enum);
        assert_eq!(names(&f, &color.members), ["rgb", "RED", "GREEN", "BLUE", "hex"]);
        let green = color.members[2];
        assert_eq!(names(&f, &f.decls[green].members), ["toString"]);
        let shape = &f.decls[f.top[2]];
        assert_eq!(shape.kind, Kind::Interface);
        assert_eq!(f.decls[shape.members[0]].ty.as_ref().unwrap().name, "Double");
    }

    #[test]
    fn reads_lambdas_loops_and_function_types() {
        let src = r#"
fun run(items: List<String>, block: String.(Int) -> Unit, cb: (a: Int) -> String?) {
    items.forEach { println(it) }
    items.mapIndexed { index, value -> value + index }
    for ((i, s) in items.withIndex()) println(s)
    for (item in items) { println(item) }
    try { } catch (e: IllegalStateException) { e.printStackTrace() }
}
"#;
        let f = parse(src);
        let run = &f.decls[f.top[0]];
        let block = f.decls[run.params[1]].ty.as_ref().unwrap();
        let func = block.func.as_ref().unwrap();
        assert_eq!(func.receiver.as_ref().unwrap().name, "String");
        assert_eq!(func.params[0].name, "Int");
        assert_eq!(func.ret.name, "Unit");
        let cb = f.decls[run.params[2]].ty.as_ref().unwrap().func.as_ref().unwrap();
        assert_eq!(cb.params[0].name, "Int");
        assert!(cb.ret.nullable);

        let in_lambda = src.find("println(it)").unwrap();
        let scope = f.scope_at(in_lambda);
        assert!(matches!(f.scopes[scope].kind, ScopeKind::Lambda { it: true, .. }));
        let in_indexed = src.find("value + index").unwrap();
        let scope = f.scope_at(in_indexed);
        assert!(matches!(f.scopes[scope].kind, ScopeKind::Lambda { it: false, .. }));
        assert_eq!(names(&f, &f.scopes[scope].decls), ["index", "value"]);
        let loop_body = src.find("println(s)").unwrap();
        assert_eq!(names(&f, &f.scopes[f.scope_at(loop_body)].decls), ["i", "s"]);
        let item = src.find("println(item)").unwrap();
        assert_eq!(names(&f, &f.scopes[f.scope_at(item)].decls), ["item"]);
        let catch = src.find("e.printStackTrace").unwrap();
        let e = f.scopes[f.scope_at(catch)].decls[0];
        assert_eq!(f.decls[e].ty.as_ref().unwrap().name, "IllegalStateException");
    }

    #[test]
    fn reads_annotations_documentation_and_hidden_declarations() {
        let src = "/** Sums. */\n@SinceKotlin(\"1.4\")\n@kotlin.jvm.JvmName(\"x\")\npublic inline fun sum(): Int = 0\n@Deprecated(\"no\", level = DeprecationLevel.HIDDEN)\nfun old() {}\n";
        let f = parse(src);
        assert_eq!(names(&f, &f.top), ["sum", "old"]);
        let sum = &f.decls[f.top[0]];
        assert!(src[sum.doc.clone().unwrap()].contains("Sums."));
        assert!(sum.has("@SinceKotlin") && sum.has("@JvmName") && sum.has("inline"));
        assert!(!sum.hidden && f.decls[f.top[1]].hidden);
    }

    #[test]
    fn survives_incomplete_code() {
        for src in ["fun f(", "class A {", "val x = foo.", "fun g() { val y = listOf(1, ", "@", "class", "fun <T", "import a.", "x@{"] {
            let _ = parse(src);
        }
        let f = parse("fun main() {\n    val s = \"x\"\n    s.\n}\nfun other() {}");
        assert_eq!(names(&f, &f.top), ["main", "other"]);
    }

    #[test]
    fn ends_expressions_at_line_breaks() {
        let src = "val a = foo\n    .bar()\nval b = 1 +\n  2\nval c = x\nval d = 0";
        let f = parse(src);
        let ends: Vec<&str> = f
            .top
            .iter()
            .map(|&d| {
                let start = f.decls[d].init.unwrap();
                let e = expr_end(src, &f.lexed, start, f.lexed.tokens.len());
                &src[f.lexed.tokens[start].start..f.lexed.tokens[e - 1].end]
            })
            .collect();
        assert_eq!(ends, ["foo\n    .bar()", "1 +\n  2", "x", "0"]);
    }
}
#[cfg(test)]
mod stress {
    #[test]
    #[ignore]
    fn parses_a_directory() {
        let dir = std::env::var("KOTLIN_SOURCES").unwrap();
        let start = std::time::Instant::now();
        let (mut files, mut decls, mut bytes) = (0, 0, 0);
        for entry in walkdir::WalkDir::new(dir) {
            let entry = entry.unwrap();
            if entry.path().extension().is_some_and(|e| e == "kt") {
                let src = std::fs::read_to_string(entry.path()).unwrap();
                let f = super::parse(&src);
                files += 1;
                decls += f.decls.len();
                bytes += src.len();
            }
        }
        eprintln!("{files} files, {bytes} bytes, {decls} decls in {:?}", start.elapsed());
    }
}
