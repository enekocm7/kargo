//! The declarations of a `.d.ts` file. Only what Kotlin can express is kept in detail
//! (interfaces, classes, functions, variables, type aliases, namespaces, imports and exports);
//! other constructs are skipped, and type syntax with no Kotlin counterpart (conditional, mapped,
//! indexed access types, ...) becomes [`Type::Other`]. A statement that doesn't parse is skipped,
//! so one unusual declaration never loses the rest of the file.

use super::lexer::{Tok, Token, lex};

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Any,
    Unknown,
    Never,
    Void,
    Undefined,
    Null,
    Object,
    String,
    Number,
    Boolean,
    BigInt,
    Symbol,
    This,
    StrLit,
    NumLit,
    BoolLit,
    /// A named type, `A.B<Args>`.
    Ref(Vec<String>, Vec<Type>),
    /// `import("module").A.B<Args>`.
    Import(String, Vec<String>, Vec<Type>),
    Array(Box<Type>),
    Tuple,
    Union(Vec<Type>),
    Inter(Vec<Type>),
    Fn(Box<Sig>),
    Ctor(Box<Sig>),
    Obj(Vec<Member>),
    /// `typeof a.b`: the type of a value.
    Query(Vec<String>),
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub optional: bool,
    pub rest: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub constraint: Option<Type>,
    pub default: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sig {
    pub tparams: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub ret: Type,
    /// A parameter or the return type has no annotation, as in a `.ts` file that leaves them to
    /// inference: they are `any` here.
    pub untyped: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemberKind {
    Prop {
        ty: Type,
        optional: bool,
        readonly: bool,
    },
    Method {
        sig: Sig,
    },
    Call(Sig),
    Ctor(Sig),
    /// A string index signature, `[key: string]: T`.
    Index(Type),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub doc: Option<String>,
    pub is_static: bool,
}

/// An interface, or a class (whose `extends` and `implements` are both in `extends`).
#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub tparams: Vec<TypeParam>,
    pub extends: Vec<Type>,
    pub members: Vec<Member>,
    pub is_class: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclKind {
    Interface(Interface),
    Alias { tparams: Vec<TypeParam>, ty: Type },
    Function(Sig),
    Var(Type),
    Namespace(Vec<Stmt>),
    Enum,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decl {
    pub name: String,
    pub kind: DeclKind,
    pub doc: Option<String>,
    pub exported: bool,
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportWhat {
    /// `import * as x from "m"`
    Namespace,
    /// `import x = require("m")`
    Equals,
    /// `import x from "m"`
    Default,
    /// `import { name as x } from "m"`
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Decl(Decl),
    Import {
        local: String,
        spec: String,
        what: ImportWhat,
    },
    /// `import x = A.B;`
    ImportAlias {
        local: String,
        path: Vec<String>,
    },
    /// `export = x;`
    ExportAssign(Vec<String>),
    /// `export default x;`
    ExportDefault(Vec<String>),
    /// `export { local as exported } [from "m"];`
    ExportNames {
        spec: Option<String>,
        names: Vec<(String, String)>,
    },
    /// `export * [as alias] from "m";`
    ExportStar {
        spec: String,
        alias: Option<String>,
    },
    /// `declare module "m" { ... }`
    AmbientModule {
        spec: String,
        body: Vec<Stmt>,
    },
}

pub fn parse(src: &str) -> Vec<Stmt> {
    let mut p = Parser { toks: lex(src), pos: 0, untyped: false };
    p.stmts(false)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Whether a parameter of the signature being read has no type.
    untyped: bool,
}

type R<T> = Result<T, ()>;

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn token(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) if *q == p)
    }

    fn is_at(&self, n: usize, p: &str) -> bool {
        matches!(self.peek_at(n), Tok::Punct(q) if *q == p)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(i) if i == kw)
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(i) if i == kw)
    }

    fn eat(&mut self, p: &str) -> bool {
        let yes = self.is(p);
        if yes {
            self.bump();
        }
        yes
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let yes = self.is_kw(kw);
        if yes {
            self.bump();
        }
        yes
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) { Ok(()) } else { Err(()) }
    }

    fn ident(&mut self) -> R<String> {
        match self.peek() {
            Tok::Ident(i) => {
                let i = i.clone();
                self.bump();
                Ok(i)
            }
            _ => Err(()),
        }
    }

    fn string(&mut self) -> R<String> {
        match self.peek() {
            Tok::Str(s) => {
                let s = s.clone();
                self.bump();
                Ok(s)
            }
            _ => Err(()),
        }
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    /// Skips a bracketed group starting at the current token, or else one token.
    fn skip_group(&mut self) {
        self.skip_bracketed(true);
    }

    /// Like [`Self::skip_group`] for code (an initializer, a default value), where `<` compares.
    fn skip_value(&mut self) {
        self.skip_bracketed(false);
    }

    /// Skips a group of `{}`, `()`, `[]`, and of `<>` when `angle` is set. Inside the others `<` is a
    /// comparison of the code in a body or initializer; inside type arguments it opens more of them.
    fn skip_bracketed(&mut self, angle: bool) {
        let close = match self.peek() {
            Tok::Punct("{") => "}",
            Tok::Punct("(") => ")",
            Tok::Punct("[") => "]",
            Tok::Punct("<") if angle => ">",
            _ => {
                self.bump();
                return;
            }
        };
        self.bump();
        while !self.at_eof() && !self.is(close) {
            self.skip_bracketed(close == ">");
        }
        self.bump();
    }

    /// Skips the body of a function or method in a `.ts` file, if there is one.
    fn skip_body(&mut self) {
        if self.is("{") {
            self.skip_group();
        }
    }

    /// The signature of a function value after `=` in a `.ts` file: an arrow function
    /// (`(a: number): number => ...`) or a function expression. Its body is left for the caller.
    fn function_value(&mut self) -> Option<Sig> {
        let start = self.pos;
        if self.is_kw("async") && (self.is_at(1, "(") || self.is_at(1, "<") || self.is_kw_at(1, "function")) {
            self.bump();
        }
        let sig = if self.eat_kw("function") {
            self.eat("*");
            if let Tok::Ident(_) = self.peek() {
                self.bump();
            }
            self.sig(":").ok()
        } else if self.is("(") || self.is("<") {
            self.sig(":").ok().filter(|_| self.eat("=>"))
        } else {
            None
        };
        if sig.is_none() {
            self.pos = start;
        }
        sig
    }

    /// Skips the rest of a statement: up to a `;` (consumed), a closing `}` (not consumed), the
    /// end of a `{ ... }` block, or a line that starts another declaration.
    fn skip_statement(&mut self) {
        let start = self.pos;
        while !self.at_eof() {
            if self.eat(";") || self.is("}") {
                return;
            }
            if self.pos > start
                && self.token().newline
                && matches!(self.peek(), Tok::Ident(i) if matches!(i.as_str(), "export" | "declare" | "interface" | "import" | "type" | "namespace"))
            {
                return;
            }
            if self.is("{") {
                self.skip_group();
                return;
            }
            self.bump();
        }
    }

    fn stmts(&mut self, in_block: bool) -> Vec<Stmt> {
        let mut out = Vec::new();
        loop {
            while self.eat(";") {}
            if self.at_eof() || (in_block && self.is("}")) {
                break;
            }
            let start = self.pos;
            match self.stmt() {
                Ok(mut stmts) => out.append(&mut stmts),
                Err(()) => {
                    self.pos = start;
                    self.skip_statement();
                }
            }
            if self.pos == start {
                self.bump();
            }
        }
        out
    }

    fn stmt(&mut self) -> R<Vec<Stmt>> {
        let doc = self.token().doc.clone();
        if self.is_kw("import") && !self.is_at(1, "(") {
            self.bump();
            return self.import();
        }
        if self.eat_kw("export") {
            return self.export(doc);
        }
        self.decl(doc, false, false)
    }

    fn import(&mut self) -> R<Vec<Stmt>> {
        if self.is_kw("type") && !self.is_at(1, "=") && !self.is_kw_at(1, "from") && !self.is_at(1, ",") {
            self.bump();
        }
        if let Tok::Str(_) = self.peek() {
            self.bump();
            self.eat(";");
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut pending: Vec<(String, ImportWhat)> = Vec::new();
        if let Tok::Ident(_) = self.peek()
            && !self.is_kw("from")
        {
            let local = self.ident()?;
            if self.eat("=") {
                if self.eat_kw("require") {
                    self.expect("(")?;
                    let spec = self.string()?;
                    self.expect(")")?;
                    self.eat(";");
                    return Ok(vec![Stmt::Import { local, spec, what: ImportWhat::Equals }]);
                }
                let path = self.path()?;
                self.eat(";");
                return Ok(vec![Stmt::ImportAlias { local, path }]);
            }
            pending.push((local, ImportWhat::Default));
            self.eat(",");
        }
        if self.eat("*") {
            if !self.eat_kw("as") {
                return Err(());
            }
            pending.push((self.ident()?, ImportWhat::Namespace));
        } else if self.eat("{") {
            for (name, local) in self.name_list()? {
                pending.push((local, ImportWhat::Named(name)));
            }
        }
        if !self.eat_kw("from") {
            return Err(());
        }
        let spec = self.string()?;
        self.skip_statement_tail();
        for (local, what) in pending {
            out.push(Stmt::Import { local, spec: spec.clone(), what });
        }
        Ok(out)
    }

    /// Import attributes (`with { ... }`) and the `;`.
    fn skip_statement_tail(&mut self) {
        if self.is_kw("with") || self.is_kw("assert") {
            self.bump();
            self.skip_group();
        }
        self.eat(";");
    }

    /// `{ a, b as c, type d }` after the `{`: pairs of (name, alias).
    fn name_list(&mut self) -> R<Vec<(String, String)>> {
        let mut out = Vec::new();
        while !self.eat("}") {
            if self.is_kw("type") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Str(_)) && !self.is_kw_at(1, "as") {
                self.bump();
            }
            let name = match self.bump() {
                Tok::Ident(i) | Tok::Str(i) => i,
                _ => return Err(()),
            };
            let alias = if self.eat_kw("as") {
                match self.bump() {
                    Tok::Ident(i) | Tok::Str(i) => i,
                    _ => return Err(()),
                }
            } else {
                name.clone()
            };
            out.push((name, alias));
            if !self.eat(",") && !self.is("}") {
                return Err(());
            }
        }
        Ok(out)
    }

    fn export(&mut self, doc: Option<String>) -> R<Vec<Stmt>> {
        if self.eat("=") {
            let path = self.path()?;
            self.eat(";");
            return Ok(vec![Stmt::ExportAssign(path)]);
        }
        if self.eat_kw("default") {
            if let Tok::Ident(_) = self.peek()
                && (self.is_at(1, ";") || self.toks[self.pos + 1].newline || self.is_at(1, "."))
                && !self.is_kw("function")
                && !self.is_kw("class")
                && !self.is_kw("interface")
            {
                let path = self.path()?;
                self.eat(";");
                return Ok(vec![Stmt::ExportDefault(path)]);
            }
            return self.decl(doc, true, true);
        }
        if self.eat("*") {
            let alias = if self.eat_kw("as") { Some(self.ident()?) } else { None };
            if !self.eat_kw("from") {
                return Err(());
            }
            let spec = self.string()?;
            self.skip_statement_tail();
            return Ok(vec![Stmt::ExportStar { spec, alias }]);
        }
        if self.is_kw("type") && self.is_at(1, "{") {
            self.bump();
        }
        if self.eat("{") {
            let names = self.name_list()?;
            let spec = if self.eat_kw("from") { Some(self.string()?) } else { None };
            self.skip_statement_tail();
            return Ok(vec![Stmt::ExportNames { spec, names }]);
        }
        if self.eat_kw("as") {
            // `export as namespace X;`: a global name for script use.
            self.skip_statement();
            return Ok(Vec::new());
        }
        if self.eat_kw("import") {
            let local = self.ident()?;
            self.expect("=")?;
            let mut out = Vec::new();
            if self.eat_kw("require") {
                self.expect("(")?;
                let spec = self.string()?;
                self.expect(")")?;
                out.push(Stmt::Import { local: local.clone(), spec, what: ImportWhat::Equals });
            } else {
                out.push(Stmt::ImportAlias { local: local.clone(), path: self.path()? });
            }
            self.eat(";");
            out.push(Stmt::ExportNames { spec: None, names: vec![(local.clone(), local)] });
            return Ok(out);
        }
        self.decl(doc, true, false)
    }

    fn path(&mut self) -> R<Vec<String>> {
        let mut path = vec![self.ident()?];
        while self.is(".") {
            self.bump();
            path.push(self.ident()?);
        }
        Ok(path)
    }

    fn decl(&mut self, doc: Option<String>, exported: bool, default: bool) -> R<Vec<Stmt>> {
        while self.is_kw("declare") || self.is_kw("abstract") || self.is_kw("async") || (self.is_kw("const") && self.is_kw_at(1, "enum")) {
            self.bump();
        }
        let decl = |name: String, kind: DeclKind| Stmt::Decl(Decl { name, kind, doc: doc.clone(), exported, default });
        let kw = match self.peek() {
            Tok::Ident(i) => i.clone(),
            _ => return Err(()),
        };
        match kw.as_str() {
            "function" => {
                self.bump();
                self.eat("*");
                let name = if let Tok::Ident(_) = self.peek() { self.ident()? } else { "default".to_owned() };
                let sig = self.sig(":")?;
                self.skip_body();
                self.eat(";");
                Ok(vec![decl(name, DeclKind::Function(sig))])
            }
            "const" | "let" | "var" => {
                self.bump();
                let mut out = Vec::new();
                loop {
                    let name = self.ident()?;
                    self.eat("!");
                    let annotated = self.eat(":");
                    let ty = if annotated { self.ty()? } else { Type::Any };
                    let mut kind = DeclKind::Var(ty);
                    if self.eat("=") {
                        // `const f = (a: number): number => ...` in a `.ts` file declares a function.
                        if !annotated && let Some(sig) = self.function_value() {
                            kind = DeclKind::Function(sig);
                        }
                        while !self.at_eof() && !self.is(",") && !self.is(";") && !self.is("}") && !self.token().newline {
                            self.skip_value();
                        }
                    }
                    out.push(decl(name, kind));
                    if !self.eat(",") {
                        break;
                    }
                }
                self.eat(";");
                Ok(out)
            }
            "class" | "interface" => {
                self.bump();
                let is_class = kw == "class";
                let name = if let Tok::Ident(n) = self.peek()
                    && n != "extends"
                    && n != "implements"
                {
                    self.ident()?
                } else {
                    "default".to_owned()
                };
                let tparams = self.tparams()?;
                let mut extends = Vec::new();
                while self.eat_kw("extends") || self.eat_kw("implements") {
                    loop {
                        extends.push(self.ty_primary_postfix()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                }
                self.expect("{")?;
                let members = self.members(is_class)?;
                Ok(vec![decl(name, DeclKind::Interface(Interface { tparams, extends, members, is_class }))])
            }
            "type" if matches!(self.peek_at(1), Tok::Ident(_)) => {
                self.bump();
                let name = self.ident()?;
                let tparams = self.tparams()?;
                self.expect("=")?;
                let ty = self.ty()?;
                self.eat(";");
                Ok(vec![decl(name, DeclKind::Alias { tparams, ty })])
            }
            "namespace" | "module" => {
                self.bump();
                if let Tok::Str(spec) = self.peek() {
                    let spec = spec.clone();
                    self.bump();
                    if !self.eat("{") {
                        self.eat(";");
                        return Ok(Vec::new());
                    }
                    let body = self.stmts(true);
                    self.expect("}")?;
                    return Ok(vec![Stmt::AmbientModule { spec, body }]);
                }
                let path = self.path()?;
                self.expect("{")?;
                let mut body = self.stmts(true);
                self.expect("}")?;
                // `namespace A.B { ... }` is `namespace A { namespace B { ... } }`.
                for inner in path[1..].iter().rev() {
                    body = vec![Stmt::Decl(Decl {
                        name: inner.clone(),
                        kind: DeclKind::Namespace(body),
                        doc: None,
                        exported: true,
                        default: false,
                    })];
                }
                Ok(vec![decl(path[0].clone(), DeclKind::Namespace(body))])
            }
            "global" if self.is_at(1, "{") => {
                // Global augmentations: nothing Kotlin code imports.
                self.bump();
                self.skip_group();
                Ok(Vec::new())
            }
            "enum" => {
                self.bump();
                let name = self.ident()?;
                self.skip_group();
                Ok(vec![decl(name, DeclKind::Enum)])
            }
            _ => Err(()),
        }
    }

    fn tparams(&mut self) -> R<Vec<TypeParam>> {
        let mut out = Vec::new();
        if !self.eat("<") {
            return Ok(out);
        }
        while !self.eat(">") {
            while self.is_kw("in") || self.is_kw("out") || self.is_kw("const") {
                if matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                } else {
                    break;
                }
            }
            let name = self.ident()?;
            let constraint = if self.eat_kw("extends") { Some(self.ty()?) } else { None };
            let default = if self.eat("=") { Some(self.ty()?) } else { None };
            out.push(TypeParam { name, constraint, default });
            if !self.eat(",") && !self.is(">") {
                return Err(());
            }
        }
        Ok(out)
    }

    /// Type parameters, parameters and a return type introduced by `arrow` (`:` or `=>`).
    fn sig(&mut self, arrow: &str) -> R<Sig> {
        let tparams = self.tparams()?;
        // Set by `params` for a parameter without a type; nested signatures keep their own.
        let outer = std::mem::take(&mut self.untyped);
        let params = self.params();
        let untyped_params = std::mem::replace(&mut self.untyped, outer);
        let params = params?;
        let typed = self.eat(arrow);
        let ret = if typed { self.return_type()? } else { Type::Any };
        Ok(Sig { tparams, params, ret, untyped: untyped_params || !typed })
    }

    fn return_type(&mut self) -> R<Type> {
        if self.is_kw("asserts") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            self.bump();
            if self.eat_kw("is") {
                self.ty()?;
            }
            return Ok(Type::Void);
        }
        if matches!(self.peek(), Tok::Ident(_)) && self.is_kw_at(1, "is") && !self.toks[self.pos + 1].newline {
            self.bump();
            self.bump();
            self.ty()?;
            return Ok(Type::Boolean);
        }
        self.ty()
    }

    fn params(&mut self) -> R<Vec<Param>> {
        self.expect("(")?;
        let mut out = Vec::new();
        while !self.eat(")") {
            while matches!(self.peek(), Tok::Ident(i) if matches!(i.as_str(), "public" | "private" | "protected" | "readonly" | "override"))
                && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("{") | Tok::Punct("["))
            {
                self.bump();
            }
            let rest = self.eat("...");
            let name = match self.peek() {
                Tok::Ident(i) => {
                    let i = i.clone();
                    self.bump();
                    i
                }
                Tok::Punct("{") | Tok::Punct("[") => {
                    self.skip_group();
                    "arg".to_owned()
                }
                _ => return Err(()),
            };
            let optional = self.eat("?");
            let typed = self.eat(":");
            let ty = if typed { self.ty()? } else { Type::Any };
            if self.eat("=") {
                while !self.at_eof() && !self.is(",") && !self.is(")") {
                    self.skip_value();
                }
            }
            if !typed && name != "this" {
                self.untyped = true;
            }
            if name != "this" {
                out.push(Param { name, ty, optional, rest });
            }
            if !self.eat(",") && !self.is(")") {
                return Err(());
            }
        }
        Ok(out)
    }

    /// Members of an interface, class or object type, after the `{`; consumes the `}`.
    fn members(&mut self, is_class: bool) -> R<Vec<Member>> {
        let mut out = Vec::new();
        loop {
            while self.eat(";") || self.eat(",") {}
            if self.eat("}") {
                return Ok(out);
            }
            if self.at_eof() {
                return Err(());
            }
            let start = self.pos;
            match self.member(is_class) {
                Ok(Some(m)) => out.push(m),
                Ok(None) => {}
                Err(()) => {
                    self.pos = start;
                    // Skip to the end of the member.
                    loop {
                        self.skip_group();
                        if self.at_eof() || self.is(";") || self.is(",") || self.is("}") || self.token().newline {
                            break;
                        }
                    }
                }
            }
            if self.pos == start {
                self.bump();
            }
        }
    }

    fn member(&mut self, is_class: bool) -> R<Option<Member>> {
        let doc = self.token().doc.clone();
        let mut is_static = false;
        let mut readonly = false;
        let mut private = false;
        while let Tok::Ident(word) = self.peek() {
            let modifier = matches!(
                word.as_str(),
                "static" | "readonly" | "public" | "private" | "protected" | "abstract" | "declare" | "override" | "accessor" | "async"
            );
            let followed_by_name = matches!(self.peek_at(1), Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) | Tok::Punct("[") | Tok::Punct("#"))
                && !self.toks[self.pos + 1].newline;
            if !modifier || !followed_by_name {
                break;
            }
            match word.as_str() {
                "static" => is_static = true,
                "readonly" => readonly = true,
                "private" | "protected" => private = true,
                _ => {}
            }
            self.bump();
        }
        if self.is("(") || self.is("<") {
            let sig = self.sig(":")?;
            return Ok(Some(Member { name: String::new(), kind: MemberKind::Call(sig), doc, is_static }));
        }
        if self.is_kw("new") && (self.is_at(1, "(") || self.is_at(1, "<")) {
            self.bump();
            let sig = self.sig(":")?;
            return Ok(Some(Member { name: String::new(), kind: MemberKind::Ctor(sig), doc, is_static }));
        }
        if is_class && self.is_kw("constructor") && self.is_at(1, "(") {
            self.bump();
            let sig = self.sig(":")?;
            self.skip_body();
            return Ok((!private).then_some(Member { name: String::new(), kind: MemberKind::Ctor(sig), doc, is_static }));
        }
        // Accessors: `get name(): T` and `set name(value: T)`.
        if (self.is_kw("get") || self.is_kw("set")) && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Str(_)) {
            let getter = self.is_kw("get");
            self.bump();
            let name = match self.bump() {
                Tok::Ident(i) | Tok::Str(i) => i,
                _ => return Err(()),
            };
            let sig = self.sig(":")?;
            self.skip_body();
            let ty = if getter { sig.ret } else { sig.params.into_iter().next().map_or(Type::Any, |p| p.ty) };
            if private {
                return Ok(None);
            }
            return Ok(Some(Member { name, kind: MemberKind::Prop { ty, optional: false, readonly: getter }, doc, is_static }));
        }
        if self.is("[") {
            // `[key: string]: T` is an index signature; `[K in X]: T`, `[Symbol.iterator]()` aren't kept.
            let index = matches!(self.peek_at(1), Tok::Ident(_)) && self.is_at(2, ":");
            if index {
                self.bump();
                self.bump();
                self.bump();
                let key = self.ty()?;
                self.expect("]")?;
                self.eat("?");
                self.expect(":")?;
                let ty = self.ty()?;
                if key == Type::String {
                    return Ok(Some(Member { name: String::new(), kind: MemberKind::Index(ty), doc, is_static }));
                }
                return Ok(None);
            }
            self.skip_group();
            self.eat("?");
            if self.is("(") || self.is("<") {
                self.sig(":")?;
            } else if self.eat(":") {
                self.ty()?;
            }
            return Ok(None);
        }
        if self.eat("#") {
            private = true;
        }
        let name = match self.bump() {
            Tok::Ident(i) | Tok::Str(i) | Tok::Num(i) => i,
            _ => return Err(()),
        };
        let optional = self.eat("?");
        self.eat("!");
        let kind = if self.is("(") || self.is("<") {
            let sig = self.sig(":")?;
            self.skip_body();
            MemberKind::Method { sig }
        } else {
            let ty = if self.eat(":") { self.ty()? } else { Type::Any };
            if self.eat("=") {
                while !self.at_eof() && !self.is(";") && !self.is("}") && !self.token().newline {
                    self.skip_value();
                }
            }
            MemberKind::Prop { ty, optional, readonly }
        };
        if private {
            return Ok(None);
        }
        Ok(Some(Member { name, kind, doc, is_static }))
    }

    // ----- types -----

    fn ty(&mut self) -> R<Type> {
        let t = self.union()?;
        if self.is_kw("extends") && !self.token().newline {
            // A conditional type, `A extends B ? C : D`.
            self.bump();
            self.union()?;
            self.expect("?")?;
            self.ty()?;
            self.expect(":")?;
            self.ty()?;
            return Ok(Type::Other);
        }
        Ok(t)
    }

    fn union(&mut self) -> R<Type> {
        self.eat("|");
        let mut parts = vec![self.inter()?];
        while self.eat("|") {
            parts.push(self.inter()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Type::Union(parts) })
    }

    fn inter(&mut self) -> R<Type> {
        self.eat("&");
        let mut parts = vec![self.unary()?];
        while self.eat("&") {
            parts.push(self.unary()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Type::Inter(parts) })
    }

    fn unary(&mut self) -> R<Type> {
        if self.is_kw("keyof") && !self.is_at(1, ":") {
            self.bump();
            self.unary()?;
            return Ok(Type::Other);
        }
        if self.is_kw("unique") && self.is_kw_at(1, "symbol") {
            self.bump();
            self.bump();
            return Ok(Type::Symbol);
        }
        if self.is_kw("readonly") && !self.is_at(1, ":") && !self.is_at(1, ",") {
            self.bump();
            return self.unary();
        }
        if self.is_kw("infer") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            self.bump();
            if self.is_kw("extends") && !self.is_at(1, "?") {
                // `infer U extends X` inside a conditional's `extends` clause.
                let save = self.pos;
                self.bump();
                if self.union().is_err() || self.is("?") {
                    self.pos = save;
                }
            }
            return Ok(Type::Other);
        }
        if (self.is_kw("new") && (self.is_at(1, "(") || self.is_at(1, "<"))) || (self.is_kw("abstract") && self.is_kw_at(1, "new")) {
            self.eat_kw("abstract");
            self.bump();
            return Ok(Type::Ctor(Box::new(self.sig("=>")?)));
        }
        if self.is("<") {
            return Ok(Type::Fn(Box::new(self.sig("=>")?)));
        }
        if self.is("(") && self.is_function_type() {
            return Ok(Type::Fn(Box::new(self.sig("=>")?)));
        }
        self.ty_primary_postfix()
    }

    /// Whether the `(` at the current position starts a function type rather than a parenthesized type.
    fn is_function_type(&self) -> bool {
        let mut depth = 0usize;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Punct("=>")));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn ty_primary_postfix(&mut self) -> R<Type> {
        let mut t = self.primary()?;
        while self.is("[") && !self.token().newline {
            self.bump();
            if self.eat("]") {
                t = Type::Array(Box::new(t));
            } else {
                // `T[K]`, an indexed access type.
                self.ty()?;
                self.expect("]")?;
                t = Type::Other;
            }
        }
        Ok(t)
    }

    fn type_args(&mut self) -> R<Vec<Type>> {
        let mut args = Vec::new();
        if self.is("<") && !self.token().newline {
            self.bump();
            while !self.eat(">") {
                args.push(self.ty()?);
                if !self.eat(",") && !self.is(">") {
                    return Err(());
                }
            }
        }
        Ok(args)
    }

    fn primary(&mut self) -> R<Type> {
        match self.peek().clone() {
            Tok::Punct("(") => {
                self.bump();
                let t = self.ty()?;
                self.expect(")")?;
                Ok(t)
            }
            Tok::Punct("{") => {
                // A mapped type `{ [K in X]: T }` or an object type.
                if self.is_at(1, "[") && matches!(self.peek_at(2), Tok::Ident(_)) && self.is_kw_at(3, "in") {
                    self.skip_group();
                    return Ok(Type::Other);
                }
                if (self.is_kw_at(1, "readonly") || self.is_at(1, "+") || self.is_at(1, "-")) && self.is_at(2, "[") {
                    self.skip_group();
                    return Ok(Type::Other);
                }
                self.bump();
                Ok(Type::Obj(self.members(false)?))
            }
            Tok::Punct("[") => {
                self.skip_group();
                Ok(Type::Tuple)
            }
            Tok::Punct("-") => {
                self.bump();
                match self.bump() {
                    Tok::Num(_) => Ok(Type::NumLit),
                    _ => Err(()),
                }
            }
            Tok::Str(_) => {
                self.bump();
                Ok(Type::StrLit)
            }
            Tok::Template => {
                self.bump();
                Ok(Type::StrLit)
            }
            Tok::Num(_) => {
                self.bump();
                Ok(Type::NumLit)
            }
            Tok::Ident(word) => {
                if word == "typeof" {
                    self.bump();
                    if self.is_kw("import") {
                        self.bump();
                        self.skip_group();
                        while self.eat(".") {
                            self.ident()?;
                        }
                        return Ok(Type::Other);
                    }
                    let path = self.path()?;
                    self.type_args()?;
                    return Ok(Type::Query(path));
                }
                if word == "import" && self.is_at(1, "(") {
                    self.bump();
                    self.bump();
                    let spec = self.string()?;
                    while !self.eat(")") {
                        self.skip_group();
                    }
                    let mut path = Vec::new();
                    while self.eat(".") {
                        path.push(self.ident()?);
                    }
                    let args = self.type_args()?;
                    return Ok(Type::Import(spec, path, args));
                }
                let simple = match word.as_str() {
                    "any" => Some(Type::Any),
                    "unknown" => Some(Type::Unknown),
                    "never" => Some(Type::Never),
                    "void" => Some(Type::Void),
                    "undefined" => Some(Type::Undefined),
                    "null" => Some(Type::Null),
                    "object" => Some(Type::Object),
                    "string" => Some(Type::String),
                    "number" => Some(Type::Number),
                    "boolean" => Some(Type::Boolean),
                    "bigint" => Some(Type::BigInt),
                    "symbol" => Some(Type::Symbol),
                    "true" | "false" => Some(Type::BoolLit),
                    "this" => Some(Type::This),
                    _ => None,
                };
                if let Some(t) = simple
                    && !self.is_at(1, ".")
                {
                    self.bump();
                    return Ok(t);
                }
                let path = self.path()?;
                let args = self.type_args()?;
                Ok(Type::Ref(path, args))
            }
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(src: &str) -> Vec<Decl> {
        parse(src)
            .into_iter()
            .filter_map(|s| match s {
                Stmt::Decl(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn functions_namespaces_and_export_assignment() {
        let stmts = parse(
            r#"
            import * as core from "core";
            import bodyParser = require("body-parser");
            /** Creates an app. */
            declare function e(): core.Express;
            declare namespace e {
                var json: typeof bodyParser.json;
                export function Router(options?: RouterOptions): core.Router;
                interface RouterOptions { caseSensitive?: boolean | undefined; }
            }
            export = e;
            "#,
        );
        assert_eq!(stmts[0], Stmt::Import { local: "core".into(), spec: "core".into(), what: ImportWhat::Namespace });
        assert_eq!(stmts[1], Stmt::Import { local: "bodyParser".into(), spec: "body-parser".into(), what: ImportWhat::Equals });
        let Stmt::Decl(e) = &stmts[2] else { panic!("{stmts:?}") };
        assert_eq!(e.doc.as_deref(), Some("Creates an app."));
        assert_eq!(
            e.kind,
            DeclKind::Function(Sig {
                tparams: vec![],
                params: vec![],
                ret: Type::Ref(vec!["core".into(), "Express".into()], vec![]),
                untyped: false
            })
        );
        let Stmt::Decl(Decl { kind: DeclKind::Namespace(body), .. }) = &stmts[3] else { panic!() };
        assert_eq!(body.len(), 3);
        assert_eq!(stmts[4], Stmt::ExportAssign(vec!["e".into()]));
    }

    #[test]
    fn interface_members() {
        let d = decls(
            r#"
            export interface IRouter<T = string> extends RequestHandler, Other<T> {
                (req: Request, res: Response): any;
                new (x: number): IRouter;
                readonly path: string;
                readonly: boolean
                get: IRouterMatcher<this, "get">;
                "m-search": IRouterMatcher<this>;
                [key: string]: any;
                [Symbol.iterator](): Iterator<string>;
                route<T extends string | RegExp>(prefix: T): IRoute<T>;
                accepts(...type: string[]): string | false;
                get size(): number;
                private secret: string;
            }
            "#,
        );
        let DeclKind::Interface(i) = &d[0].kind else { panic!() };
        assert_eq!(i.tparams[0].default, Some(Type::String));
        assert_eq!(i.extends.len(), 2);
        let names: Vec<&str> = i.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["", "", "path", "readonly", "get", "m-search", "", "route", "accepts", "size"]);
        assert!(matches!(i.members[2].kind, MemberKind::Prop { readonly: true, .. }));
        assert!(matches!(i.members[3].kind, MemberKind::Prop { readonly: false, ty: Type::Boolean, .. }));
        let MemberKind::Method { sig } = &i.members[8].kind else { panic!() };
        assert!(sig.params[0].rest);
        assert_eq!(sig.params[0].ty, Type::Array(Box::new(Type::String)));
    }

    #[test]
    fn types() {
        let d = decls(
            r#"
            type A = (err: any, req: Request, ...rest: any[]) => unknown;
            type B = string | RegExp | Array<string | RegExp>;
            type C<S extends string> = S extends `${infer P}/` ? P : S;
            type D = { [P in Keys]: string } & Partial<X>;
            type E = (A | B)[];
            type F = typeof import("x").y;
            type G = import("./types").Opts<string>;
            type H = (x: number) => x is string;
            "#,
        );
        assert_eq!(d.len(), 8);
        assert!(matches!(&d[0].kind, DeclKind::Alias { ty: Type::Fn(sig), .. } if sig.params.len() == 3 && sig.params[2].rest));
        assert!(matches!(&d[1].kind, DeclKind::Alias { ty: Type::Union(parts), .. } if parts.len() == 3));
        assert!(matches!(&d[2].kind, DeclKind::Alias { ty: Type::Other, .. }));
        assert!(matches!(&d[3].kind, DeclKind::Alias { ty: Type::Inter(parts), .. } if parts[0] == Type::Other));
        assert!(matches!(&d[4].kind, DeclKind::Alias { ty: Type::Array(_), .. }));
        assert!(
            matches!(&d[6].kind, DeclKind::Alias { ty: Type::Import(spec, path, args), .. } if spec == "./types" && path == &["Opts"] && args.len() == 1)
        );
        assert!(matches!(&d[7].kind, DeclKind::Alias { ty: Type::Fn(sig), .. } if sig.ret == Type::Boolean));
    }

    #[test]
    fn a_broken_statement_is_skipped() {
        let d =
            decls("declare function broken(: void;\nexport declare class Ok { constructor(a: string); go(): void; static make(): Ok; }");
        assert_eq!(d.len(), 1);
        let DeclKind::Interface(c) = &d[0].kind else { panic!() };
        assert!(c.is_class);
        assert!(matches!(c.members[0].kind, MemberKind::Ctor(_)));
        assert!(c.members[2].is_static);
    }

    #[test]
    fn es_module_exports() {
        let stmts = parse(
            r#"
            export { a, b as c } from "./x";
            export * from "./y";
            export * as z from "./z";
            export default function pad(s: string, n: number): string;
            export type { T } from "./t";
            "#,
        );
        assert_eq!(
            stmts[0],
            Stmt::ExportNames { spec: Some("./x".into()), names: vec![("a".into(), "a".into()), ("b".into(), "c".into())] }
        );
        assert_eq!(stmts[1], Stmt::ExportStar { spec: "./y".into(), alias: None });
        assert_eq!(stmts[2], Stmt::ExportStar { spec: "./z".into(), alias: Some("z".into()) });
        assert!(matches!(&stmts[3], Stmt::Decl(Decl { name, default: true, .. }) if name == "pad"));
        assert!(matches!(&stmts[4], Stmt::ExportNames { names, .. } if names[0].0 == "T"));
    }
}
