//! Name resolution and type inference over the [`super::index`]es.
//!
//! Names resolve the way Kotlin scopes them: local declarations and parameters, `it`, the
//! implicit receivers (lambdas with receivers, extension functions, enclosing classes with
//! their supertypes and companions), the file, its explicit imports, its package, star imports
//! and Kotlin's default imports. Types are inferred from declarations and from expressions as
//! far as completion and navigation need: literals, calls with their generic return types
//! (bound from receivers, explicit type arguments and value arguments), member chains,
//! operators, `for` loop elements, destructuring and lambda parameters.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};

use super::index::{Index, Origin, Platforms, SetKind, SourceSet, SymId, Unit, UnitId, default_imports};
use super::java;
use super::lexer::{HARD_KEYWORDS, Lexed, Tok};
use super::syntax::{self, Decl, DeclId, File, InitKind, Kind, ScopeKind, TypeRef};

/// A type, as far as Kargo knows it; unknown parts are `None`.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// An instance of a class, interface or object.
    Class { sym: SymId, args: Vec<Option<Ty>>, nullable: bool },
    /// A function type.
    Func { receiver: Option<Box<Ty>>, params: Vec<Option<Ty>>, ret: Option<Box<Ty>> },
    /// A class itself, as in `Foo.bar`: its companion, nested classes and enum entries.
    Static(SymId),
    /// A package in a qualified name: `kotlin.collections.`.
    Package(String),
}

/// Type arguments by type parameter name.
pub type Subst = HashMap<String, Ty>;

/// A declaration found for a name, with the type arguments of the receiver it was found on.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub sym: SymId,
    pub subst: Subst,
}

impl Member {
    fn plain(sym: SymId) -> Self {
        Member { sym, subst: Subst::new() }
    }
}

/// What a name refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Sym(Member),
    /// The implicit `it` of the lambda whose `{` is token `brace`.
    It {
        unit: UnitId,
        brace: usize,
    },
}

/// The types a lambda gets from the function it is passed to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LambdaCtx {
    pub receiver: Option<Ty>,
    pub params: Vec<Option<Ty>>,
    /// The function the lambda is passed to.
    pub callee: Option<SymId>,
}

/// Where a type reference is resolved: a file, and the declaration it appears in (for type
/// parameters and nested classes).
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub unit: UnitId,
    pub decl: Option<DeclId>,
}

/// One argument of a call, as tokens.
#[derive(Debug, Clone)]
pub struct Arg {
    pub start: usize,
    pub end: usize,
    pub name: Option<String>,
    /// A lambda literal; `start` is its `{`.
    pub lambda: bool,
    /// A lambda after the parentheses.
    pub trailing: bool,
}

/// The project's packages: which packages each one sees (itself and its path dependencies), and
/// the platforms of its targets.
#[derive(Debug, Clone, Default)]
pub struct Packages {
    pub visible: Vec<Vec<usize>>,
    /// Per package, the platforms of its enabled targets. Common code (in `src/`) compiles for
    /// all of them, so it sees only the libraries on all of them.
    pub platforms: Vec<Platforms>,
}

pub struct Analyzer<'a> {
    pub lib: &'a Index,
    pub proj: &'a Index,
    pub packages: &'a Packages,
    depth: Cell<u32>,
    lambdas: RefCell<HashMap<(UnitId, usize), Option<LambdaCtx>>>,
    values: RefCell<HashMap<SymId, Option<Ty>>>,
}

const MAX_DEPTH: u32 = 24;

impl<'a> Analyzer<'a> {
    pub fn new(lib: &'a Index, proj: &'a Index, packages: &'a Packages) -> Self {
        Self { lib, proj, packages, depth: Cell::new(0), lambdas: RefCell::default(), values: RefCell::default() }
    }

    pub fn index_of(&self, lib: bool) -> &'a Index {
        if lib { self.lib } else { self.proj }
    }

    pub fn unit(&self, id: UnitId) -> &'a Unit {
        self.index_of(id.lib).unit(id).expect("a unit an index holds")
    }

    pub fn file(&self, id: UnitId) -> &'a File {
        &self.unit(id).file
    }

    pub fn decl(&self, s: SymId) -> &'a Decl {
        &self.file(s.unit_id()).decls[s.decl_id()]
    }

    fn enter(&self) -> bool {
        let d = self.depth.get();
        if d >= MAX_DEPTH {
            return false;
        }
        self.depth.set(d + 1);
        true
    }

    fn leave(&self) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }

    /// The fully qualified name of a declaration; local ones get a name unique to them.
    pub fn fq(&self, s: SymId) -> String {
        let d = self.decl(s);
        if d.local || d.name.is_empty() || matches!(d.kind, Kind::Parameter | Kind::Variable) {
            return format!("{}#{}:{}:{}", d.name, s.lib, s.unit, s.decl);
        }
        self.file(s.unit_id()).fq_name(s.decl_id())
    }

    /// The package a declaration is in.
    pub fn package_of(&self, s: SymId) -> &'a str {
        &self.file(s.unit_id()).package
    }

    /// The classes and declarations with the fully qualified name `fq`. A Java platform class
    /// is found as the Kotlin class it maps to (`java.lang.String` is `kotlin.String`).
    pub fn by_fq(&self, fq: &str) -> Vec<SymId> {
        if let Some(kotlin) = java::kotlin_name(fq) {
            let mapped = self.by_fq(kotlin);
            if !mapped.is_empty() {
                return mapped;
            }
        }
        let mut out: Vec<SymId> = self.proj.by_fq.get(fq).cloned().unwrap_or_default();
        out.extend(self.lib.by_fq.get(fq).into_iter().flatten().copied());
        out.extend(self.lib.jdk_by_fq(fq));
        out
    }

    /// The declarations with the simple name `name` in the project and the libraries.
    fn by_name(&self, name: &str) -> Vec<SymId> {
        let mut out: Vec<SymId> =
            self.proj.by_name.get(name).into_iter().flatten().chain(self.lib.by_name.get(name).into_iter().flatten()).copied().collect();
        out.extend(self.lib.jdk_named(name));
        out
    }

    pub fn has_package(&self, package: &str) -> bool {
        self.proj.packages.contains(package) || self.lib.packages.contains(package)
    }

    // ----- visibility -----

    /// Whether code in `from` can use `s`.
    pub fn visible(&self, from: UnitId, s: SymId) -> bool {
        let d = self.decl(s);
        if d.hidden {
            return false;
        }
        if d.is_private() {
            return s.unit_id() == from;
        }
        let target = self.unit(s.unit_id());
        let source = self.unit(from);
        match (&source.origin, &target.origin) {
            (_, Origin::Library { main, test, .. }) => !d.is_internal() && self.library_in_scope(&source.origin, *main, *test),
            (_, Origin::Loose) => s.unit_id() == from,
            (Origin::Library { .. }, _) => false,
            (Origin::Loose, Origin::Source(t)) => t.kind == SetKind::Main,
            (Origin::Source(f), Origin::Source(t)) => {
                let kind_ok = t.kind == SetKind::Main || (t.kind == SetKind::Test && f.kind == SetKind::Test);
                let target_ok = t.target.is_none() || t.target == f.target;
                if f.package == t.package {
                    kind_ok && target_ok
                } else {
                    // `internal` stays inside the package (the Gradle module) that declares it.
                    t.kind == SetKind::Main
                        && target_ok
                        && !d.is_internal()
                        && self.packages.visible.get(f.package).is_some_and(|v| v.contains(&t.package))
                }
            }
        }
    }

    /// Whether code of origin `from` sees a library file on the platforms `main` (main code) and
    /// `test` (test code only). A source set sees it when it compiles for all of its platforms,
    /// and test code sees the test-only libraries too. A library file sees the libraries it
    /// shares a platform with.
    fn library_in_scope(&self, from: &Origin, main: Platforms, test: Platforms) -> bool {
        match from {
            Origin::Source(set) => {
                let scope = if set.kind == SetKind::Test { main.union(test) } else { main };
                scope.contains(self.platforms_of(set))
            }
            Origin::Library { main: from_main, test: from_test, .. } => from_main.union(*from_test).intersects(main.union(test)),
            Origin::Loose => true,
        }
    }

    /// The platforms the code of `set` compiles for: its target's, or every target's for common
    /// code, which compiles for each of them.
    fn platforms_of(&self, set: &SourceSet) -> Platforms {
        match &set.target {
            Some(target) => Platforms::of_target(target),
            None => match self.packages.platforms.get(set.package) {
                Some(platforms) if !platforms.is_empty() => *platforms,
                _ => Platforms::ALL,
            },
        }
    }

    /// Whether a top-level declaration is in scope in `from` without an import.
    fn imported(&self, from: UnitId, s: SymId) -> bool {
        let file = self.file(from);
        let package = self.package_of(s);
        if package == file.package || default_imports(self.unit(from)).contains(&package) {
            return true;
        }
        let name = &self.decl(s).name;
        let fq = self.fq(s);
        file.imports.iter().any(|i| if i.star { i.fq() == package } else { i.fq() == fq && i.path.last() == Some(name) })
    }

    // ----- types -----

    /// The type a type reference denotes in `ctx`, with type parameters replaced from `subst`.
    pub fn ty_of_ref(&self, tr: &TypeRef, ctx: Ctx, subst: &Subst) -> Option<Ty> {
        if !self.enter() {
            return None;
        }
        let out = self.ty_of_ref_inner(tr, ctx, subst);
        self.leave();
        out
    }

    fn ty_of_ref_inner(&self, tr: &TypeRef, ctx: Ctx, subst: &Subst) -> Option<Ty> {
        if let Some(f) = &tr.func {
            return Some(Ty::Func {
                receiver: f.receiver.as_ref().and_then(|r| self.ty_of_ref(r, ctx, subst)).map(Box::new),
                params: f.params.iter().map(|p| self.ty_of_ref(p, ctx, subst)).collect(),
                ret: self.ty_of_ref(&f.ret, ctx, subst).map(Box::new),
            });
        }
        if tr.name.is_empty() || tr.name == "*" {
            return None;
        }
        if !tr.name.contains('.') {
            if let Some(t) = subst.get(&tr.name) {
                return Some(nullable(t.clone(), tr.nullable));
            }
            if let Some((owner, bound)) = self.type_param_bound(ctx, &tr.name) {
                // A generic upper bound stands for the type parameter: `fun <T : CharSequence> f(x: T)`.
                let bound = bound?;
                let owner_ctx = Ctx { unit: ctx.unit, decl: Some(owner) };
                let ty = if bound.name == tr.name || bound.args.iter().any(|a| a.name == tr.name) {
                    // `T : Comparable<T>`: the class, without the type argument that refers back.
                    self.instance(self.resolve_type(owner_ctx, &bound.name)?)
                } else {
                    self.ty_of_ref(bound, owner_ctx, subst)?
                };
                return Some(nullable(ty, tr.nullable));
            }
        }
        let sym = self.resolve_type(ctx, &tr.name)?;
        let d = self.decl(sym);
        if d.kind == Kind::TypeAlias {
            let alias_subst: Subst =
                d.type_params.iter().zip(&tr.args).filter_map(|(p, a)| Some((p.name.clone(), self.ty_of_ref(a, ctx, subst)?))).collect();
            let alias_ctx = Ctx { unit: sym.unit_id(), decl: Some(sym.decl_id()) };
            if let Some(t) = d.ty.as_ref().and_then(|t| self.ty_of_ref(t, alias_ctx, &alias_subst)) {
                return Some(nullable(t, tr.nullable));
            }
        }
        let args = tr.args.iter().map(|a| self.ty_of_ref(a, ctx, subst)).collect();
        Some(Ty::Class { sym, args, nullable: tr.nullable })
    }

    /// The declaration that declares the type parameter `name` visible in `ctx`, and its upper bound.
    fn type_param_bound(&self, ctx: Ctx, name: &str) -> Option<(DeclId, Option<&'a TypeRef>)> {
        let file = self.file(ctx.unit);
        let mut d = ctx.decl;
        while let Some(id) = d {
            let decl = &file.decls[id];
            if let Some(t) = decl.type_params.iter().find(|t| t.name == name) {
                return Some((id, t.bound.as_ref()));
            }
            d = decl.parent;
        }
        None
    }

    /// The class (or type alias) a type name denotes in `ctx`.
    pub fn resolve_type(&self, ctx: Ctx, name: &str) -> Option<SymId> {
        let file = self.file(ctx.unit);
        if let Some((first, rest)) = name.split_once('.') {
            if let Some(s) = self.best_type(self.by_fq(name).into_iter()) {
                return Some(s);
            }
            let mut s = self.resolve_type(Ctx { decl: ctx.decl, ..ctx }, first)?;
            for seg in rest.split('.') {
                s = self.nested_class(s, seg)?;
            }
            return Some(s);
        }
        let mut d = ctx.decl;
        while let Some(id) = d {
            let decl = &file.decls[id];
            if decl.kind.is_class() {
                if decl.name == name {
                    return Some(ctx.unit.sym(id));
                }
                if let Some(s) = self.nested_class(ctx.unit.sym(id), name) {
                    return Some(s);
                }
            }
            for &local in self.local_classes(ctx.unit, id) {
                if file.decls[local].name == name {
                    return Some(ctx.unit.sym(local));
                }
            }
            d = decl.parent;
        }
        let local = file.top.iter().copied().find(|&t| file.decls[t].name == name && is_type_decl(&file.decls[t]));
        if let Some(t) = local {
            return Some(ctx.unit.sym(t));
        }
        self.global_type(ctx.unit, name)
    }

    /// Local classes declared in the body of `decl`.
    fn local_classes(&self, unit: UnitId, decl: DeclId) -> &'a [DeclId] {
        let file = self.file(unit);
        match file.decls[decl].body {
            Some(scope) if matches!(file.scopes[scope].kind, ScopeKind::Function(_)) => &file.scopes[scope].decls,
            _ => &[],
        }
    }

    fn global_type(&self, from: UnitId, name: &str) -> Option<SymId> {
        let file = self.file(from);
        for import in file.imports.iter().filter(|i| i.name() == Some(name)) {
            if let Some(s) = self.best_type(self.import_targets(import.fq()).into_iter().filter(|&s| is_type_decl(self.decl(s)))) {
                return Some(s);
            }
        }
        let candidates: Vec<SymId> =
            self.by_name(name).into_iter().filter(|&s| is_type_decl(self.decl(s)) && self.visible(from, s)).collect();
        let in_package = |p: &str| self.best_type(candidates.iter().copied().filter(|&s| self.package_of(s) == p));
        // A class named `p.name`, which may be a Java platform class that Kotlin names elsewhere.
        let named_in = |p: &str| {
            self.best_type(self.by_fq(&format!("{p}.{name}")).into_iter().filter(|&s| self.visible(from, s) && is_type_decl(self.decl(s))))
        };
        if let Some(s) = in_package(&file.package) {
            return Some(s);
        }
        for import in file.imports.iter().filter(|i| i.star) {
            if let Some(s) = in_package(&import.fq()).or_else(|| named_in(&import.fq())) {
                return Some(s);
            }
            if let Some(class) = self.best_type(self.by_fq(&import.fq()).into_iter())
                && let Some(s) = self.nested_class(class, name)
            {
                return Some(s);
            }
        }
        default_imports(self.unit(from)).iter().find_map(|p| in_package(p).or_else(|| named_in(p)))
    }

    /// What `import a.b.C` names: classes, functions and properties, or members of an object.
    pub fn import_targets(&self, fq: String) -> Vec<SymId> {
        let direct = self.by_fq(&fq);
        if !direct.is_empty() {
            return direct;
        }
        let Some((owner, name)) = fq.rsplit_once('.') else { return Vec::new() };
        let mut out = Vec::new();
        for class in self.by_fq(owner).into_iter().filter(|&s| self.decl(s).kind.is_class()) {
            for &m in &self.decl(class).members {
                if self.file(class.unit_id()).decls[m].name == name {
                    out.push(class.unit_id().sym(m));
                }
            }
        }
        out
    }

    /// Among declarations of one type name, the best to show: a class over a type alias (an
    /// `expect class` over its JVM `actual typealias`), then the one with documentation.
    pub fn best_type(&self, candidates: impl Iterator<Item = SymId>) -> Option<SymId> {
        candidates.min_by_key(|&s| {
            let d = self.decl(s);
            (!d.kind.is_class(), d.doc.is_none(), d.members.is_empty())
        })
    }

    fn nested_class(&self, class: SymId, name: &str) -> Option<SymId> {
        for c in self.class_decls(class) {
            let file = self.file(c.unit_id());
            for &m in &self.decl(c).members {
                if file.decls[m].name == name && is_type_decl(&file.decls[m]) {
                    return Some(c.unit_id().sym(m));
                }
            }
        }
        None
    }

    /// Every declaration of the class `s` (the `expect` and `actual` ones), `s` first.
    pub fn class_decls(&self, s: SymId) -> Vec<SymId> {
        let mut out = vec![s];
        let d = self.decl(s);
        if !d.local && !d.name.is_empty() {
            for other in self.by_fq(&self.fq(s)) {
                if other != s && self.decl(other).kind.is_class() {
                    out.push(other);
                }
            }
        }
        out
    }

    /// The type of an instance of the class `s`, with unknown type arguments.
    pub fn instance(&self, s: SymId) -> Ty {
        Ty::Class { sym: s, args: vec![None; self.decl(s).type_params.len()], nullable: false }
    }

    fn std_class(&self, fq: &str) -> Option<SymId> {
        self.best_type(self.by_fq(fq).into_iter())
    }

    fn std_ty(&self, fq: &str) -> Option<Ty> {
        self.std_class(fq).map(|s| Ty::Class { sym: s, args: Vec::new(), nullable: false })
    }

    /// The class `s` and its supertypes, each with the type arguments it has there.
    pub fn closure(&self, ty: &Ty) -> Vec<(SymId, Subst)> {
        let (start, args) = match ty {
            Ty::Class { sym, args, .. } => (*sym, args.clone()),
            Ty::Func { .. } => match self.std_class("kotlin.Function") {
                Some(s) => (s, Vec::new()),
                None => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((start, self.class_subst(start, &args)));
        while let Some((sym, subst)) = queue.pop_front() {
            if !seen.insert(self.fq(sym)) || out.len() > 64 {
                continue;
            }
            // A Java class is its own; looking up its name would give the Kotlin class it maps to.
            let classes = if self.is_platform(sym) { vec![sym] } else { self.class_decls(sym) };
            for c in classes {
                let d = self.decl(c);
                let ctx = Ctx { unit: c.unit_id(), decl: Some(c.decl_id()) };
                for st in &d.supertypes {
                    if let Some(Ty::Class { sym: s2, args: a2, .. }) = self.ty_of_ref(st, ctx, &subst) {
                        queue.push_back((s2, self.class_subst(s2, &a2)));
                    }
                }
                if d.kind == Kind::Enum
                    && let Some(e) = self.std_class("kotlin.Enum")
                {
                    let mut s = Subst::new();
                    s.insert("E".into(), self.instance(c));
                    queue.push_back((e, s));
                }
                out.push((c, subst.clone()));
            }
            // The members of the Java class that a Kotlin class also shows, with its type arguments.
            if let Some(java) = java::java_counterpart(&self.fq(sym))
                .and_then(|fq| self.lib.jdk_by_fq(fq).into_iter().find(|&s| is_type_decl(self.decl(s))))
            {
                let args: Vec<Option<Ty>> = self.decl(sym).type_params.iter().map(|p| subst.get(&p.name).cloned()).collect();
                queue.push_back((java, self.class_subst(java, &args)));
            }
        }
        if let Some(any) = self.std_class("kotlin.Any")
            && !seen.contains("kotlin.Any")
        {
            out.push((any, Subst::new()));
        }
        out
    }

    /// Whether `s` is a Java class that Kotlin maps to its own, or whose members a Kotlin class shows.
    fn is_platform(&self, s: SymId) -> bool {
        s.lib && java::is_platform_class(&self.fq(s))
    }

    fn class_subst(&self, class: SymId, args: &[Option<Ty>]) -> Subst {
        let d = self.decl(class);
        d.type_params.iter().zip(args).filter_map(|(p, a)| Some((p.name.clone(), a.clone()?))).collect()
    }

    /// The members of a type (inherited ones too) and the extensions in scope in `from`; with
    /// `all`, extensions that need an import as well.
    pub fn members(&self, ty: &Ty, from: UnitId, all: bool) -> Vec<Member> {
        let mut out = Vec::new();
        let mut keys = HashSet::new();
        let mut push = |this: &Self, m: Member, out: &mut Vec<Member>| {
            if this.visible(from, m.sym) && keys.insert(this.member_key(m.sym)) {
                out.push(m);
            }
        };
        match ty {
            Ty::Static(class) => {
                for c in self.class_decls(*class) {
                    let file = self.file(c.unit_id());
                    let d = self.decl(c);
                    for &m in &d.members {
                        let md = &file.decls[m];
                        // Nested classes, enum entries, and the static members of a Java class.
                        let static_member = md.has("static") && matches!(md.kind, Kind::Function | Kind::Property);
                        if md.kind.is_class() && md.kind != Kind::Companion || md.kind == Kind::EnumEntry {
                            push(self, Member::plain(c.unit_id().sym(m)), &mut out);
                        } else if static_member {
                            push(self, Member { sym: c.unit_id().sym(m), subst: Subst::new() }, &mut out);
                        }
                    }
                    // `Color.values()`, `Color.entries`.
                    for &g in d.synthetic.iter().filter(|&&g| file.decls[g].has("static")) {
                        push(self, Member::plain(c.unit_id().sym(g)), &mut out);
                    }
                    if matches!(d.kind, Kind::Object | Kind::Companion) {
                        for m in self.members(&self.instance(c), from, all) {
                            push(self, m, &mut out);
                        }
                    } else if let Some(companion) = file.companion(c.decl_id()) {
                        for m in self.members(&self.instance(c.unit_id().sym(companion)), from, all) {
                            push(self, m, &mut out);
                        }
                    }
                }
            }
            Ty::Class { .. } | Ty::Func { .. } => {
                let closure = self.closure(ty);
                // A Java member is shown only when Kotlin declares or offers no member of that name:
                // Kotlin's `lines()` is an extension that wins over Java's.
                let mut kotlin_names: HashSet<String> = HashSet::new();
                if closure.iter().any(|(c, _)| self.is_platform(*c)) {
                    for (c, _) in closure.iter().filter(|(c, _)| !self.is_platform(*c)) {
                        let file = self.file(c.unit_id());
                        for &m in &self.decl(*c).members {
                            if matches!(file.decls[m].kind, Kind::Function | Kind::Property) {
                                kotlin_names.insert(file.decls[m].name.clone());
                            }
                        }
                    }
                    kotlin_names.extend(self.extensions(ty, &closure, from, true).iter().map(|m| self.decl(m.sym).name.clone()));
                }
                // A nullable receiver has only the extensions of nullable types; `?.` unwraps it.
                let nullable_receiver = matches!(ty, Ty::Class { nullable: true, .. });
                for (c, subst) in closure.iter().filter(|_| !nullable_receiver) {
                    let platform = self.is_platform(*c);
                    let file = self.file(c.unit_id());
                    for &m in &self.decl(*c).members {
                        let md = &file.decls[m];
                        if !matches!(md.kind, Kind::Function | Kind::Property) {
                            continue;
                        }
                        if platform && (md.has("static") || kotlin_names.contains(&md.name)) {
                            continue;
                        }
                        push(self, Member { sym: c.unit_id().sym(m), subst: subst.clone() }, &mut out);
                    }
                    // `copy`, `component1` and the other members the compiler generates.
                    for &g in self.decl(*c).synthetic.iter().filter(|&&g| !file.decls[g].has("static")) {
                        push(self, Member { sym: c.unit_id().sym(g), subst: subst.clone() }, &mut out);
                    }
                }
                for m in self.extensions(ty, &closure, from, all) {
                    push(self, m, &mut out);
                }
            }
            Ty::Package(_) => {}
        }
        out
    }

    /// A key that overrides, `expect`/`actual` pairs and duplicates share.
    fn member_key(&self, s: SymId) -> String {
        let d = self.decl(s);
        let file = self.file(s.unit_id());
        let text = &self.unit(s.unit_id()).text;
        let params: Vec<String> =
            d.params.iter().map(|&p| file.decls[p].ty.as_ref().map(|t| t.display(text)).unwrap_or_default()).collect();
        let receiver = d.receiver.as_ref().map(|r| r.display(text)).unwrap_or_default();
        format!("{:?}|{}|{}|{}", value_kind(d.kind), receiver, d.name, params.join(","))
    }

    fn extensions(&self, ty: &Ty, closure: &[(SymId, Subst)], from: UnitId, all: bool) -> Vec<Member> {
        let nullable_receiver = matches!(ty, Ty::Class { nullable: true, .. });
        let mut out = Vec::new();
        let mut names: Vec<(&str, Ty)> = Vec::new();
        for (c, subst) in closure {
            let d = self.decl(*c);
            let args = d.type_params.iter().map(|p| subst.get(&p.name).cloned()).collect();
            names.push((&d.name, Ty::Class { sym: *c, args, nullable: false }));
        }
        let simple_names: HashSet<&str> = names.iter().map(|(n, _)| *n).collect();
        for index in [self.proj, self.lib] {
            for (name, class_ty) in &names {
                for &e in index.extensions.get(*name).into_iter().flatten() {
                    if !all && !self.imported(from, e) {
                        continue;
                    }
                    let d = self.decl(e);
                    let Some(receiver) = &d.receiver else { continue };
                    if nullable_receiver && !receiver.nullable {
                        continue;
                    }
                    let mut subst = Subst::new();
                    let tparams: Vec<String> = d.type_params.iter().map(|t| t.name.clone()).collect();
                    self.unify(receiver, class_ty, &tparams, &mut subst);
                    out.push(Member { sym: e, subst });
                }
            }
            for &e in index.extensions.get("*").into_iter().flatten() {
                if !all && !self.imported(from, e) {
                    continue;
                }
                let d = self.decl(e);
                let Some(receiver) = &d.receiver else { continue };
                if receiver.func.is_some() != matches!(ty, Ty::Func { .. }) {
                    continue;
                }
                // `fun <T : Any> T.f()` is not for a nullable receiver.
                if nullable_receiver
                    && !receiver.nullable
                    && d.type_params.iter().find(|t| t.name == receiver.name).and_then(|t| t.bound.as_ref()).is_some_and(|b| !b.nullable)
                {
                    continue;
                }
                let bound_ok = d.type_params.iter().find(|t| t.name == receiver.name).and_then(|t| t.bound.as_ref()).is_none_or(|b| {
                    let simple = b.simple_name();
                    simple == "Any" || simple_names.contains(simple)
                });
                if !bound_ok {
                    continue;
                }
                let mut subst = Subst::new();
                if receiver.func.is_none() {
                    subst.insert(receiver.name.clone(), strip_nullable(ty.clone()));
                }
                out.push(Member { sym: e, subst });
            }
        }
        out
    }

    /// Binds the type parameters `tparams` that `pattern` mentions from `actual`.
    pub fn unify(&self, pattern: &TypeRef, actual: &Ty, tparams: &[String], subst: &mut Subst) {
        if pattern.func.is_some() {
            if let (Some(f), Ty::Func { params, ret, .. }) = (&pattern.func, actual) {
                for (p, a) in f.params.iter().zip(params) {
                    if let Some(a) = a {
                        self.unify(p, a, tparams, subst);
                    }
                }
                if let Some(r) = ret {
                    self.unify(&f.ret, r, tparams, subst);
                }
            }
            return;
        }
        if tparams.contains(&pattern.name) {
            subst.entry(pattern.name.clone()).or_insert_with(|| strip_nullable(actual.clone()));
            return;
        }
        if pattern.args.is_empty() || !matches!(actual, Ty::Class { .. }) {
            return;
        }
        let simple = pattern.simple_name();
        for (c, s) in self.closure(actual) {
            let d = self.decl(c);
            if d.name == simple {
                for (pa, tp) in pattern.args.iter().zip(&d.type_params) {
                    if let Some(a) = s.get(&tp.name) {
                        self.unify(pa, a, tparams, subst);
                    }
                }
                return;
            }
        }
    }

    /// The members named `name` of a type.
    pub fn members_named(&self, ty: &Ty, name: &str, from: UnitId) -> Vec<Member> {
        let mut found: Vec<Member> = self.members(ty, from, false).into_iter().filter(|m| self.decl(m.sym).name == name).collect();
        if found.is_empty() {
            // An extension that isn't imported: still worth navigating to.
            found = self.members(ty, from, true).into_iter().filter(|m| self.decl(m.sym).name == name).collect();
        }
        found
    }

    // ----- values -----

    /// The type of a value declaration, or what a function returns, with `subst` applied.
    pub fn value_ty(&self, s: SymId, subst: &Subst) -> Option<Ty> {
        let cacheable = subst.is_empty();
        if cacheable && let Some(t) = self.values.borrow().get(&s) {
            return t.clone();
        }
        if !self.enter() {
            return None;
        }
        if cacheable {
            self.values.borrow_mut().insert(s, None);
        }
        let out = self.value_ty_inner(s, subst);
        self.leave();
        if cacheable {
            self.values.borrow_mut().insert(s, out.clone());
        }
        out
    }

    fn value_ty_inner(&self, s: SymId, subst: &Subst) -> Option<Ty> {
        let d = self.decl(s);
        let u = s.unit_id();
        let ctx = Ctx { unit: u, decl: Some(s.decl_id()) };
        let file = self.file(u);
        match d.kind {
            Kind::Object | Kind::Companion => Some(self.instance(s)),
            k if k.is_class() => Some(Ty::Static(s)),
            Kind::TypeAlias => self.ty_of_ref(d.ty.as_ref()?, ctx, subst).map(|t| match t {
                Ty::Class { sym, .. } => Ty::Static(sym),
                t => t,
            }),
            Kind::EnumEntry => file.owner_class(s.decl_id()).map(|c| self.instance(u.sym(c))),
            Kind::Constructor => file.owner_class(s.decl_id()).map(|c| self.instance(u.sym(c))),
            Kind::Function => self.return_ty(s, subst),
            _ => {
                if let Some(t) = &d.ty {
                    let ty = self.ty_of_ref(t, ctx, subst)?;
                    if d.vararg {
                        let array = self.std_class("kotlin.Array")?;
                        return Some(Ty::Class { sym: array, args: vec![Some(ty)], nullable: false });
                    }
                    return Some(ty);
                }
                let src = &self.unit(u).text;
                let lx = &file.lexed;
                match d.init_kind {
                    InitKind::Value => {
                        let start = d.init?;
                        self.infer(u, start, syntax::expr_end(src, lx, start, lx.tokens.len()))
                    }
                    InitKind::Delegate => {
                        let start = d.init?;
                        // `by lazy { ... }`: what the lambda returns.
                        if lx.is(src, start, "lazy") {
                            let brace = (start + 1..start + 6).find(|&i| lx.kind(i) == Some(Tok::LBrace))?;
                            return self.lambda_result(u, brace);
                        }
                        // `by map`, `by Delegates.observable(...)`, a custom delegate: its `getValue`.
                        let delegate = self.infer(u, start, syntax::expr_end(src, lx, start, lx.tokens.len()))?;
                        let m =
                            self.members_named(&delegate, "getValue", u).into_iter().find(|m| self.decl(m.sym).kind == Kind::Function)?;
                        self.value_ty(m.sym, &m.subst)
                    }
                    InitKind::Iterated => {
                        let start = d.init?;
                        let end = self.iterated_end(u, start);
                        self.element_ty(&self.infer(u, start, end)?, u)
                    }
                    InitKind::IteratedComponent(n) => {
                        let start = d.init?;
                        let end = self.iterated_end(u, start);
                        let element = self.element_ty(&self.infer(u, start, end)?, u)?;
                        self.component_ty(&element, n, u)
                    }
                    InitKind::Component(n) => {
                        let start = d.init?;
                        let value = self.infer(u, start, syntax::expr_end(src, lx, start, lx.tokens.len()))?;
                        self.component_ty(&value, n, u)
                    }
                    InitKind::LambdaParam(n) => self.lambda_ctx(u, d.init?)?.params.get(n).cloned().flatten(),
                    InitKind::LambdaComponent(n, m) => {
                        let param = self.lambda_ctx(u, d.init?)?.params.get(n).cloned().flatten()?;
                        self.component_ty(&param, m, u)
                    }
                }
            }
        }
    }

    /// The end of the iterated expression of a `for` loop starting at `start`: its `)`.
    fn iterated_end(&self, u: UnitId, start: usize) -> usize {
        let lx = &self.file(u).lexed;
        let mut i = start;
        while i < lx.tokens.len() {
            match lx.tokens[i].kind {
                Tok::RParen => return i,
                Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen => i = lx.matching[i].map_or(lx.tokens.len(), |c| c + 1),
                _ => i += 1,
            }
        }
        i
    }

    /// What a function returns.
    pub fn return_ty(&self, s: SymId, subst: &Subst) -> Option<Ty> {
        let d = self.decl(s);
        let u = s.unit_id();
        if let Some(t) = &d.ty {
            return self.ty_of_ref(t, Ctx { unit: u, decl: Some(s.decl_id()) }, subst);
        }
        if let Some(start) = d.init {
            let file = self.file(u);
            let end = syntax::expr_end(&self.unit(u).text, &file.lexed, start, file.lexed.tokens.len());
            return self.infer(u, start, end);
        }
        self.std_ty("kotlin.Unit")
    }

    /// The type of the elements a `for` loop over `ty` gets: `iterator().next()`.
    pub fn element_ty(&self, ty: &Ty, from: UnitId) -> Option<Ty> {
        let iterator = self.call_member(ty, "iterator", from)?;
        self.call_member(&iterator, "next", from)
    }

    fn call_member(&self, ty: &Ty, name: &str, from: UnitId) -> Option<Ty> {
        let m = self.members_named(ty, name, from).into_iter().find(|m| self.decl(m.sym).kind == Kind::Function)?;
        self.value_ty(m.sym, &m.subst)
    }

    /// The type of component `n` of `ty`: its `componentN()`, or a data class's `n`th property.
    pub fn component_ty(&self, ty: &Ty, n: usize, from: UnitId) -> Option<Ty> {
        if let Some(t) = self.call_member(ty, &format!("component{n}"), from) {
            return Some(t);
        }
        let Ty::Class { sym, args, .. } = ty else { return None };
        let d = self.decl(*sym);
        let file = self.file(sym.unit_id());
        let prop = d.params.iter().filter(|&&p| file.decls[p].kind == Kind::Property).nth(n.checked_sub(1)?)?;
        self.value_ty(sym.unit_id().sym(*prop), &self.class_subst(*sym, args))
    }

    // ----- names -----

    /// What `name` refers to at `offset` in `u`, without a receiver: the declarations of the
    /// innermost scope that has any.
    pub fn resolve_name(&self, u: UnitId, offset: usize, name: &str) -> Vec<Found> {
        let file = self.file(u);
        let chain = file.scope_chain(offset);
        for (ci, &sid) in chain.iter().enumerate() {
            let scope = &file.scopes[sid];
            if let ScopeKind::Lambda { it: true, brace } = scope.kind
                && name == "it"
            {
                return vec![Found::It { unit: u, brace }];
            }
            let mut found: Vec<Found> = scope
                .decls
                .iter()
                .copied()
                .filter(|&d| {
                    let decl = &file.decls[d];
                    decl.name == name && (decl.kind == Kind::Parameter || decl.name_span.start < offset || decl.kind.is_class())
                })
                .map(|d| Found::Sym(Member::plain(u.sym(d))))
                .collect();
            if !found.is_empty() {
                // The innermost declaration shadows the others.
                if found.iter().all(|f| matches!(f, Found::Sym(m) if self.decl(m.sym).kind == Kind::Function)) {
                    return found;
                }
                found.sort_by_key(|f| match f {
                    Found::Sym(m) => std::cmp::Reverse(self.decl(m.sym).name_span.start),
                    Found::It { .. } => std::cmp::Reverse(0),
                });
                found.truncate(1);
                return found;
            }
            let receiver = match scope.kind {
                ScopeKind::Function(fd) => {
                    file.decls[fd].receiver.as_ref().and_then(|r| self.ty_of_ref(r, Ctx { unit: u, decl: Some(fd) }, &Subst::new()))
                }
                ScopeKind::Lambda { brace, .. } => self.lambda_ctx(u, brace).and_then(|c| c.receiver),
                ScopeKind::Class(cd) => {
                    let decl = &file.decls[cd];
                    let in_function = chain[..ci].iter().any(|&s| matches!(file.scopes[s].kind, ScopeKind::Function(_)));
                    if !in_function {
                        let params: Vec<Found> = decl
                            .params
                            .iter()
                            .copied()
                            .filter(|&p| file.decls[p].kind == Kind::Parameter && file.decls[p].name == name)
                            .map(|p| Found::Sym(Member::plain(u.sym(p))))
                            .collect();
                        if !params.is_empty() {
                            return params;
                        }
                    }
                    if decl.kind == Kind::EnumEntry {
                        // An enum entry's body sees the enum's members.
                        file.owner_class(cd).map(|c| self.instance(u.sym(c)))
                    } else {
                        Some(self.instance(u.sym(cd)))
                    }
                }
                ScopeKind::File => {
                    return self.global_names(u, name);
                }
                ScopeKind::Block => None,
            };
            if let Some(receiver) = receiver {
                let mut found: Vec<Found> = self.members_named(&receiver, name, u).into_iter().map(Found::Sym).collect();
                if let (ScopeKind::Class(cd), Ty::Class { sym, .. }) = (scope.kind, &receiver) {
                    let class = if file.decls[cd].kind == Kind::EnumEntry { *sym } else { u.sym(cd) };
                    found.extend(
                        self.members(&Ty::Static(class), u, false).into_iter().filter(|m| self.decl(m.sym).name == name).map(Found::Sym),
                    );
                }
                if !found.is_empty() {
                    return found;
                }
            }
        }
        Vec::new()
    }

    /// Top-level declarations named `name` visible in `u`: in the file, imported, in the
    /// package, star-imported, default-imported.
    pub fn global_names(&self, u: UnitId, name: &str) -> Vec<Found> {
        let file = self.file(u);
        let wrap = |v: Vec<SymId>| -> Vec<Found> { v.into_iter().map(|s| Found::Sym(Member::plain(s))).collect() };
        let local: Vec<SymId> = file
            .top
            .iter()
            .copied()
            .filter(|&t| file.decls[t].name == name && file.decls[t].receiver.is_none())
            .map(|t| u.sym(t))
            .collect();
        if !local.is_empty() {
            return wrap(local);
        }
        let mut imported = Vec::new();
        for import in file.imports.iter().filter(|i| i.name() == Some(name)) {
            imported.extend(self.import_targets(import.fq()).into_iter().filter(|&s| self.decl(s).receiver.is_none()));
        }
        if !imported.is_empty() {
            return wrap(imported);
        }
        let candidates: Vec<SymId> = self.by_name(name).into_iter().filter(|&s| self.visible(u, s)).collect();
        let in_package = |p: &str| -> Vec<SymId> { candidates.iter().copied().filter(|&s| self.package_of(s) == p).collect() };
        // The declarations named `p.name`, which may be a Java platform class that Kotlin names elsewhere.
        let named_in = |p: &str| -> Vec<SymId> { self.by_fq(&format!("{p}.{name}")).into_iter().filter(|&s| self.visible(u, s)).collect() };
        let same = in_package(&file.package);
        if !same.is_empty() {
            return wrap(same);
        }
        let mut star = Vec::new();
        for import in file.imports.iter().filter(|i| i.star) {
            star.extend(in_package(&import.fq()));
            for s in named_in(&import.fq()) {
                if !star.contains(&s) {
                    star.push(s);
                }
            }
            for class in self.by_fq(&import.fq()).into_iter().filter(|&s| self.decl(s).kind.is_class()) {
                star.extend(
                    self.members(&Ty::Static(class), u, false).into_iter().filter(|m| self.decl(m.sym).name == name).map(|m| m.sym),
                );
            }
        }
        if !star.is_empty() {
            return wrap(star);
        }
        let mut defaults: Vec<SymId> =
            default_imports(self.unit(u)).iter().flat_map(|p| in_package(p).into_iter().chain(named_in(p))).collect();
        defaults.dedup();
        // Prefer the documented `expect` declaration of the standard library.
        defaults.sort_by_key(|&s| self.decl(s).doc.is_none());
        wrap(defaults)
    }

    /// The type of `this` at `offset`, optionally `this@label`.
    pub fn this_ty(&self, u: UnitId, offset: usize, label: Option<&str>) -> Option<Ty> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        for sid in file.scope_chain(offset) {
            match file.scopes[sid].kind {
                ScopeKind::Lambda { brace, .. } => {
                    let Some(ctx) = self.lambda_ctx(u, brace) else { continue };
                    let Some(receiver) = ctx.receiver else { continue };
                    let callee = ctx.callee.map(|c| self.decl(c).name.as_str());
                    // `this@name`: the callee's name, or the label written before the lambda.
                    let own = brace.checked_sub(1).filter(|&p| lx.kind(p) == Some(Tok::Label)).map(|p| lx.text(src, p));
                    if label.is_none() || label == callee || label == own {
                        return Some(receiver);
                    }
                }
                ScopeKind::Function(fd) => {
                    let d = &file.decls[fd];
                    if let Some(r) = &d.receiver
                        && (label.is_none() || label == Some(d.name.as_str()))
                    {
                        return self.ty_of_ref(r, Ctx { unit: u, decl: Some(fd) }, &Subst::new());
                    }
                }
                ScopeKind::Class(cd) => {
                    let d = &file.decls[cd];
                    if d.name.is_empty() {
                        // An anonymous object: its supertype's members and its own.
                        if label.is_none() && !d.supertypes.is_empty() {
                            return Some(self.instance(u.sym(cd)));
                        }
                        continue;
                    }
                    if label.is_none() || label == Some(d.name.as_str()) {
                        return Some(self.instance(u.sym(cd)));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The types a lambda gets from the call it is an argument of.
    pub fn lambda_ctx(&self, u: UnitId, brace: usize) -> Option<LambdaCtx> {
        if let Some(c) = self.lambdas.borrow().get(&(u, brace)) {
            return c.clone();
        }
        if !self.enter() {
            return None;
        }
        self.lambdas.borrow_mut().insert((u, brace), None);
        let out = self.lambda_ctx_inner(u, brace);
        self.leave();
        self.lambdas.borrow_mut().insert((u, brace), out.clone());
        out
    }

    fn lambda_ctx_inner(&self, u: UnitId, brace: usize) -> Option<LambdaCtx> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let mut p = brace;
        if p > 0 && lx.kind(p - 1) == Some(Tok::Label) {
            p -= 1;
        }
        // Where the call's callee ends, and which argument the lambda is.
        enum Position {
            Trailing,
            Index(usize),
            Named(String),
        }
        let (callee_end, position) = match p.checked_sub(1).and_then(|i| lx.kind(i)) {
            Some(Tok::RParen) => (lx.matching[p - 1]?, Position::Trailing),
            Some(Tok::Ident | Tok::Gt) if !HARD_KEYWORDS.contains(&lx.text(src, p - 1)) || lx.kind(p - 1) == Some(Tok::Gt) => {
                (p, Position::Trailing)
            }
            _ => {
                let open = enclosing_paren(lx, p)?;
                if p >= 2 && lx.kind(p - 1) == Some(Tok::Eq) && lx.kind(p - 2) == Some(Tok::Ident) {
                    (open, Position::Named(lx.text(src, p - 2).to_owned()))
                } else {
                    let commas = (open + 1..p).filter(|&i| lx.kind(i) == Some(Tok::Comma) && depth_between(lx, open + 1, i) == 0).count();
                    (open, Position::Index(commas))
                }
            }
        };
        let (callee, members) = self.callee(u, callee_end)?;
        let call_args = self.call_args(u, callee_end, lx.tokens.len()).0;
        let lambda_params = file.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Lambda { brace: b, .. } if b == brace));
        let explicit =
            lambda_params.map_or(0, |s| s.decls.iter().filter(|&&d| matches!(file.decls[d].init_kind, InitKind::LambdaParam(_))).count());
        let has_it = lambda_params.is_some_and(|s| matches!(s.kind, ScopeKind::Lambda { it: true, .. }));
        let mut best: Option<(Member, &TypeRef, i32)> = None;
        let no_subst = Subst::new();
        for m in members.iter().cloned() {
            let d = self.decl(m.sym);
            if d.kind != Kind::Function {
                continue;
            }
            let fparams = &self.file(m.sym.unit_id()).decls;
            let param = match &position {
                Position::Trailing => d.params.last(),
                Position::Index(i) => d.params.get(*i).or_else(|| d.params.last().filter(|&&l| fparams[l].vararg)),
                Position::Named(n) => d.params.iter().find(|&&q| fparams[q].name == *n),
            };
            let Some(&param) = param else { continue };
            let Some(pty) = fparams[param].ty.as_ref() else { continue };
            let mctx = Ctx { unit: m.sym.unit_id(), decl: Some(m.sym.decl_id()) };
            let Some((func, _, _)) = self.func_of(pty, mctx, &no_subst) else { continue };
            let arity = func.params.len();
            let mut score = 0;
            if has_it && (arity == 1 || (arity == 0 && func.receiver.is_some())) || !has_it && arity == explicit {
                score += 2;
            }
            if callee.is_some_and(|c| c == m.sym) {
                score += 1;
            }
            if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
                best = Some((m, pty, score));
            }
        }
        let Some((m, pty, _)) = best else {
            // `Runnable { ... }`: the lambda is the single method of a functional interface.
            let (class, func) = members.iter().find_map(|m| Some((m.sym, self.sam_of(m.sym)?)))?;
            let ctx = Ctx { unit: class.unit_id(), decl: Some(class.decl_id()) };
            let subst = Subst::new();
            return Some(LambdaCtx {
                receiver: None,
                params: func.params.iter().map(|t| self.ty_of_ref(t, ctx, &subst)).collect(),
                callee: Some(class),
            });
        };
        let ctx = Ctx { unit: m.sym.unit_id(), decl: Some(m.sym.decl_id()) };
        let mut subst = m.subst.clone();
        self.bind_args(&m, &mut subst, u, &call_args, true);
        let (func, ctx, subst) = self.func_of(pty, ctx, &subst)?;
        Some(LambdaCtx {
            receiver: func.receiver.as_ref().and_then(|r| self.ty_of_ref(r, ctx, &subst)),
            params: func.params.iter().map(|t| self.ty_of_ref(t, ctx, &subst)).collect(),
            callee: Some(m.sym),
        })
    }

    /// The function type that `tr` denotes in `ctx`: written out, or behind a type alias
    /// (`typealias Handler = (Event) -> Unit`), with the context and type arguments to read it in.
    fn func_of(&self, tr: &TypeRef, ctx: Ctx, subst: &Subst) -> Option<(syntax::FuncType, Ctx, Subst)> {
        if let Some(f) = &tr.func {
            return Some(((**f).clone(), ctx, subst.clone()));
        }
        if tr.name.is_empty() || !self.enter() {
            return None;
        }
        let out = (|| {
            let sym = self.resolve_type(ctx, &tr.name)?;
            let d = self.decl(sym);
            if d.kind != Kind::TypeAlias {
                // A functional interface takes a lambda for its one method (SAM conversion).
                let func = self.sam_of(sym)?;
                let sam_subst: Subst = d
                    .type_params
                    .iter()
                    .zip(&tr.args)
                    .filter_map(|(p, a)| Some((p.name.clone(), self.ty_of_ref(a, ctx, subst)?)))
                    .collect();
                return Some((func, Ctx { unit: sym.unit_id(), decl: Some(sym.decl_id()) }, sam_subst));
            }
            let alias_subst: Subst =
                d.type_params.iter().zip(&tr.args).filter_map(|(p, a)| Some((p.name.clone(), self.ty_of_ref(a, ctx, subst)?))).collect();
            let alias_ctx = Ctx { unit: sym.unit_id(), decl: Some(sym.decl_id()) };
            let target = d.ty.as_ref()?;
            self.func_of(target, alias_ctx, &alias_subst)
        })();
        self.leave();
        out
    }

    /// The function type of a functional interface (`fun interface`, or a Java interface with one
    /// abstract method): the parameters and result of that method.
    fn sam_of(&self, class: SymId) -> Option<syntax::FuncType> {
        let d = self.decl(class);
        if d.kind != Kind::Interface || !(d.has("fun") || self.unit(class.unit_id()).is_java()) {
            return None;
        }
        let file = self.file(class.unit_id());
        let mut abstracts = d.members.iter().copied().filter(|&m| {
            let md = &file.decls[m];
            md.kind == Kind::Function
                && !matches!(md.name.as_str(), "equals" | "hashCode" | "toString")
                && super::actions::is_abstract(self, class, class.unit_id().sym(m))
        });
        let method = &file.decls[abstracts.next()?];
        if abstracts.next().is_some() {
            return None;
        }
        Some(syntax::FuncType {
            receiver: None,
            params: method.params.iter().map(|&p| file.decls[p].ty.clone().unwrap_or_default()).collect(),
            names: method.params.iter().map(|&p| file.decls[p].name.clone()).collect(),
            ret: method.ty.clone().unwrap_or_else(|| TypeRef::named("Unit", Vec::new())),
        })
    }

    /// The function candidates of the callee whose expression ends before token `end`.
    fn callee(&self, u: UnitId, end: usize) -> Option<(Option<SymId>, Vec<Member>)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let mut name_tok = end.checked_sub(1)?;
        if lx.kind(name_tok) == Some(Tok::Gt) {
            name_tok = matching_lt(lx, name_tok)?.checked_sub(1)?;
        }
        if lx.kind(name_tok) != Some(Tok::Ident) {
            return None;
        }
        let name = lx.text(src, name_tok);
        if name_tok > 0 && matches!(lx.kind(name_tok - 1), Some(Tok::Dot | Tok::SafeDot)) {
            let start = chain_start(src, lx, name_tok - 1);
            let receiver = self.infer(u, start, name_tok - 1)?;
            let members = self.members_named(&receiver, name, u);
            return Some((None, members));
        }
        let found = self.resolve_name(u, lx.tokens[name_tok].start, name);
        let members: Vec<Member> = found
            .into_iter()
            .filter_map(|f| match f {
                Found::Sym(m) => Some(m),
                Found::It { .. } => None,
            })
            .collect();
        Some((None, members))
    }

    /// The arguments of the call whose argument list or trailing lambda starts at token `at`
    /// (just after the callee), and the token after them.
    pub fn call_args(&self, u: UnitId, at: usize, limit: usize) -> (Vec<Arg>, usize) {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let mut args = Vec::new();
        let mut i = at;
        if lx.kind(i) == Some(Tok::Lt)
            && let Some(gt) = matching_gt(lx, i)
        {
            i = gt + 1;
        }
        if lx.kind(i) == Some(Tok::LParen) {
            let close = lx.matching[i].filter(|&c| c > i).unwrap_or(limit.min(lx.tokens.len()));
            let mut start = i + 1;
            let mut k = i + 1;
            while k <= close && k < lx.tokens.len().max(close + 1) {
                if k == close || lx.kind(k) == Some(Tok::Comma) {
                    if k > start {
                        let (name, s) = if lx.kind(start) == Some(Tok::Ident) && lx.kind(start + 1) == Some(Tok::Eq) {
                            (Some(lx.text(src, start).to_owned()), start + 2)
                        } else {
                            (None, start)
                        };
                        let lambda = lx.kind(s) == Some(Tok::LBrace) && lx.matching[s].is_some_and(|c| c + 1 >= k);
                        args.push(Arg { start: s, end: k, name, lambda, trailing: false });
                    }
                    start = k + 1;
                    k += 1;
                    continue;
                }
                k = match lx.kind(k) {
                    Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen) => {
                        lx.matching[k].filter(|&c| c > k).map_or(close, |c| c + 1)
                    }
                    _ => k + 1,
                };
            }
            i = close + 1;
        }
        loop {
            let brace = if lx.kind(i) == Some(Tok::Label) { i + 1 } else { i };
            if lx.kind(brace) == Some(Tok::LBrace) && !lx.tokens[brace].nl {
                let close = lx.matching[brace].filter(|&c| c > brace).unwrap_or(limit.min(lx.tokens.len()).saturating_sub(1));
                args.push(Arg { start: brace, end: close + 1, name: None, lambda: true, trailing: true });
                i = close + 1;
            } else {
                break;
            }
        }
        (args, i)
    }

    /// Binds the type parameters of the function `m` from the call's arguments.
    pub fn bind_args(&self, m: &Member, subst: &mut Subst, u: UnitId, args: &[Arg], skip_lambdas: bool) {
        let d = self.decl(m.sym);
        let file = self.file(m.sym.unit_id());
        let all: Vec<String> = match d.kind {
            Kind::Constructor => {
                file.owner_class(m.sym.decl_id()).map(|c| &file.decls[c]).unwrap_or(d).type_params.iter().map(|t| t.name.clone()).collect()
            }
            _ => d.type_params.iter().map(|t| t.name.clone()).collect(),
        };
        if all.is_empty() {
            return;
        }
        let decls = &self.file(m.sym.unit_id()).decls;
        let mut index = 0;
        for arg in args {
            let param = if let Some(n) = &arg.name {
                d.params.iter().find(|&&p| decls[p].name == *n)
            } else if arg.trailing {
                d.params.last()
            } else {
                let p = d.params.get(index).or_else(|| d.params.last().filter(|&&l| decls[l].vararg));
                index += 1;
                p
            };
            let Some(&param) = param else { continue };
            let Some(pty) = decls[param].ty.as_ref() else { continue };
            if arg.lambda {
                if skip_lambdas {
                    continue;
                }
                // `map { ... }`: the lambda's result binds the function type's return type.
                if let Some(func) = &pty.func
                    && all.contains(&func.ret.name)
                    && !subst.contains_key(&func.ret.name)
                    && let Some(r) = self.lambda_result(u, arg.start)
                {
                    subst.insert(func.ret.name.clone(), r);
                }
                continue;
            }
            if let Some(actual) = self.infer(u, arg.start, arg.end) {
                self.unify(pty, &actual, &all, subst);
            }
        }
    }

    /// The type of the last expression of the lambda whose `{` is `brace`.
    pub fn lambda_result(&self, u: UnitId, brace: usize) -> Option<Ty> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let close = lx.matching[brace].filter(|&c| c > brace)?;
        let scope = file.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Lambda { brace: b, .. } if b == brace));
        let mut start = brace + 1;
        if let Some(s) = scope
            && let Some(arrow) = (brace + 1..close).find(|&i| lx.kind(i) == Some(Tok::Arrow) && depth_between(lx, brace + 1, i) == 0)
            && !matches!(s.kind, ScopeKind::Lambda { it: true, .. })
        {
            start = arrow + 1;
        }
        // The start of the last statement.
        let mut last = start;
        let mut i = start;
        while i < close {
            let e = syntax::expr_end(src, lx, i, close);
            if e >= close {
                break;
            }
            i = if lx.kind(e) == Some(Tok::Semi) { e + 1 } else { e.max(i + 1) };
            if i < close {
                last = i;
            }
        }
        if last >= close || lx.is(src, last, "return") || lx.is(src, last, "val") || lx.is(src, last, "var") {
            return None;
        }
        self.infer(u, last, close)
    }

    // ----- expressions -----

    /// The type of the expression in tokens `a..b` of `u`.
    pub fn infer(&self, u: UnitId, a: usize, b: usize) -> Option<Ty> {
        if a >= b || !self.enter() {
            return None;
        }
        let out = self.infer_inner(u, a, b);
        self.leave();
        out
    }

    fn infer_inner(&self, u: UnitId, a: usize, b: usize) -> Option<Ty> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let b = b.min(lx.tokens.len());
        // An `if` runs to the end of the expression, and `when` and `try` hold their branches in
        // blocks: what follows their conditions is no operand.
        if ["if", "when", "try"].iter().any(|w| lx.is(src, a, w)) {
            return self.chain(u, a, b);
        }
        // Binary operators outside brackets, by precedence.
        let mut ops: Vec<(usize, &str)> = Vec::new();
        let mut k = a;
        while k < b {
            let t = lx.tokens[k];
            match t.kind {
                Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen => {
                    k = lx.matching[k].filter(|&c| c > k).map_or(b, |c| c + 1);
                    continue;
                }
                Tok::Lt if k > a => {
                    if let Some(gt) = matching_gt(lx, k) {
                        k = gt + 1;
                        continue;
                    }
                    ops.push((k, "<"));
                }
                Tok::Gt if k > a => ops.push((k, ">")),
                Tok::Op if k > a => ops.push((k, &src[t.span()])),
                Tok::Ident if k > a && is_operand_end(src, lx, k - 1) && k + 1 < b => {
                    let word = &src[t.span()];
                    if matches!(word, "is" | "in" | "as") {
                        ops.push((k, word));
                    } else if !HARD_KEYWORDS.contains(&word) && lx.kind(k - 1) != Some(Tok::Dot) {
                        ops.push((k, "infix"));
                    }
                }
                _ => {}
            }
            k += 1;
        }
        let find = |set: &[&str]| ops.iter().find(|(_, o)| set.contains(o)).map(|(i, o)| (*i, *o));
        let boolean = || self.std_ty("kotlin.Boolean");
        if find(&["||", "&&", "==", "!=", "===", "!==", "<", ">", "<=", ">=", "is", "in"]).is_some() {
            return boolean();
        }
        if let Some((i, _)) = find(&["?:"]) {
            return self.infer(u, a, i).map(strip_nullable).or_else(|| self.infer(u, i + 1, b));
        }
        if let Some(&(i, _)) = ops.iter().rev().find(|(_, o)| *o == "infix") {
            let left = self.infer(u, a, i)?;
            let name = lx.text(src, i);
            let m = self.members_named(&left, name, u).into_iter().find(|m| self.decl(m.sym).kind == Kind::Function)?;
            let mut subst = m.subst.clone();
            self.bind_args(&m, &mut subst, u, &[Arg { start: i + 1, end: b, name: None, lambda: false, trailing: false }], false);
            return self.value_ty(m.sym, &subst);
        }
        if let Some((i, op)) = find(&["..", "..<"]) {
            let left = self.infer(u, a, i)?;
            let fq = self.class_fq(&left);
            if fq.as_deref().is_some_and(|fq| !fq.starts_with("kotlin.")) {
                // `operator fun rangeTo` of a custom type.
                return self.operator_ty(u, &left, op, i + 1, b);
            }
            let range = match fq.as_deref() {
                Some("kotlin.Long") => "kotlin.ranges.LongRange",
                Some("kotlin.Char") => "kotlin.ranges.CharRange",
                Some("kotlin.UInt") => "kotlin.ranges.UIntRange",
                Some("kotlin.ULong") => "kotlin.ranges.ULongRange",
                _ => "kotlin.ranges.IntRange",
            };
            return self.std_ty(range);
        }
        // Left to right: the last operator of the lowest precedence ends the left operand.
        let last = |set: &[&str]| ops.iter().rev().find(|(_, o)| set.contains(o)).map(|(i, o)| (*i, *o));
        if let Some((i, op)) = last(&["+", "-"]).or_else(|| last(&["*", "/", "%"])) {
            let Some(left) = self.infer(u, a, i) else { return self.infer(u, i + 1, b) };
            // The operator function of the left operand; its type when there is none.
            return Some(self.operator_ty(u, &left, op, i + 1, b).unwrap_or(left));
        }
        if let Some((i, _)) = find(&["as"]) {
            let mut t = i + 1;
            let safe = lx.kind(t) == Some(Tok::Question);
            if safe {
                t += 1;
            }
            let name: Vec<&str> =
                (t..b).take_while(|&k| matches!(lx.kind(k), Some(Tok::Ident | Tok::Dot))).map(|k| lx.text(src, k)).collect();
            let decl = self.decl_containing(u, lx.tokens[i].start);
            let sym = self.resolve_type(Ctx { unit: u, decl }, &name.concat())?;
            return Some(Ty::Class { sym, args: Vec::new(), nullable: safe });
        }
        // Prefix operators.
        if lx.kind(a) == Some(Tok::Op) {
            return match &src[lx.tokens[a].span()] {
                "!" => boolean(),
                op @ ("-" | "+") => {
                    let operand = self.infer(u, a + 1, b)?;
                    let name = if op == "-" { "unaryMinus" } else { "unaryPlus" };
                    let m = self.members_named(&operand, name, u).into_iter().find(|m| self.decl(m.sym).kind == Kind::Function);
                    Some(m.and_then(|m| self.value_ty(m.sym, &m.subst)).unwrap_or(operand))
                }
                _ => self.infer(u, a + 1, b),
            };
        }
        self.chain(u, a, b)
    }

    /// What `left op right` is through the operator function `op` stands for (`plus`, `rangeTo`,
    /// ...) of `left`, choosing the overload that takes the right operand's type. `right` is the
    /// expression in tokens `from..to`.
    fn operator_ty(&self, u: UnitId, left: &Ty, op: &str, from: usize, to: usize) -> Option<Ty> {
        let name = match op {
            "+" => "plus",
            "-" => "minus",
            "*" => "times",
            "/" => "div",
            "%" => "rem",
            ".." => "rangeTo",
            "..<" => "rangeUntil",
            _ => return None,
        };
        let candidates: Vec<Member> =
            self.members_named(left, name, u).into_iter().filter(|m| self.decl(m.sym).kind == Kind::Function).collect();
        let right = self.infer(u, from, to);
        let takes = |m: &Member, right: &Ty| {
            let d = self.decl(m.sym);
            let ctx = Ctx { unit: m.sym.unit_id(), decl: Some(m.sym.decl_id()) };
            let param = d.params.first().and_then(|&p| self.file(m.sym.unit_id()).decls[p].ty.as_ref());
            let ty = param.and_then(|t| self.ty_of_ref(t, ctx, &m.subst));
            matches!((ty.as_ref().and_then(|t| self.class_fq(t)), self.class_fq(right)), (Some(p), Some(r)) if p == r)
        };
        let chosen = right.as_ref().and_then(|r| candidates.iter().find(|m| takes(m, r))).or(candidates.first())?;
        let mut subst = chosen.subst.clone();
        self.bind_args(chosen, &mut subst, u, &[Arg { start: from, end: to, name: None, lambda: false, trailing: false }], false);
        self.value_ty(chosen.sym, &subst)
    }

    /// The type that values of all of `tys` have in common: the class itself when they agree,
    /// else the first supertype they share.
    fn common_supertype(&self, tys: Vec<Ty>) -> Option<Ty> {
        let mut it = tys.into_iter();
        let mut acc = it.next()?;
        for t in it {
            acc = self.join(acc, t);
        }
        Some(acc)
    }

    fn join(&self, a: Ty, b: Ty) -> Ty {
        let (Ty::Class { sym: sa, args: aa, nullable: na }, Ty::Class { sym: sb, args: ab, nullable: nb }) = (&a, &b) else {
            return a;
        };
        let nullable = *na || *nb;
        if sa == sb {
            let args = aa.iter().zip(ab).map(|(x, y)| if x == y { x.clone() } else { None }).collect();
            return Ty::Class { sym: *sa, args, nullable };
        }
        let other: HashSet<String> = self.closure(&b).into_iter().map(|(c, _)| self.fq(c)).collect();
        for (c, subst) in self.closure(&a) {
            if other.contains(&self.fq(c)) {
                let args = self.decl(c).type_params.iter().map(|p| subst.get(&p.name).cloned()).collect();
                return Ty::Class { sym: c, args, nullable };
            }
        }
        a
    }

    /// The type of a branch of `if` or `when`: a block's last expression, or an expression in
    /// tokens `start..end`.
    fn branch_ty(&self, u: UnitId, start: usize, end: usize) -> Option<Ty> {
        if self.file(u).lexed.kind(start) == Some(Tok::LBrace) { self.block_result(u, start) } else { self.infer(u, start, end) }
    }

    /// The type of a callable reference to `m` (`::foo`, `String::length`, `obj::method`): a
    /// function type, taking `unbound` first when it is the receiver the reference leaves open.
    fn callable_ref(&self, m: &Member, unbound: Option<Ty>) -> Option<Ty> {
        let d = self.decl(m.sym);
        let u = m.sym.unit_id();
        let ctx = Ctx { unit: u, decl: Some(m.sym.decl_id()) };
        let decls = &self.file(u).decls;
        let mut params: Vec<Option<Ty>> = unbound.into_iter().map(Some).collect();
        let ret = match d.kind {
            Kind::Function | Kind::Constructor => {
                params.extend(d.params.iter().map(|&p| decls[p].ty.as_ref().and_then(|t| self.ty_of_ref(t, ctx, &m.subst))));
                self.value_ty(m.sym, &m.subst)
            }
            k if k.is_class() && k != Kind::Object && k != Kind::Companion => {
                params.extend(d.params.iter().map(|&p| decls[p].ty.as_ref().and_then(|t| self.ty_of_ref(t, ctx, &m.subst))));
                Some(self.instance(m.sym))
            }
            k if k.is_value() => self.value_ty(m.sym, &m.subst),
            _ => return None,
        };
        Some(Ty::Func { receiver: None, params, ret: ret.map(Box::new) })
    }

    /// The type of the lambda literal whose `{` is token `brace`, as a value.
    fn lambda_value(&self, u: UnitId, brace: usize) -> Option<Ty> {
        let file = self.file(u);
        let scope = file.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Lambda { brace: b, .. } if b == brace))?;
        let ctx = Ctx { unit: u, decl: self.decl_containing(u, file.lexed.tokens[brace].start) };
        let mut written: Vec<(usize, Option<Ty>)> = scope
            .decls
            .iter()
            .filter_map(|&d| match file.decls[d].init_kind {
                InitKind::LambdaParam(n) | InitKind::LambdaComponent(n, _) => {
                    Some((n, file.decls[d].ty.as_ref().and_then(|t| self.ty_of_ref(t, ctx, &Subst::new()))))
                }
                _ => None,
            })
            .collect();
        written.sort_by_key(|(n, _)| *n);
        written.dedup_by_key(|(n, _)| *n);
        let mut params: Vec<Option<Ty>> = written.into_iter().map(|(_, t)| t).collect();
        if matches!(scope.kind, ScopeKind::Lambda { it: true, .. }) {
            params = vec![None];
        }
        Some(Ty::Func { receiver: None, params, ret: self.lambda_result(u, brace).map(Box::new) })
    }

    /// The innermost non-local declaration around `offset`, for resolving type names.
    pub fn decl_containing(&self, u: UnitId, offset: usize) -> Option<DeclId> {
        let file = self.file(u);
        for sid in file.scope_chain(offset) {
            if let ScopeKind::Class(d) | ScopeKind::Function(d) = file.scopes[sid].kind {
                return Some(d);
            }
        }
        None
    }

    pub fn class_fq(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::Class { sym, .. } => Some(self.fq(*sym)),
            _ => None,
        }
    }

    /// A postfix chain: a primary expression and `.member`, `?.member`, `!!`, calls and indexing.
    fn chain(&self, u: UnitId, a: usize, b: usize) -> Option<Ty> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let (mut cur, mut i) = self.primary(u, a, b)?;
        while i < b {
            match lx.kind(i)? {
                Tok::Dot | Tok::SafeDot => {
                    if lx.kind(i + 1) != Some(Tok::Ident) || i + 1 >= b {
                        return None;
                    }
                    let safe = lx.kind(i) == Some(Tok::SafeDot);
                    let (ty, next) = self.member_access(u, &if safe { strip_nullable(cur) } else { cur }, i + 1, b)?;
                    cur = if safe { nullable(ty, true) } else { ty };
                    i = next;
                }
                Tok::BangBang => {
                    cur = strip_nullable(cur);
                    i += 1;
                }
                Tok::ColonColon => {
                    // `String::length` leaves the receiver open; `value::method` binds it.
                    if lx.kind(i + 1) != Some(Tok::Ident) || i + 1 >= b || lx.is(src, i + 1, "class") {
                        return None;
                    }
                    let name = lx.text(src, i + 1);
                    let (owner, unbound) = match &cur {
                        Ty::Static(s) => (self.instance(*s), true),
                        other => (other.clone(), false),
                    };
                    let m = self.members_named(&owner, name, u).into_iter().next()?;
                    cur = self.callable_ref(&m, unbound.then_some(owner))?;
                    i += 2;
                }
                Tok::LParen => {
                    let next = lx.matching[i].filter(|&c| c > i).map_or(b, |c| c + 1);
                    cur = match cur {
                        Ty::Func { ret, .. } => *ret?,
                        other => self.call_member(&other, "invoke", u)?,
                    };
                    i = next;
                }
                Tok::LBracket => {
                    let next = lx.matching[i].filter(|&c| c > i).map_or(b, |c| c + 1);
                    let m = self.members_named(&cur, "get", u).into_iter().find(|m| self.decl(m.sym).kind == Kind::Function)?;
                    cur = self.value_ty(m.sym, &m.subst)?;
                    i = next;
                }
                _ => return None,
            }
        }
        Some(cur)
    }

    fn primary(&self, u: UnitId, i: usize, b: usize) -> Option<(Ty, usize)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let t = lx.tokens.get(i)?;
        let after = |k: usize| lx.matching[k].filter(|&c| c > k).map_or(b, |c| c + 1);
        match t.kind {
            Tok::StrOpen => Some((self.std_ty("kotlin.String")?, after(i))),
            Tok::Int => {
                let text = &src[t.span()];
                let fq = if text.ends_with("uL") || text.ends_with("UL") {
                    "kotlin.ULong"
                } else if text.ends_with(['u', 'U']) {
                    "kotlin.UInt"
                } else if text.ends_with('L') {
                    "kotlin.Long"
                } else {
                    "kotlin.Int"
                };
                Some((self.std_ty(fq)?, i + 1))
            }
            Tok::Float => {
                let fq = if src[t.span()].ends_with(['f', 'F']) { "kotlin.Float" } else { "kotlin.Double" };
                Some((self.std_ty(fq)?, i + 1))
            }
            Tok::Char => Some((self.std_ty("kotlin.Char")?, i + 1)),
            Tok::LBrace => Some((self.lambda_value(u, i)?, after(i))),
            Tok::ColonColon => {
                if lx.kind(i + 1) != Some(Tok::Ident) || i + 1 >= b {
                    return None;
                }
                let found = self.resolve_name(u, lx.tokens[i + 1].start, lx.text(src, i + 1));
                let m = found.iter().find_map(|f| if let Found::Sym(m) = f { Some(m) } else { None })?;
                Some((self.callable_ref(m, None)?, i + 2))
            }
            Tok::LParen => {
                let close = lx.matching[i].filter(|&c| c > i)?;
                Some((self.infer(u, i + 1, close)?, close + 1))
            }
            Tok::Ident => {
                let word = &src[t.span()];
                match word {
                    "true" | "false" => Some((self.std_ty("kotlin.Boolean")?, i + 1)),
                    "null" => None,
                    "this" => {
                        let label = (lx.kind(i + 1) == Some(Tok::LabelRef)).then(|| lx.text(src, i + 1));
                        let next = if label.is_some() { i + 2 } else { i + 1 };
                        Some((self.this_ty(u, t.start, label)?, next))
                    }
                    "super" => {
                        let decl = self.decl_containing(u, t.start)?;
                        let class = self.enclosing_class(u, decl)?;
                        let st = file.decls[class].supertypes.first()?;
                        let ty = self.ty_of_ref(st, Ctx { unit: u, decl: Some(class) }, &Subst::new())?;
                        Some((ty, if lx.kind(i + 1) == Some(Tok::LabelRef) { i + 2 } else { i + 1 }))
                    }
                    "if" => {
                        let lp = i + 1;
                        let close = lx.matching.get(lp).copied().flatten()?;
                        let branch = close + 1;
                        let (then_ty, after) = if lx.kind(branch) == Some(Tok::LBrace) {
                            (self.block_result(u, branch), lx.matching[branch].filter(|&c| c > branch).map_or(b, |c| c + 1))
                        } else {
                            // The branch ends at its `else`.
                            let end = syntax::expr_end(src, lx, branch, b);
                            let mut k = branch;
                            while k < end && !lx.is(src, k, "else") {
                                k = match lx.kind(k) {
                                    Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen) => {
                                        lx.matching[k].filter(|&c| c > k).map_or(end, |c| c + 1)
                                    }
                                    _ => k + 1,
                                };
                            }
                            (self.infer(u, branch, k.min(end)), k.min(end))
                        };
                        let mut tys = vec![then_ty];
                        if lx.is(src, after, "else") {
                            let e = after + 1;
                            tys.push(self.branch_ty(u, e, syntax::expr_end(src, lx, e, b)));
                        }
                        Some((self.common_supertype(tys.into_iter().flatten().collect())?, b))
                    }
                    "when" => {
                        let mut brace = i + 1;
                        if lx.kind(brace) == Some(Tok::LParen) {
                            brace = lx.matching[brace].filter(|&c| c > brace)? + 1;
                        }
                        if lx.kind(brace) != Some(Tok::LBrace) {
                            return None;
                        }
                        let end = lx.matching[brace].filter(|&c| c > brace)?;
                        let mut tys = Vec::new();
                        let mut j = brace + 1;
                        while j < end {
                            // A branch: conditions, `->`, then a block or an expression.
                            let mut a = j;
                            while a < end && lx.kind(a) != Some(Tok::Arrow) {
                                a = match lx.kind(a) {
                                    Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen) => {
                                        lx.matching[a].filter(|&c| c > a).map_or(end, |c| c + 1)
                                    }
                                    _ => a + 1,
                                };
                            }
                            if a >= end {
                                break;
                            }
                            let body = a + 1;
                            let body_end = if lx.kind(body) == Some(Tok::LBrace) {
                                lx.matching[body].filter(|&c| c > body).map_or(end, |c| c + 1)
                            } else {
                                // The expression stops where the next branch, `else ->`, starts.
                                let stop = syntax::expr_end(src, lx, body, end);
                                let mut k = body;
                                while k < stop && !(lx.is(src, k, "else") && lx.kind(k + 1) == Some(Tok::Arrow)) {
                                    k = match lx.kind(k) {
                                        Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen) => {
                                            lx.matching[k].filter(|&c| c > k).map_or(stop, |c| c + 1)
                                        }
                                        _ => k + 1,
                                    };
                                }
                                k.min(stop)
                            };
                            tys.extend(self.branch_ty(u, body, body_end));
                            j = body_end.max(a + 1);
                            if lx.kind(j) == Some(Tok::Semi) {
                                j += 1;
                            }
                        }
                        Some((self.common_supertype(tys)?, end + 1))
                    }
                    "try" => {
                        let block = i + 1;
                        if lx.kind(block) != Some(Tok::LBrace) {
                            return None;
                        }
                        let mut k = lx.matching[block].filter(|&c| c > block)? + 1;
                        let mut tys = vec![self.block_result(u, block)];
                        loop {
                            if lx.is(src, k, "catch") && lx.kind(k + 1) == Some(Tok::LParen) {
                                let brace = lx.matching[k + 1].filter(|&c| c > k + 1).map_or(b, |c| c + 1);
                                if lx.kind(brace) != Some(Tok::LBrace) {
                                    break;
                                }
                                tys.push(self.block_result(u, brace));
                                k = lx.matching[brace].filter(|&c| c > brace).map_or(b, |c| c + 1);
                            } else if lx.is(src, k, "finally") && lx.kind(k + 1) == Some(Tok::LBrace) {
                                k = lx.matching[k + 1].filter(|&c| c > k + 1).map_or(b, |c| c + 1);
                                break;
                            } else {
                                break;
                            }
                        }
                        Some((self.common_supertype(tys.into_iter().flatten().collect())?, k))
                    }
                    "object" => {
                        let d = file.decls.iter().position(|d| d.kind == Kind::Object && d.name.is_empty() && d.name_span == t.span())?;
                        let next = file.decls[d].body.map(|s| file.scopes[s].range.end).map_or(b, |end| lx.at_or_after(end));
                        Some((self.instance(u.sym(d)), next))
                    }
                    _ if HARD_KEYWORDS.contains(&word) => None,
                    _ => self.name_ref(u, i, b),
                }
            }
            _ => None,
        }
    }

    fn enclosing_class(&self, u: UnitId, mut decl: DeclId) -> Option<DeclId> {
        let file = self.file(u);
        loop {
            if file.decls[decl].kind.is_class() {
                return Some(decl);
            }
            decl = file.decls[decl].parent?;
        }
    }

    /// The type of the last expression of the block whose `{` is `brace`.
    fn block_result(&self, u: UnitId, brace: usize) -> Option<Ty> {
        // Blocks and lambdas end alike.
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let close = lx.matching[brace].filter(|&c| c > brace)?;
        let mut last = brace + 1;
        let mut i = brace + 1;
        while i < close {
            let e = syntax::expr_end(src, lx, i, close);
            if e >= close {
                break;
            }
            i = if lx.kind(e) == Some(Tok::Semi) { e + 1 } else { e.max(i + 1) };
            if i < close {
                last = i;
            }
        }
        self.infer(u, last, close)
    }

    /// A name used as an expression at token `i`: a value, a call, a class or a package.
    fn name_ref(&self, u: UnitId, i: usize, b: usize) -> Option<(Ty, usize)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let name = lx.text(src, i);
        let found = self.resolve_name(u, lx.tokens[i].start, name);
        let mut j = i + 1;
        let targs = self.explicit_type_args(u, j);
        if let Some((_, next)) = &targs {
            j = *next;
        }
        let is_call = matches!(lx.kind(j), Some(Tok::LParen)) && !lx.tokens[j].nl
            || matches!(lx.kind(j), Some(Tok::LBrace)) && !lx.tokens[j].nl
            || lx.kind(j) == Some(Tok::Label) && lx.kind(j + 1) == Some(Tok::LBrace);
        if found.is_empty() {
            return self.has_package(name).then(|| (Ty::Package(name.to_owned()), j));
        }
        if is_call {
            let (args, next) = self.call_args(u, j, b);
            let ty = self.call_found(u, &found, &args, targs.map(|t| t.0).unwrap_or_default());
            return Some((ty?, next));
        }
        let ty = found.iter().find_map(|f| match f {
            Found::It { unit, brace } => self.lambda_ctx(*unit, *brace).and_then(|c| c.params.first().cloned().flatten()),
            Found::Sym(m) => {
                let d = self.decl(m.sym);
                (d.kind != Kind::Function).then(|| self.value_ty(m.sym, &m.subst).map(|t| self.smart_cast(u, i, m.sym, t))).flatten()
            }
        })?;
        Some((ty, j))
    }

    /// The type of the stable value `sym` (a parameter or a `val`) used at token `i`, narrowed by
    /// the checks that surround it: `if (x is T)` and its `&&` chains, `when (x) { is T -> }`,
    /// `when { x is T -> }`, and, for the code after them, `if (x !is T) return`, `x as T` and
    /// `require(x is T)`. `declared` is its type without the narrowing.
    fn smart_cast(&self, u: UnitId, i: usize, sym: SymId, declared: Ty) -> Ty {
        let d = self.decl(sym);
        if sym.unit_id() != u || !(d.kind == Kind::Parameter || d.kind == Kind::Variable && !d.mutable) {
            return declared;
        }
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let name = d.name.as_str();
        let same = |k: usize| {
            lx.kind(k) == Some(Tok::Ident)
                && lx.text(src, k) == name
                && !matches!(k.checked_sub(1).and_then(|p| lx.kind(p)), Some(Tok::Dot | Tok::SafeDot))
                && matches!(self.resolve_name(u, lx.tokens[k].start, name).first(), Some(Found::Sym(m)) if m.sym == sym)
        };
        let len = lx.tokens.len();
        // The end of a branch: its block, or its expression up to an `else`.
        let branch_end = |start: usize| -> usize {
            if lx.kind(start) == Some(Tok::LBrace) {
                return lx.matching[start].filter(|&c| c > start).map_or(len, |c| c + 1);
            }
            // A bare `return` ends at its line.
            if matches!(lx.text(src, start), "return" | "break" | "continue") && lx.tokens.get(start + 1).is_none_or(|t| t.nl) {
                return start + 1;
            }
            let end = syntax::expr_end(src, lx, start, len);
            let mut k = start;
            while k < end && !lx.is(src, k, "else") {
                k = match lx.kind(k) {
                    Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen) => {
                        lx.matching[k].filter(|&c| c > k).map_or(end, |c| c + 1)
                    }
                    _ => k + 1,
                };
            }
            k.min(end)
        };
        // The types the operands of a condition (split at `sep`) check `name` against.
        let facts = |a: usize, b: usize, sep: &str, negated: bool| -> Vec<Option<Ty>> {
            let mut out = Vec::new();
            let mut s = a;
            let mut k = a;
            while k <= b {
                let at_end = k >= b;
                if !at_end && matches!(lx.kind(k), Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen)) {
                    k = lx.matching[k].filter(|&c| c > k).map_or(b, |c| c + 1);
                    continue;
                }
                if at_end || lx.kind(k) == Some(Tok::Op) && lx.text(src, k) == sep {
                    if same(s) {
                        // `x != null` (`x == null` when negated): not null.
                        let op = (lx.kind(s + 1) == Some(Tok::Op)).then(|| lx.text(src, s + 1));
                        if op == Some(if negated { "==" } else { "!=" }) && lx.is(src, s + 2, "null") {
                            out.push(None);
                        }
                        let ty_at = if negated {
                            (lx.kind(s + 1) == Some(Tok::Op) && lx.text(src, s + 1) == "!" && lx.is(src, s + 2, "is")).then_some(s + 3)
                        } else {
                            lx.is(src, s + 1, "is").then_some(s + 2)
                        };
                        if let Some((ty, _)) = ty_at.and_then(|j| self.checked_type(u, j)) {
                            out.push(Some(ty));
                        }
                    }
                    s = k + 1;
                }
                k += 1;
            }
            out
        };
        let diverges = |a: usize, b: usize| {
            (a..b.min(len)).any(|k| lx.kind(k) == Some(Tok::Ident) && matches!(lx.text(src, k), "return" | "throw" | "break" | "continue"))
        };
        // The narrowings that reach `i`, each with the token it starts at.
        let mut found: Vec<(usize, Option<Ty>)> = Vec::new();
        // Narrowings for the rest of the block they are in: (start, block's `{`, type).
        let mut after: Vec<(usize, Option<usize>, Option<Ty>)> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        let mut k = lx.at_or_after(d.name_span.start);
        while k < i.min(len) {
            match lx.kind(k) {
                Some(Tok::LBrace | Tok::TemplateOpen) => open.push(k),
                Some(Tok::RBrace) => {
                    open.pop();
                }
                Some(Tok::Ident) => {
                    let word = lx.text(src, k);
                    let paren = lx.kind(k + 1) == Some(Tok::LParen);
                    let close = lx.matching.get(k + 1).copied().flatten().filter(|&c| c > k + 1);
                    match word {
                        "if" if paren => {
                            if let Some(close) = close {
                                let pos = facts(k + 2, close, "&&", false);
                                let neg = facts(k + 2, close, "||", true);
                                let then = close + 1;
                                let then_end = branch_end(then);
                                if (then..then_end).contains(&i) {
                                    found.extend(pos.iter().map(|t| (then, t.clone())));
                                }
                                if lx.is(src, then_end, "else") {
                                    let end = branch_end(then_end + 1);
                                    if (then_end + 1..end).contains(&i) {
                                        found.extend(neg.iter().map(|t| (then_end + 1, t.clone())));
                                    }
                                } else if diverges(then, then_end) {
                                    after.extend(neg.iter().map(|t| (then_end, open.last().copied(), t.clone())));
                                }
                            }
                        }
                        "when" => {
                            let (subject, brace) = match (paren, close) {
                                (true, Some(c)) => (c == k + 3 && same(k + 2), c + 1),
                                _ => (false, k + 1),
                            };
                            if lx.kind(brace) == Some(Tok::LBrace) && (subject || !paren) {
                                let end = lx.matching[brace].filter(|&c| c > brace).unwrap_or(len);
                                let mut j = brace + 1;
                                while j < end {
                                    let mut a = j;
                                    let mut single = true;
                                    while a < end && lx.kind(a) != Some(Tok::Arrow) {
                                        single &= lx.kind(a) != Some(Tok::Comma);
                                        a = match lx.kind(a) {
                                            Some(Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen) => {
                                                lx.matching[a].filter(|&c| c > a).map_or(end, |c| c + 1)
                                            }
                                            _ => a + 1,
                                        };
                                    }
                                    if a >= end {
                                        break;
                                    }
                                    let body = a + 1;
                                    let body_end = if lx.kind(body) == Some(Tok::LBrace) {
                                        lx.matching[body].filter(|&c| c > body).map_or(end, |c| c + 1)
                                    } else {
                                        syntax::expr_end(src, lx, body, end)
                                    };
                                    if (body..body_end).contains(&i) {
                                        let ty = if subject {
                                            let ty = (single && lx.is(src, j, "is")).then(|| self.checked_type(u, j + 1)).flatten();
                                            ty.map(|t| vec![Some(t.0)]).unwrap_or_default()
                                        } else {
                                            facts(j, a, "&&", false)
                                        };
                                        found.extend(ty.into_iter().map(|t| (body, t)));
                                    }
                                    j = body_end.max(a + 1);
                                    if lx.kind(j) == Some(Tok::Semi) {
                                        j += 1;
                                    }
                                }
                            }
                        }
                        "require" | "check" if paren => {
                            if let Some(close) = close {
                                after.extend(facts(k + 2, close, "&&", false).into_iter().map(|t| (close + 1, open.last().copied(), t)));
                            }
                        }
                        _ if same(k) => {
                            let statement = k == 0 || lx.tokens[k].nl || matches!(lx.kind(k - 1), Some(Tok::Semi | Tok::LBrace));
                            let negated = lx.kind(k + 1) == Some(Tok::Op) && lx.text(src, k + 1) == "!" && lx.is(src, k + 2, "is");
                            if lx.is(src, k + 1, "as") && lx.kind(k + 2) != Some(Tok::Question) && statement {
                                if let Some((ty, end)) = self.checked_type(u, k + 2) {
                                    after.push((end, open.last().copied(), Some(ty)));
                                }
                            } else if statement && lx.kind(k + 1) == Some(Tok::BangBang) {
                                // `x!!` and `x ?: return`: not null for the rest of the block.
                                after.push((k + 2, open.last().copied(), None));
                            } else if lx.kind(k + 1) == Some(Tok::Op)
                                && lx.text(src, k + 1) == "?:"
                                && matches!(lx.text(src, k + 2), "return" | "throw" | "break" | "continue")
                            {
                                after.push((k + 2, open.last().copied(), None));
                            } else if lx.kind(k + 1) == Some(Tok::Op)
                                && lx.is(src, k + 2, "null")
                                && lx.kind(k + 3) == Some(Tok::Op)
                                && matches!((lx.text(src, k + 1), lx.text(src, k + 3)), ("!=", "&&") | ("==", "||"))
                                && (k + 3..syntax::expr_end(src, lx, k + 3, len)).contains(&i)
                            {
                                // `x != null && ...` and `x == null || ...`.
                                found.push((k + 3, None));
                            } else if negated || lx.is(src, k + 1, "is") {
                                // `x is T && ...` and `x !is T || ...`: the operands that follow.
                                if let Some((ty, end)) = self.checked_type(u, k + if negated { 3 } else { 2 }) {
                                    let sep = if negated { "||" } else { "&&" };
                                    if lx.kind(end) == Some(Tok::Op)
                                        && lx.text(src, end) == sep
                                        && (end..syntax::expr_end(src, lx, end, len)).contains(&i)
                                    {
                                        found.push((end, Some(ty)));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            k += 1;
        }
        found.extend(
            after.into_iter().filter(|(from, block, _)| *from <= i && block.is_none_or(|b| open.contains(&b))).map(|(f, _, t)| (f, t)),
        );
        let narrowed = found.iter().filter_map(|(from, t)| t.as_ref().map(|t| (*from, t))).max_by_key(|(from, _)| *from);
        let ty = narrowed.map_or(declared, |(_, t)| t.clone());
        if found.iter().any(|(_, t)| t.is_none()) { strip_nullable(ty) } else { ty }
    }

    /// The class named by the type that starts at token `j` after `is` or `as`, and the token
    /// after the type. Nullable types and unknown names are not narrowings.
    fn checked_type(&self, u: UnitId, j: usize) -> Option<(Ty, usize)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let mut k = j;
        let mut name = String::new();
        while matches!(lx.kind(k), Some(Tok::Ident | Tok::Dot))
            && (k == j || lx.kind(k) == Some(Tok::Dot) || lx.kind(k - 1) == Some(Tok::Dot))
        {
            name.push_str(lx.text(src, k));
            k += 1;
        }
        if name.is_empty() {
            return None;
        }
        if lx.kind(k) == Some(Tok::Lt) {
            k = matching_gt(lx, k)? + 1;
        }
        if lx.kind(k) == Some(Tok::Question) {
            return None;
        }
        let decl = self.decl_containing(u, lx.tokens[j].start);
        let sym = self.resolve_type(Ctx { unit: u, decl }, &name)?;
        Some((self.instance(sym), k))
    }

    /// `<String, Int>` at `j` in an expression: the types and the token after them.
    fn explicit_type_args(&self, u: UnitId, j: usize) -> Option<(Vec<Option<Ty>>, usize)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        if lx.kind(j) != Some(Tok::Lt) {
            return None;
        }
        let gt = matching_gt(lx, j)?;
        let decl = self.decl_containing(u, lx.tokens[j].start);
        let mut out = Vec::new();
        let mut k = j + 1;
        while k < gt {
            let start = k;
            let mut depth = 0;
            while k < gt && !(depth == 0 && lx.kind(k) == Some(Tok::Comma)) {
                match lx.kind(k) {
                    Some(Tok::Lt) => depth += 1,
                    Some(Tok::Gt) => depth -= 1,
                    _ => {}
                }
                k += 1;
            }
            let name: String =
                (start..k).take_while(|&t| matches!(lx.kind(t), Some(Tok::Ident | Tok::Dot))).map(|t| lx.text(src, t)).collect();
            let ty = self.resolve_type(Ctx { unit: u, decl }, &name).map(|s| self.instance(s));
            out.push(ty);
            k += 1;
        }
        Some((out, gt + 1))
    }

    /// What a call of one of `found` returns, choosing the overload by the number of arguments.
    fn call_found(&self, u: UnitId, found: &[Found], args: &[Arg], targs: Vec<Option<Ty>>) -> Option<Ty> {
        let members: Vec<&Member> = found.iter().filter_map(|f| if let Found::Sym(m) = f { Some(m) } else { None }).collect();
        let m = self.pick_overload(u, &members, args)?;
        self.call_ret(u, m, args, &targs)
    }

    /// The types of a call's arguments; unknown for lambdas and for what cannot be inferred.
    pub fn arg_types(&self, u: UnitId, args: &[Arg]) -> Vec<Option<Ty>> {
        args.iter().map(|a| if a.lambda { None } else { self.infer(u, a.start, a.end) }).collect()
    }

    /// How well the call `args` fits the function or class `m`, or `None` when it cannot be called
    /// that way: too few or too many arguments, an unknown name, a lambda with the wrong number of
    /// parameters, or an argument whose type is no subtype of the parameter's. Arguments of
    /// unknown type fit anything; closer subtypes and exact matches score higher.
    pub fn call_fit(&self, m: &Member, args: &[Arg], arg_types: &[Option<Ty>], u: UnitId) -> Option<i32> {
        let d = self.decl(m.sym);
        if d.kind != Kind::Function && d.kind != Kind::Constructor && !d.kind.is_class() {
            return Some(0);
        }
        let decls = &self.file(m.sym.unit_id()).decls;
        let ctx = Ctx { unit: m.sym.unit_id(), decl: Some(m.sym.decl_id()) };
        let params = &d.params;
        let mut used = vec![false; params.len()];
        let mut index = 0;
        let mut score = 0;
        for (arg, ty) in args.iter().zip(arg_types) {
            let pi = if let Some(n) = &arg.name {
                params.iter().position(|&p| decls[p].name == *n)?
            } else if arg.trailing {
                params.len().checked_sub(1)?
            } else if index < params.len() {
                let i = index;
                if !decls[params[i]].vararg {
                    index += 1;
                }
                i
            } else {
                return None;
            };
            let param = &decls[params[pi]];
            if std::mem::replace(&mut used[pi], true) && !param.vararg {
                return None;
            }
            let Some(pty) = param.ty.as_ref().and_then(|t| self.ty_of_ref(t, ctx, &m.subst)) else { continue };
            if arg.lambda {
                match &pty {
                    Ty::Func { params: fp, .. } => {
                        match self.lambda_arity(u, arg.start) {
                            Some(n) if n != fp.len() => return None,
                            None if fp.len() > 1 => return None,
                            _ => {}
                        }
                        score += 4;
                    }
                    // A SAM interface, as far as we can tell.
                    Ty::Class { .. } => {}
                    _ => {}
                }
            } else if let Some(at) = ty {
                score += self.type_fit(at, &pty)?;
            }
        }
        for (i, &p) in params.iter().enumerate() {
            if !used[i] && !decls[p].has_default && !decls[p].vararg {
                return None;
            }
        }
        Some(score)
    }

    /// The number of parameters a lambda declares, or `None` when it has the implicit `it`.
    fn lambda_arity(&self, u: UnitId, brace: usize) -> Option<usize> {
        let lx = &self.file(u).lexed;
        let close = lx.matching.get(brace).copied().flatten().filter(|&c| c > brace)?;
        let arrow = (brace + 1..close).find(|&i| lx.kind(i) == Some(Tok::Arrow) && depth_between(lx, brace + 1, i) == 0)?;
        if arrow == brace + 1 {
            return Some(0);
        }
        let commas = (brace + 1..arrow).filter(|&i| lx.kind(i) == Some(Tok::Comma) && depth_between(lx, brace + 1, i) == 0).count();
        Some(commas + 1)
    }

    /// How well a value of type `arg` fits a parameter of type `param`: `None` when it cannot be
    /// passed, more for an exact type than for a distant supertype.
    pub fn type_fit(&self, arg: &Ty, param: &Ty) -> Option<i32> {
        match (arg, param) {
            (Ty::Func { params: a, .. }, Ty::Func { params: b, .. }) => (a.len() == b.len()).then_some(8),
            (Ty::Class { .. }, Ty::Func { .. }) => None,
            (Ty::Func { .. } | Ty::Class { .. }, Ty::Class { sym: p, nullable: pn, .. }) => {
                if let Ty::Class { nullable: true, .. } = arg
                    && !pn
                {
                    return None;
                }
                let want = self.fq(*p);
                if let Ty::Class { sym: a, .. } = arg
                    && self.fq(*a) == want
                {
                    return Some(10);
                }
                if let Some(i) = self.closure(arg).iter().position(|(s, _)| self.fq(*s) == want) {
                    return Some(9 - i.min(8) as i32);
                }
                // Integer literals convert to the other numeric types.
                const NUMERIC: [&str; 6] = ["Int", "Long", "Short", "Byte", "Double", "Float"];
                let simple = |s: &str| s.rsplit('.').next().unwrap_or_default().to_owned();
                let numeric = |t: &Ty| matches!(t, Ty::Class { sym, .. } if self.fq(*sym).starts_with("kotlin.") && NUMERIC.contains(&simple(&self.fq(*sym)).as_str()));
                (numeric(arg) && numeric(param)).then_some(1)
            }
            _ => Some(0),
        }
    }

    /// Among candidates for a call, the one that fits its arguments best; the first of equally
    /// good ones.
    pub fn pick_overload<'m>(&self, u: UnitId, members: &[&'m Member], args: &[Arg]) -> Option<&'m Member> {
        let argc = args.len();
        if members.iter().filter(|m| self.decl(m.sym).kind == Kind::Function).count() > 1 {
            let types = self.arg_types(u, args);
            let mut best: Option<(i32, &'m Member)> = None;
            for &m in members {
                if self.decl(m.sym).kind != Kind::Function {
                    continue;
                }
                if let Some(s) = self.call_fit(m, args, &types, u)
                    && best.is_none_or(|(b, _)| s > b)
                {
                    best = Some((s, m));
                }
            }
            if let Some((_, m)) = best {
                return Some(m);
            }
        }
        let callable = |m: &Member| {
            let d = self.decl(m.sym);
            d.kind == Kind::Function || d.kind.is_class() || d.kind == Kind::Constructor || d.kind.is_value()
        };
        let fits = |m: &Member| {
            let d = self.decl(m.sym);
            let decls = &self.file(m.sym.unit_id()).decls;
            let required = d.params.iter().filter(|&&p| !decls[p].has_default && !decls[p].vararg).count();
            let vararg = d.params.iter().any(|&p| decls[p].vararg);
            d.kind == Kind::Function && argc >= required && (vararg || argc <= d.params.len())
        };
        members.iter().copied().find(|m| fits(m)).or_else(|| members.iter().copied().find(|m| callable(m)))
    }

    /// What calling `m` with `args` returns: a function's return type, a class's instance.
    pub fn call_ret(&self, u: UnitId, m: &Member, args: &[Arg], targs: &[Option<Ty>]) -> Option<Ty> {
        let d = self.decl(m.sym);
        let mut subst = m.subst.clone();
        for (tp, ta) in d.type_params.iter().zip(targs) {
            if let Some(t) = ta {
                subst.insert(tp.name.clone(), t.clone());
            }
        }
        match d.kind {
            Kind::Function => {
                self.bind_args(m, &mut subst, u, args, false);
                self.value_ty(m.sym, &subst)
            }
            Kind::Object | Kind::Companion => self.call_member(&self.instance(m.sym), "invoke", u),
            k if k.is_class() => {
                // A constructor call: bind the class's type parameters from the primary
                // constructor's arguments.
                let ctor = Member { sym: m.sym, subst: subst.clone() };
                self.bind_args(&ctor, &mut subst, u, args, false);
                let args = d.type_params.iter().map(|p| subst.get(&p.name).cloned()).collect();
                Some(Ty::Class { sym: m.sym, args, nullable: false })
            }
            Kind::TypeAlias => {
                let target = self.value_ty(m.sym, &subst)?;
                match target {
                    Ty::Static(s) => Some(self.instance(s)),
                    t => Some(t),
                }
            }
            _ => match self.value_ty(m.sym, &subst)? {
                Ty::Func { ret, .. } => ret.map(|r| *r),
                other => self.call_member(&other, "invoke", u),
            },
        }
    }

    /// `.name` on `cur` at token `name_tok`: its type and the token after it (and after the call).
    fn member_access(&self, u: UnitId, cur: &Ty, name_tok: usize, b: usize) -> Option<(Ty, usize)> {
        let file = self.file(u);
        let lx = &file.lexed;
        let src = &self.unit(u).text;
        let name = lx.text(src, name_tok);
        let mut j = name_tok + 1;
        let targs = self.explicit_type_args(u, j);
        if let Some((_, next)) = &targs {
            j = *next;
        }
        let is_call = matches!(lx.kind(j), Some(Tok::LParen | Tok::LBrace)) && !lx.tokens[j].nl;
        if let Ty::Package(p) = cur {
            let fq = format!("{p}.{name}");
            if let Some(class) = self.best_type(self.by_fq(&fq).into_iter().filter(|&s| is_type_decl(self.decl(s)))) {
                if is_call {
                    let (args, next) = self.call_args(u, j, b);
                    return Some((self.call_ret(u, &Member::plain(class), &args, &[])?, next));
                }
                return Some((self.value_ty(class, &Subst::new())?, j));
            }
            if self.has_package(&fq) {
                return Some((Ty::Package(fq), j));
            }
            let members: Vec<Member> = self.by_fq(&fq).into_iter().map(Member::plain).collect();
            let found: Vec<Found> = members.into_iter().map(Found::Sym).collect();
            if is_call {
                let (args, next) = self.call_args(u, j, b);
                return Some((self.call_found(u, &found, &args, Vec::new())?, next));
            }
            let Found::Sym(m) = found.first()? else { return None };
            return Some((self.value_ty(m.sym, &m.subst)?, j));
        }
        let members = self.members_named(cur, name, u);
        if is_call {
            let (args, next) = self.call_args(u, j, b);
            let refs: Vec<&Member> = members.iter().collect();
            let m = self.pick_overload(u, &refs, &args)?;
            let ty = self.call_ret(u, m, &args, &targs.map(|t| t.0).unwrap_or_default())?;
            return Some((ty, next));
        }
        let m = members.iter().find(|m| self.decl(m.sym).kind != Kind::Function)?;
        Some((self.value_ty(m.sym, &m.subst)?, j))
    }
}

fn is_type_decl(d: &Decl) -> bool {
    d.kind.is_class() || d.kind == Kind::TypeAlias
}

/// Functions and values differ; property-like kinds share a key.
fn value_kind(kind: Kind) -> u8 {
    match kind {
        Kind::Function | Kind::Constructor => 0,
        k if k.is_value() => 1,
        _ => 2,
    }
}

pub fn nullable(ty: Ty, nullable: bool) -> Ty {
    match ty {
        Ty::Class { sym, args, nullable: n } => Ty::Class { sym, args, nullable: n || nullable },
        t => t,
    }
}

pub fn strip_nullable(ty: Ty) -> Ty {
    match ty {
        Ty::Class { sym, args, .. } => Ty::Class { sym, args, nullable: false },
        t => t,
    }
}

/// Whether token `k` can end an operand (for spotting infix calls and binary operators).
fn is_operand_end(src: &str, lx: &Lexed, k: usize) -> bool {
    match lx.tokens[k].kind {
        Tok::Ident => {
            !HARD_KEYWORDS.contains(&&src[lx.tokens[k].span()]) || matches!(&src[lx.tokens[k].span()], "this" | "true" | "false" | "null")
        }
        Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose | Tok::Int | Tok::Float | Tok::Char | Tok::BangBang | Tok::LabelRef => {
            true
        }
        _ => false,
    }
}

/// The `<` that the `>` at `gt` closes, when it encloses only type syntax.
pub fn matching_lt(lx: &Lexed, gt: usize) -> Option<usize> {
    let mut depth = 0;
    let mut k = gt;
    loop {
        match lx.kind(k)? {
            Tok::Gt => depth += 1,
            Tok::Lt => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            Tok::Ident | Tok::Dot | Tok::Comma | Tok::Question | Tok::LParen | Tok::RParen | Tok::Arrow | Tok::ColonColon => {}
            Tok::Op if lx.tokens[k].end - lx.tokens[k].start == 1 => {}
            _ => return None,
        }
        k = k.checked_sub(1)?;
    }
}

/// The `>` closing type arguments at `lt`, when what follows makes them type arguments (a
/// call, a callable reference, a member access or the end of a type).
pub fn matching_gt(lx: &Lexed, lt: usize) -> Option<usize> {
    let mut depth = 0;
    for k in lt..lx.tokens.len().min(lt + 64) {
        match lx.kind(k)? {
            Tok::Lt => depth += 1,
            Tok::Gt => {
                depth -= 1;
                if depth == 0 {
                    return matches!(lx.kind(k + 1), Some(Tok::LParen | Tok::ColonColon | Tok::Dot | Tok::LBrace) | None)
                        .then_some(k)
                        .filter(|_| !lx.tokens.get(k + 1).is_some_and(|t| t.nl && t.kind == Tok::LBrace));
                }
            }
            Tok::Ident | Tok::Dot | Tok::Comma | Tok::Question | Tok::Arrow | Tok::Colon => {}
            Tok::LParen | Tok::RParen => {}
            Tok::Op if lx.tokens[k].end - lx.tokens[k].start == 1 => {}
            _ => return None,
        }
    }
    None
}

/// The `(` of the call whose arguments contain token `p`.
fn enclosing_paren(lx: &Lexed, p: usize) -> Option<usize> {
    let mut k = p;
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

/// The bracket depth of token `i` relative to `from`.
fn depth_between(lx: &Lexed, from: usize, i: usize) -> i32 {
    let mut depth = 0;
    for k in from..i {
        match lx.tokens[k].kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::StrOpen | Tok::TemplateOpen => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The first token of the receiver expression that ends before token `end`: `a.b(c)[0]!!`.
pub fn chain_start(src: &str, lx: &Lexed, end: usize) -> usize {
    let mut i = end;
    loop {
        if i == 0 {
            return 0;
        }
        let t = i - 1;
        match lx.tokens[t].kind {
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::StrClose => match lx.matching[t] {
                Some(o) if o < t => {
                    i = o;
                    // A string, parenthesized expression or lambda may start the chain.
                    if lx.tokens[t].kind == Tok::StrClose {
                        if o > 0 && matches!(lx.kind(o - 1), Some(Tok::Dot | Tok::SafeDot)) {
                            i = o - 1;
                        } else {
                            return o;
                        }
                    }
                    // `when (x) { ... }.foo`: the `when` starts the chain.
                    if lx.tokens[t].kind == Tok::RBrace {
                        let mut h = o;
                        if h > 0 && lx.kind(h - 1) == Some(Tok::RParen) {
                            h = lx.matching[h - 1].filter(|&c| c < h - 1).unwrap_or(h);
                        }
                        if h > 0 && lx.is(src, h - 1, "when") {
                            return h - 1;
                        }
                    }
                    continue;
                }
                _ => return i,
            },
            Tok::Gt => match matching_lt(lx, t) {
                Some(lt) if lt > 0 && lx.kind(lt - 1) == Some(Tok::Ident) => i = lt,
                _ => return i,
            },
            Tok::BangBang => i = t,
            Tok::LabelRef => i = t,
            Tok::Ident | Tok::Int | Tok::Float | Tok::Char => {
                let word = &src[lx.tokens[t].span()];
                if lx.tokens[t].kind == Tok::Ident
                    && HARD_KEYWORDS.contains(&word)
                    && !matches!(word, "this" | "super" | "null" | "true" | "false")
                {
                    return i;
                }
                i = t;
                if t > 0 && matches!(lx.kind(t - 1), Some(Tok::Dot | Tok::SafeDot)) {
                    i = t - 1;
                    continue;
                }
                return i;
            }
            Tok::Dot | Tok::SafeDot => {
                // Directly after a closing bracket: `foo().bar`.
                i = t;
                continue;
            }
            _ => return i,
        }
    }
}
