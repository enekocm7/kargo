//! Kotlin/Wasm `external` declarations for a [`Program`] of TypeScript declarations.
//!
//! Kotlin/Wasm interop only passes JS-compatible types (`String`, `Int`, `Double`, `Boolean`,
//! function types, `JsAny` and external types), so the TypeScript types are simplified:
//!
//! - Interfaces and classes become `external interface X : JsAny`, with the members of every
//!   supertype copied in (no Kotlin supertypes, so overloads that differ only in TypeScript never
//!   conflict). Generic types are not generic in Kotlin: type parameters take their defaults.
//! - A property whose type is callable (`get: IRouterMatcher<this>`) becomes methods, one per call
//!   signature; an interface that is only callable (`RequestHandler`) is a Kotlin function type.
//! - Unions become overloads in parameters and `JsAny` elsewhere; `number` parameters accept both
//!   `Int` and `Double`; a rest parameter becomes a `vararg` (and a single parameter, so a trailing
//!   lambda works); optional parameters default to `definedExternally`.
//! - Only the npm packages the project depends on get functions, values and classes (imported
//!   with `@JsModule`); the packages their declarations refer to only contribute types.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use super::parser::{DeclKind, MemberKind, Sig, Type};
use super::program::{DeclId, Ent, Program, ScopeId, package_of};

/// The most interfaces generated, so that a large dependency graph stays bounded; types beyond it are `JsAny`.
const MAX_TYPES: usize = 600;
/// The most overloads one TypeScript signature expands into.
const MAX_VARIANTS: usize = 24;

/// A TypeScript type with its names resolved.
#[derive(Debug, Clone, PartialEq)]
enum RT {
    /// `any`, `unknown` and types Kotlin can't express: `JsAny?`.
    Any,
    Void,
    Null,
    Str,
    Num,
    Bool,
    This,
    /// A type parameter of the interface being read.
    Param(String),
    Ent(Ent, Vec<RT>),
    TypeOf(Ent),
    Array(Box<RT>),
    Promise(Box<RT>),
    Union(Vec<RT>),
    Inter(Vec<RT>),
    Fn(Box<RSig>),
    /// An object Kotlin has no type for (`object`, `Date`, ...): `JsAny`.
    Opaque,
}

#[derive(Debug, Clone, PartialEq)]
struct RSig {
    params: Vec<RParam>,
    ret: RT,
}

#[derive(Debug, Clone, PartialEq)]
struct RParam {
    name: String,
    ty: RT,
    optional: bool,
    rest: bool,
}

type Bindings = HashMap<String, RT>;

/// A Kotlin type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum KT {
    Str,
    Num,
    Int,
    Bool,
    Unit,
    Any,
    AnyQ,
    Named(String),
    Array(Box<KT>),
    Promise(Box<KT>),
    Fun(Vec<KT>, Box<KT>),
    Nullable(Box<KT>),
}

impl KT {
    fn render(&self) -> String {
        match self {
            KT::Str => "String".into(),
            KT::Num => "Double".into(),
            KT::Int => "Int".into(),
            KT::Bool => "Boolean".into(),
            KT::Unit => "Unit".into(),
            KT::Any => "JsAny".into(),
            KT::AnyQ => "JsAny?".into(),
            KT::Named(n) => n.clone(),
            KT::Array(e) => format!("JsArray<{}>", e.render()),
            KT::Promise(e) => format!("Promise<{}>", e.render()),
            KT::Fun(params, ret) => format!("({}) -> {}", params.iter().map(KT::render).collect::<Vec<_>>().join(", "), ret.render()),
            KT::Nullable(inner) => match **inner {
                KT::Fun(..) => format!("({})?", inner.render()),
                _ => format!("{}?", inner.render()),
            },
        }
    }

    /// The type as a JS-typed element of `JsArray` or `Promise`.
    fn element(self) -> KT {
        match self {
            KT::Str => KT::Named("JsString".into()),
            KT::Num | KT::Int => KT::Named("JsNumber".into()),
            KT::Bool => KT::Named("JsBoolean".into()),
            KT::Unit => KT::AnyQ,
            KT::Fun(..) => KT::Any,
            KT::Nullable(inner) => inner.element().nullable(),
            other => other,
        }
    }

    fn nullable(self) -> KT {
        match self {
            KT::Unit | KT::AnyQ | KT::Nullable(_) => self,
            other => KT::Nullable(Box::new(other)),
        }
    }

    fn is_fun(&self) -> bool {
        matches!(self, KT::Fun(..))
    }

    /// Whether `JsAny` accepts the type, so that a `JsAny` overload makes one for it redundant.
    fn is_js_object(&self) -> bool {
        match self {
            KT::Named(_) | KT::Array(_) | KT::Promise(_) | KT::Any => true,
            KT::Nullable(inner) => inner.is_js_object(),
            _ => false,
        }
    }

    /// The type as far as Kotlin's overload conflicts see it.
    fn erased(&self) -> String {
        match self {
            KT::Fun(params, _) => format!("Function{}", params.len()),
            KT::Array(_) => "JsArray".into(),
            KT::Promise(_) => "Promise".into(),
            KT::Nullable(inner) => inner.erased(),
            KT::AnyQ => "JsAny".into(),
            other => other.render(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pos {
    Param,
    Ret,
    Prop,
    CbParam,
    CbRet,
    Elem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    Optional,
    Vararg,
    /// Left out, with every parameter after it: an optional parameter of several types is an
    /// overload without it and one per type, rather than ambiguous overloads with defaults.
    Omitted,
}

/// A Kotlin function, before rendering.
struct Fun {
    name: String,
    params: Vec<(String, KT, Mode)>,
    ret: KT,
}

impl Fun {
    fn key(&self) -> String {
        let params: Vec<String> =
            self.params.iter().map(|(_, t, m)| if *m == Mode::Vararg { format!("*{}", t.erased()) } else { t.erased() }).collect();
        format!("{}({})", self.name, params.join(","))
    }

    fn render_params(&self) -> String {
        self.params
            .iter()
            .map(|(name, ty, mode)| match mode {
                Mode::Plain => format!("{name}: {}", ty.render()),
                Mode::Optional => format!("{name}: {} = definedExternally", ty.render()),
                Mode::Vararg => format!("vararg {name}: {}", ty.render()),
                Mode::Omitted => unreachable!("omitted parameters are removed"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The members of an interface or class, collected from it and its supertypes.
#[derive(Default)]
struct Members {
    props: Vec<(String, KT, bool, Option<String>)>,
    funs: Vec<(Fun, Option<String>)>,
    keys: HashSet<String>,
    ctors: Vec<Fun>,
    /// How far up the class hierarchy the constructors were found: a class inherits the nearest ones.
    ctor_depth: Option<usize>,
    index: Option<KT>,
}

impl Members {
    fn add_fun(&mut self, fun: Fun, doc: Option<String>) {
        if self.keys.insert(fun.key()) {
            self.funs.push((fun, doc));
        }
    }
}

/// How a class is imported from its module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassImport {
    /// The module's default export.
    Default(String),
    /// A named export of the module.
    Named,
}

/// The generated Kotlin, by Kotlin package: its declarations and the JS module of its exports.
#[derive(Default)]
pub struct Output {
    pub packages: BTreeMap<String, KotlinPackage>,
}

#[derive(Default)]
pub struct KotlinPackage {
    /// The npm package the Kotlin package is for.
    pub npm: String,
    /// Interfaces, type aliases and extensions.
    pub types: Vec<String>,
    /// Functions, values and classes imported by name from `npm` (`@file:JsModule`).
    pub exports: Vec<String>,
    /// The module's default export (`@JsModule` on each declaration).
    pub default: Vec<String>,
}

pub struct Generator<'a> {
    prog: &'a mut Program,
    out: Output,
    /// Kotlin names of interfaces and classes, by their first declaration.
    names: HashMap<DeclId, String>,
    taken: HashSet<(String, String)>,
    queue: Vec<(Vec<DeclId>, String)>,
    classes: HashMap<DeclId, ClassImport>,
    /// The Kotlin package of the npm package being generated, where extensions go: code that
    /// imports from it (`import express.*`) gets them without importing the packages it builds on.
    root: String,
    depth: usize,
}

impl<'a> Generator<'a> {
    pub fn new(prog: &'a mut Program) -> Self {
        Generator {
            prog,
            out: Output::default(),
            names: HashMap::new(),
            taken: HashSet::new(),
            queue: Vec::new(),
            classes: HashMap::new(),
            root: String::new(),
            depth: 0,
        }
    }

    /// Declarations for the npm package `npm`, whose module is `module`.
    pub fn package(&mut self, npm: &str, module: ScopeId) {
        let pkg = kotlin_package(npm);
        self.root = pkg.clone();
        self.out.packages.entry(pkg.clone()).or_default().npm = npm.to_owned();
        let default = self.prog.default_export(module);
        let names = self.prog.export_names(module);
        let exports: Vec<(String, Option<Ent>)> = names.into_iter().map(|n| (n.clone(), self.prog.export(module, &n))).collect();

        // Classes first, so that references to them use the class.
        if let Some(Ent::Decls(decls)) = &default {
            for &d in decls {
                if self.is_class(d) {
                    self.classes.insert(d, ClassImport::Default(npm.to_owned()));
                }
            }
        }
        for (_, ent) in &exports {
            if let Some(Ent::Decls(decls)) = ent {
                for &d in decls {
                    if self.is_class(d) {
                        self.classes.entry(d).or_insert(ClassImport::Named);
                    }
                }
            }
        }

        if let Some(Ent::Decls(decls)) = default {
            let name = function_name(npm);
            for d in decls {
                let texts = self.value(d, &name, &format!("@JsModule(\"{npm}\")\n"));
                self.package_mut(&pkg).default.extend(texts);
                if self.is_class(d) {
                    self.name_for(&[d]);
                }
            }
        }
        for (name, ent) in exports {
            let Some(Ent::Decls(decls)) = ent else { continue };
            for &d in &decls {
                if matches!(self.prog.decls[d].decl.kind, DeclKind::Function(_) | DeclKind::Var(_)) {
                    let texts = self.value(d, &name, "");
                    self.package_mut(&pkg).exports.extend(texts);
                }
            }
            self.export_type(&pkg, &name, &decls);
        }
        self.drain();
    }

    pub fn finish(mut self) -> Output {
        self.drain();
        self.out
    }

    fn package_mut(&mut self, pkg: &str) -> &mut KotlinPackage {
        self.out.packages.entry(pkg.to_owned()).or_default()
    }

    fn is_class(&self, d: DeclId) -> bool {
        matches!(&self.prog.decls[d].decl.kind, DeclKind::Interface(i) if i.is_class)
    }

    fn drain(&mut self) {
        while let Some((decls, fq)) = self.queue.pop() {
            self.emit_type(&decls, &fq);
        }
    }

    /// An exported interface, class or type alias under its exported name.
    fn export_type(&mut self, pkg: &str, name: &str, decls: &[DeclId]) {
        let Some(simple) = identifier(name) else { return };
        if decls.iter().any(|&d| matches!(self.prog.decls[d].decl.kind, DeclKind::Interface(_))) {
            let ent = Ent::Decls(decls.to_vec());
            let target = self.ent_kt(&ent, &[], Pos::Param, &None);
            if let KT::Named(fq) = target
                && fq != format!("{pkg}.{simple}")
                && self.taken.insert((pkg.to_owned(), simple.clone()))
            {
                self.package_mut(pkg).types.push(format!("typealias {simple} = {fq}"));
            }
            return;
        }
        if let Some(&d) = decls.iter().find(|&&d| matches!(self.prog.decls[d].decl.kind, DeclKind::Alias { .. })) {
            let body = self.alias_body(d, &[]);
            let target = self.kt(&body, Pos::Param, &None);
            if (target.is_fun() || matches!(target, KT::Named(_))) && self.taken.insert((pkg.to_owned(), simple.clone())) {
                self.package_mut(pkg).types.push(format!("typealias {simple} = {}", target.render()));
            }
        }
    }

    /// A function or variable declaration as top-level Kotlin declarations named `name`, each
    /// with `annotation` after its documentation.
    fn value(&mut self, d: DeclId, name: &str, annotation: &str) -> Vec<String> {
        let Some(kname) = identifier(name) else { return Vec::new() };
        let scope = self.prog.decls[d].scope;
        let doc = self.prog.decls[d].decl.doc.clone();
        let kind = self.prog.decls[d].decl.kind.clone();
        let mut out = Vec::new();
        let mut keys = HashSet::new();
        let sigs = match &kind {
            DeclKind::Function(sig) => vec![self.rsig(sig, scope, &Bindings::new())],
            DeclKind::Var(ty) => {
                let r = self.rt(ty, scope, &Bindings::new());
                let sigs = self.callables(&r);
                if sigs.is_empty() {
                    let t = self.kt(&r, Pos::Prop, &None);
                    return vec![format!("{}{annotation}external val {kname}: {}", kdoc(&doc, ""), t.render())];
                }
                sigs
            }
            _ => return out,
        };
        for sig in sigs {
            for fun in self.variants(&kname, &sig, &None) {
                if keys.insert(fun.key()) {
                    out.push(format!("{}{annotation}external fun {kname}({}): {}", kdoc(&doc, ""), fun.render_params(), fun.ret.render()));
                }
            }
        }
        out
    }

    // ----- resolving types -----

    fn rt(&mut self, ty: &Type, scope: ScopeId, env: &Bindings) -> RT {
        match ty {
            Type::Any | Type::Unknown | Type::Never | Type::Other => RT::Any,
            Type::Void | Type::Undefined => RT::Void,
            Type::Null => RT::Null,
            Type::Object | Type::BigInt | Type::Symbol | Type::Ctor(_) | Type::Obj(_) => RT::Opaque,
            Type::String | Type::StrLit => RT::Str,
            Type::Number | Type::NumLit => RT::Num,
            Type::Boolean | Type::BoolLit => RT::Bool,
            Type::This => RT::This,
            Type::Tuple => RT::Array(Box::new(RT::Any)),
            Type::Array(t) => RT::Array(Box::new(self.rt(t, scope, env))),
            Type::Union(ts) => RT::Union(ts.iter().map(|t| self.rt(t, scope, env)).collect()),
            Type::Inter(ts) => RT::Inter(ts.iter().map(|t| self.rt(t, scope, env)).collect()),
            Type::Fn(sig) => RT::Fn(Box::new(self.rsig(sig, scope, env))),
            Type::Query(path) => self.prog.resolve(scope, path).map_or(RT::Opaque, RT::TypeOf),
            Type::Ref(path, args) => {
                if path.len() == 1
                    && let Some(bound) = env.get(&path[0])
                {
                    return bound.clone();
                }
                let args: Vec<RT> = args.iter().map(|a| self.rt(a, scope, env)).collect();
                match self.prog.resolve(scope, path) {
                    Some(ent) => RT::Ent(ent, args),
                    None => builtin(path.last().map(String::as_str).unwrap_or_default(), args),
                }
            }
            Type::Import(spec, path, args) => {
                let args: Vec<RT> = args.iter().map(|a| self.rt(a, scope, env)).collect();
                let Some(module) = self.prog.module(scope, spec) else { return RT::Opaque };
                let mut ent = Ent::Module(module);
                for name in path {
                    match self.prog.member(&ent, name) {
                        Some(e) => ent = e,
                        None => return RT::Opaque,
                    }
                }
                RT::Ent(ent, args)
            }
        }
    }

    /// A signature, with its own type parameters replaced by their defaults or constraints.
    fn rsig(&mut self, sig: &Sig, scope: ScopeId, env: &Bindings) -> RSig {
        let mut env = env.clone();
        for tp in &sig.tparams {
            let value = match (&tp.default, &tp.constraint) {
                (Some(t), _) | (None, Some(t)) => self.rt(t, scope, &env),
                (None, None) => RT::Any,
            };
            env.insert(tp.name.clone(), value);
        }
        let params = sig
            .params
            .iter()
            .map(|p| RParam { name: p.name.clone(), ty: self.rt(&p.ty, scope, &env), optional: p.optional, rest: p.rest })
            .collect();
        RSig { params, ret: self.rt(&sig.ret, scope, &env) }
    }

    /// The interface-level environment of a declaration: its type parameters stay parameters.
    fn own_env(&self, d: DeclId) -> Bindings {
        let tparams = match &self.prog.decls[d].decl.kind {
            DeclKind::Interface(i) => &i.tparams,
            DeclKind::Alias { tparams, .. } => tparams,
            _ => return Bindings::new(),
        };
        tparams.iter().map(|tp| (tp.name.clone(), RT::Param(tp.name.clone()))).collect()
    }

    /// The values of a declaration's type parameters for `args`: the arguments, else the defaults.
    fn bindings(&mut self, d: DeclId, args: &[RT]) -> Bindings {
        let tparams = match &self.prog.decls[d].decl.kind {
            DeclKind::Interface(i) => i.tparams.clone(),
            DeclKind::Alias { tparams, .. } => tparams.clone(),
            _ => return Bindings::new(),
        };
        let scope = self.prog.decls[d].scope;
        let env = self.own_env(d);
        let mut out = Bindings::new();
        for (i, tp) in tparams.iter().enumerate() {
            let value = match args.get(i) {
                Some(a) => a.clone(),
                None => match (&tp.default, &tp.constraint) {
                    (Some(t), _) | (None, Some(t)) => {
                        let r = self.rt(t, scope, &env);
                        subst(&r, &out)
                    }
                    (None, None) => RT::Any,
                },
            };
            out.insert(tp.name.clone(), value);
        }
        out
    }

    fn alias_body(&mut self, d: DeclId, args: &[RT]) -> RT {
        let DeclKind::Alias { ty, .. } = self.prog.decls[d].decl.kind.clone() else { return RT::Any };
        let scope = self.prog.decls[d].scope;
        let env = self.own_env(d);
        let body = self.rt(&ty, scope, &env);
        let b = self.bindings(d, args);
        subst(&body, &b)
    }

    fn interfaces(&self, ent: &Ent) -> Vec<DeclId> {
        match ent {
            Ent::Decls(decls) => {
                decls.iter().copied().filter(|&d| matches!(self.prog.decls[d].decl.kind, DeclKind::Interface(_))).collect()
            }
            Ent::Module(_) => Vec::new(),
        }
    }

    fn alias(&self, ent: &Ent) -> Option<DeclId> {
        match ent {
            Ent::Decls(decls) => decls.iter().copied().find(|&d| matches!(self.prog.decls[d].decl.kind, DeclKind::Alias { .. })),
            Ent::Module(_) => None,
        }
    }

    fn enter(&mut self) -> bool {
        if self.depth > 24 {
            return false;
        }
        self.depth += 1;
        true
    }

    /// The call signatures of a type: a function type's, an interface's (and its supertypes'), or
    /// those of every part of an intersection.
    fn callables(&mut self, rt: &RT) -> Vec<RSig> {
        if !self.enter() {
            return Vec::new();
        }
        let out = match rt {
            RT::Fn(sig) => vec![(**sig).clone()],
            RT::Inter(ts) => ts.iter().flat_map(|t| self.callables(t)).collect(),
            RT::TypeOf(Ent::Decls(decls)) => {
                let mut out = Vec::new();
                for &d in decls {
                    if let DeclKind::Function(sig) = self.prog.decls[d].decl.kind.clone() {
                        let scope = self.prog.decls[d].scope;
                        out.push(self.rsig(&sig, scope, &Bindings::new()));
                    }
                }
                out
            }
            RT::Ent(ent, args) => {
                if let Some(alias) = self.alias(ent) {
                    let body = self.alias_body(alias, args);
                    self.callables(&body)
                } else {
                    let mut out = Vec::new();
                    for d in self.interfaces(ent) {
                        out.extend(self.interface_callables(d, args));
                    }
                    out
                }
            }
            _ => Vec::new(),
        };
        self.depth -= 1;
        out
    }

    fn interface_callables(&mut self, d: DeclId, args: &[RT]) -> Vec<RSig> {
        let DeclKind::Interface(iface) = self.prog.decls[d].decl.kind.clone() else { return Vec::new() };
        if iface.is_class {
            return Vec::new();
        }
        let scope = self.prog.decls[d].scope;
        let env = self.own_env(d);
        let b = self.bindings(d, args);
        let mut out = Vec::new();
        for m in &iface.members {
            if let MemberKind::Call(sig) = &m.kind {
                let s = self.rsig(sig, scope, &env);
                out.push(subst_sig(&s, &b));
            }
        }
        for ext in &iface.extends {
            let r = self.rt(ext, scope, &env);
            out.extend(self.callables(&subst(&r, &b)));
        }
        out
    }

    /// Whether an interface (with its supertypes) has properties or methods.
    fn has_members(&mut self, ent: &Ent) -> bool {
        if !self.enter() {
            return true;
        }
        let mut out = false;
        for d in self.interfaces(ent) {
            let DeclKind::Interface(iface) = self.prog.decls[d].decl.kind.clone() else { continue };
            if iface.is_class
                || iface
                    .members
                    .iter()
                    .any(|m| matches!(m.kind, MemberKind::Prop { .. } | MemberKind::Method { .. } | MemberKind::Index(_)))
            {
                out = true;
                break;
            }
            let scope = self.prog.decls[d].scope;
            let env = self.own_env(d);
            for ext in &iface.extends {
                if let RT::Ent(e, _) = self.rt(ext, scope, &env)
                    && self.has_members(&e)
                {
                    out = true;
                }
            }
        }
        self.depth -= 1;
        out
    }

    /// The single supertype of an interface that declares nothing itself (`interface A extends B {}`).
    fn only_supertype(&mut self, ent: &Ent, args: &[RT]) -> Option<RT> {
        let decls = self.interfaces(ent);
        let mut found = None;
        for &d in &decls {
            let DeclKind::Interface(iface) = &self.prog.decls[d].decl.kind else { return None };
            if iface.is_class || !iface.members.is_empty() || self.classes.contains_key(&d) {
                return None;
            }
            for ext in iface.extends.clone() {
                if found.is_some() {
                    return None;
                }
                let scope = self.prog.decls[d].scope;
                let env = self.own_env(d);
                let r = self.rt(&ext, scope, &env);
                let b = self.bindings(d, args);
                found = Some(subst(&r, &b));
            }
        }
        found.filter(|r| matches!(r, RT::Ent(..)))
    }

    // ----- Kotlin types -----

    fn kt(&mut self, rt: &RT, pos: Pos, this: &Option<String>) -> KT {
        if !self.enter() {
            return KT::AnyQ;
        }
        let out = self.kt_inner(rt, pos, this);
        self.depth -= 1;
        out
    }

    fn kt_inner(&mut self, rt: &RT, pos: Pos, this: &Option<String>) -> KT {
        match rt {
            RT::Any | RT::Null | RT::Param(_) => KT::AnyQ,
            RT::Void => {
                if matches!(pos, Pos::Ret | Pos::CbRet) {
                    KT::Unit
                } else {
                    KT::AnyQ
                }
            }
            RT::Str => KT::Str,
            RT::Num => KT::Num,
            RT::Bool => KT::Bool,
            RT::This => this.clone().map_or(KT::Any, KT::Named),
            RT::Opaque | RT::TypeOf(_) => KT::Any,
            RT::Array(e) => KT::Array(Box::new(self.kt(e, Pos::Elem, this).element())),
            RT::Promise(e) => KT::Promise(Box::new(self.kt(e, Pos::Elem, this).element())),
            RT::Fn(sig) => {
                if matches!(pos, Pos::Param | Pos::CbParam) {
                    self.function_type(sig, this)
                } else {
                    KT::Any
                }
            }
            RT::Union(ts) => {
                let mut nullable = false;
                let mut kts: Vec<KT> = Vec::new();
                for t in ts {
                    if matches!(t, RT::Null | RT::Void) {
                        nullable = true;
                        continue;
                    }
                    let k = self.kt(t, pos, this);
                    if !kts.contains(&k) {
                        kts.push(k);
                    }
                }
                let k = match kts.len() {
                    0 if matches!(pos, Pos::Ret | Pos::CbRet) => KT::Unit,
                    0 => KT::AnyQ,
                    1 => kts.pop().unwrap(),
                    _ if kts.contains(&KT::AnyQ) => KT::AnyQ,
                    _ => KT::Any,
                };
                if nullable { k.nullable() } else { k }
            }
            RT::Inter(ts) => {
                let kts: Vec<KT> = ts.iter().map(|t| self.kt(t, pos, this)).collect();
                kts.iter()
                    .find(|k| matches!(k, KT::Named(_)))
                    .or_else(|| kts.iter().find(|k| !matches!(k, KT::Any | KT::AnyQ)))
                    .cloned()
                    .unwrap_or(KT::Any)
            }
            RT::Ent(ent, args) => self.ent_kt(ent, args, pos, this),
        }
    }

    fn ent_kt(&mut self, ent: &Ent, args: &[RT], pos: Pos, this: &Option<String>) -> KT {
        if let Some(alias) = self.alias(ent) {
            let body = self.alias_body(alias, args);
            return self.kt(&body, pos, this);
        }
        let decls = self.interfaces(ent);
        if decls.is_empty() {
            return match ent {
                // `Array` and friends declared in a package's own globals.
                Ent::Decls(_) => KT::Any,
                Ent::Module(_) => KT::Any,
            };
        }
        if !decls.iter().any(|d| self.classes.contains_key(d)) && !self.has_members(ent) {
            let sigs = self.callables(&RT::Ent(ent.clone(), args.to_vec()));
            if let Some(sig) = sigs.first() {
                return if matches!(pos, Pos::Param | Pos::CbParam) { self.function_type(sig, this) } else { KT::Any };
            }
        }
        if let Some(target) = self.only_supertype(ent, args) {
            return match self.kt(&target, pos, this) {
                KT::Named(fq) => self.alias_for(&decls, &fq).map_or(KT::Any, KT::Named),
                other => other,
            };
        }
        match self.name_for(&decls) {
            Some(fq) => KT::Named(fq),
            None => KT::Any,
        }
    }

    /// A Kotlin function type for a callback: what JS passes it, and what it returns.
    fn function_type(&mut self, sig: &RSig, this: &Option<String>) -> KT {
        let params = sig
            .params
            .iter()
            .filter(|p| !p.rest)
            .map(|p| {
                let k = self.kt(&p.ty, Pos::CbParam, this);
                if p.optional { k.nullable() } else { k }
            })
            .collect();
        let ret = match self.kt(&sig.ret, Pos::CbRet, this) {
            // A callback whose result JS ignores (or takes as anything) returns Unit.
            KT::AnyQ | KT::Promise(_) | KT::Nullable(_) => KT::Unit,
            other => other,
        };
        KT::Fun(params, Box::new(ret))
    }

    /// The Kotlin types a parameter of type `rt` is offered as, one overload each.
    fn alternatives(&mut self, rt: &RT, this: &Option<String>) -> Vec<KT> {
        if !self.enter() {
            return vec![KT::AnyQ];
        }
        let mut out: Vec<KT> = match rt {
            RT::Union(ts) => {
                let nullable = ts.iter().any(|t| matches!(t, RT::Null | RT::Void));
                let mut all = Vec::new();
                for t in ts.iter().filter(|t| !matches!(t, RT::Null | RT::Void)) {
                    all.extend(self.alternatives(t, this));
                }
                if nullable { all.into_iter().map(KT::nullable).collect() } else { all }
            }
            RT::Num => vec![KT::Int, KT::Num],
            // Kotlin strings aren't `JsAny`, and `any` parameters often take one (`res.send("hi")`).
            RT::Any => vec![KT::AnyQ, KT::Str],
            RT::Fn(sig) => vec![self.function_type(sig, this), KT::Any],
            RT::Ent(ent, args) if self.alias(ent).is_some() => {
                let body = self.alias_body(self.alias(ent).unwrap(), args);
                self.alternatives(&body, this)
            }
            _ => {
                let k = self.kt(rt, Pos::Param, this);
                if k.is_fun() { vec![k, KT::Any] } else { vec![k] }
            }
        };
        self.depth -= 1;
        // `JsAny` takes any JS object, so the narrower object types add nothing.
        if out.contains(&KT::AnyQ) {
            out.retain(|k| !k.is_js_object());
        } else if out.contains(&KT::Any) {
            if out.iter().any(|k| matches!(k, KT::Nullable(_)) && k.is_js_object()) {
                out.retain(|k| !k.is_js_object());
                out.push(KT::AnyQ);
            } else {
                out.retain(|k| *k == KT::Any || !k.is_js_object());
            }
        }
        let mut seen = HashSet::new();
        out.retain(|k| seen.insert(k.clone()));
        out.truncate(6);
        out
    }

    /// The Kotlin overloads of a TypeScript signature.
    fn variants(&mut self, name: &str, sig: &RSig, this: &Option<String>) -> Vec<Fun> {
        let mut options: Vec<Vec<(KT, Mode)>> = Vec::new();
        for p in &sig.params {
            if p.rest {
                let element = match &p.ty {
                    RT::Array(e) => (**e).clone(),
                    _ => RT::Any,
                };
                let mut opts = Vec::new();
                for alt in self.alternatives(&element, this) {
                    // A trailing lambda can't be a vararg.
                    if alt.is_fun() {
                        opts.push((alt.clone(), Mode::Plain));
                    }
                    let alt = match alt {
                        KT::Str | KT::Fun(..) => alt,
                        other => other.element(),
                    };
                    opts.push((alt, Mode::Vararg));
                }
                options.push(opts);
            } else {
                let alternatives = self.alternatives(&p.ty, this);
                options.push(match (p.optional, alternatives.len()) {
                    (true, 1) => alternatives.into_iter().map(|k| (k, Mode::Optional)).collect(),
                    // Overloads that differ only in a parameter the call leaves out would be ambiguous.
                    (true, _) => {
                        std::iter::once((KT::AnyQ, Mode::Omitted)).chain(alternatives.into_iter().map(|k| (k, Mode::Plain))).collect()
                    }
                    (false, _) => alternatives.into_iter().map(|k| (k, Mode::Plain)).collect(),
                });
            }
        }
        if options.iter().map(Vec::len).product::<usize>() > MAX_VARIANTS {
            for o in &mut options {
                o.truncate(1);
            }
        }
        let names = param_names(&sig.params);
        let ret = self.kt(&sig.ret, Pos::Ret, this);
        let mut combos: Vec<Vec<(KT, Mode)>> = vec![Vec::new()];
        for opts in &options {
            combos = combos.into_iter().flat_map(|c| opts.iter().map(move |o| [c.clone(), vec![o.clone()]].concat())).collect();
        }
        combos
            .into_iter()
            .map(|combo| Fun {
                name: name.to_owned(),
                params: combo
                    .into_iter()
                    .zip(&names)
                    .take_while(|((_, m), _)| *m != Mode::Omitted)
                    .map(|((t, m), n)| (n.clone(), t, m))
                    .collect(),
                ret: ret.clone(),
            })
            .collect()
    }

    // ----- interfaces and classes -----

    /// The Kotlin name of an interface or class, generated on first use.
    fn name_for(&mut self, decls: &[DeclId]) -> Option<String> {
        let key = *decls.iter().min()?;
        if let Some(fq) = self.names.get(&key) {
            return Some(fq.clone());
        }
        let fq = self.allocate(key)?;
        self.queue.push((decls.to_vec(), fq.clone()));
        Some(fq)
    }

    /// The Kotlin name of an interface that only extends `target` (`interface Router extends IRouter {}`):
    /// a type alias, which keeps the TypeScript name.
    fn alias_for(&mut self, decls: &[DeclId], target: &str) -> Option<String> {
        let key = *decls.iter().min()?;
        if let Some(fq) = self.names.get(&key) {
            return Some(fq.clone());
        }
        let fq = self.allocate(key)?;
        if fq == target {
            return Some(fq);
        }
        let (pkg, simple) = fq.rsplit_once('.').unwrap_or_default();
        let text = format!("typealias {simple} = {target}");
        self.out.packages.entry(pkg.to_owned()).or_default().types.push(text);
        Some(fq)
    }

    /// A unique Kotlin name for the declaration `key`, in the Kotlin package of its npm package.
    fn allocate(&mut self, key: DeclId) -> Option<String> {
        if self.names.len() >= MAX_TYPES {
            return None;
        }
        let node = &self.prog.decls[key];
        let file = self.prog.scopes[node.scope].file.clone();
        let npm = package_of(&file).unwrap_or_else(|| "npm".to_owned());
        let pkg = kotlin_package(&npm);
        let mut parts = self.prog.namespaces(key);
        if parts.first().is_some_and(|ns| self.prog.assigned_name(node.scope).as_ref() == Some(ns)) {
            parts.remove(0);
        }
        parts.push(node.decl.name.clone());
        let base: String =
            parts.iter().map(|p| identifier(p).map(|s| s.trim_matches('`').to_owned()).unwrap_or_else(|| "Type".into())).collect();
        let base = if base == "default" { "Default".to_owned() } else { base };
        let mut simple = base.clone();
        let mut n = 2;
        while !self.taken.insert((pkg.clone(), simple.clone())) {
            simple = format!("{base}{n}");
            n += 1;
        }
        let fq = format!("{pkg}.{}", backtick(&simple));
        self.names.insert(key, fq.clone());
        self.out.packages.entry(pkg).or_default().npm = npm;
        Some(fq)
    }

    fn emit_type(&mut self, decls: &[DeclId], fq: &str) {
        let this = Some(fq.to_owned());
        let mut members = Members::default();
        let mut visited = HashSet::new();
        for &d in decls {
            let b = self.bindings(d, &[]);
            self.collect(d, &b, &this, &mut members, &mut visited, 0);
        }
        let (pkg, simple) = fq.rsplit_once('.').map(|(p, s)| (p.to_owned(), s.to_owned())).unwrap_or_default();
        let key = *decls.iter().min().unwrap();
        let doc = decls.iter().find_map(|&d| self.prog.decls[d].decl.doc.clone());
        let class = self.classes.get(&key).cloned();
        let mut text = kdoc(&doc, "");
        match &class {
            Some(ClassImport::Default(npm)) => {
                let _ = writeln!(text, "@JsModule(\"{npm}\")");
                let _ = writeln!(text, "external class {simple} : JsAny {{");
            }
            Some(ClassImport::Named) => {
                let _ = writeln!(text, "external class {simple} : JsAny {{");
            }
            None => {
                let _ = writeln!(text, "external interface {simple} : JsAny {{");
            }
        }
        if class.is_some() {
            for ctor in &members.ctors {
                let _ = writeln!(text, "    constructor({})", ctor.render_params());
            }
        }
        for (name, ty, readonly, doc) in &members.props {
            let _ = write!(text, "{}", kdoc(doc, "    "));
            let _ = writeln!(text, "    {} {name}: {}", if *readonly { "val" } else { "var" }, ty.render());
        }
        for (fun, doc) in &members.funs {
            let _ = write!(text, "{}", kdoc(doc, "    "));
            let _ = writeln!(text, "    fun {}({}): {}", fun.name, fun.render_params(), fun.ret.render());
        }
        text.push('}');
        let package = self.out.packages.entry(pkg.clone()).or_default();
        match class {
            Some(ClassImport::Default(_)) => package.default.push(text),
            Some(ClassImport::Named) => package.exports.push(text),
            None => package.types.push(text),
        }
        if let Some(value) = members.index {
            // `obj["key"]` for a string index signature, as an extension in the package users import.
            let base = format!("{}IndexGet", simple.trim_matches('`'));
            let mut helper = base.clone();
            let mut n = 2;
            while !self.taken.insert((self.root.clone(), helper.clone())) {
                helper = format!("{base}{n}");
                n += 1;
            }
            let value = value.nullable().render();
            let text = format!(
                "@Suppress(\"UNUSED_PARAMETER\")\nprivate fun {helper}(obj: {fq}, key: String): {value} = js(\"obj[key]\")\n\n\
                 operator fun {fq}.get(key: String): {value} = {helper}(this, key)"
            );
            self.out.packages.entry(self.root.clone()).or_default().types.push(text);
        }
    }

    /// Adds the members of an interface or class `d` and its supertypes, its type parameters bound by `b`.
    fn collect(
        &mut self,
        d: DeclId,
        b: &Bindings,
        this: &Option<String>,
        members: &mut Members,
        visited: &mut HashSet<DeclId>,
        depth: usize,
    ) {
        if depth > 12 || !visited.insert(d) {
            return;
        }
        let DeclKind::Interface(iface) = self.prog.decls[d].decl.kind.clone() else { return };
        let scope = self.prog.decls[d].scope;
        let env = self.own_env(d);
        for m in iface.members.iter().filter(|m| !m.is_static) {
            match &m.kind {
                MemberKind::Prop { ty, optional, readonly } => {
                    let Some(name) = member_name(&m.name) else { continue };
                    let r = subst(&self.rt(ty, scope, &env), b);
                    let sigs = self.callables(&r);
                    if sigs.is_empty() {
                        if members.props.iter().all(|(n, ..)| *n != name) {
                            let k = self.kt(&r, Pos::Prop, this);
                            let k = if *optional { k.nullable() } else { k };
                            members.props.push((name, k, *readonly, m.doc.clone()));
                        }
                    } else {
                        for sig in sigs {
                            for fun in self.variants(&name, &sig, this) {
                                members.add_fun(fun, m.doc.clone());
                            }
                        }
                    }
                }
                MemberKind::Method { sig } => {
                    let Some(name) = member_name(&m.name) else { continue };
                    let s = subst_sig(&self.rsig(sig, scope, &env), b);
                    for fun in self.variants(&name, &s, this) {
                        members.add_fun(fun, m.doc.clone());
                    }
                }
                MemberKind::Ctor(sig) if iface.is_class && members.ctor_depth.is_none_or(|found| found == depth) => {
                    members.ctor_depth = Some(depth);
                    let s = subst_sig(&self.rsig(sig, scope, &env), b);
                    for fun in self.variants("constructor", &s, this) {
                        if members.ctors.iter().all(|c| c.key() != fun.key()) {
                            members.ctors.push(fun);
                        }
                    }
                }
                MemberKind::Index(ty) if members.index.is_none() => {
                    let r = subst(&self.rt(ty, scope, &env), b);
                    members.index = Some(self.kt(&r, Pos::Prop, this));
                }
                _ => {}
            }
        }
        for ext in &iface.extends {
            let r = subst(&self.rt(ext, scope, &env), b);
            if let RT::Ent(ent, args) = r {
                for parent in self.interfaces(&ent) {
                    let pb = self.bindings(parent, &args);
                    self.collect(parent, &pb, this, members, visited, depth + 1);
                }
            }
        }
    }
}

/// Built-in types that aren't declared in any package.
fn builtin(name: &str, mut args: Vec<RT>) -> RT {
    let first = |args: &mut Vec<RT>| if args.is_empty() { RT::Any } else { args.remove(0) };
    match name {
        "Array" | "ReadonlyArray" | "Set" | "ReadonlySet" | "Iterable" | "ArrayLike" => RT::Array(Box::new(first(&mut args))),
        "Promise" | "PromiseLike" => RT::Promise(Box::new(first(&mut args))),
        "Partial" | "Required" | "Readonly" | "NonNullable" | "Awaited" => first(&mut args),
        "String" => RT::Str,
        "Number" => RT::Num,
        "Boolean" => RT::Bool,
        _ => RT::Opaque,
    }
}

fn subst(rt: &RT, b: &Bindings) -> RT {
    if b.is_empty() {
        return rt.clone();
    }
    match rt {
        RT::Param(n) => b.get(n).cloned().unwrap_or(RT::Any),
        RT::Ent(e, args) => RT::Ent(e.clone(), args.iter().map(|a| subst(a, b)).collect()),
        RT::Array(e) => RT::Array(Box::new(subst(e, b))),
        RT::Promise(e) => RT::Promise(Box::new(subst(e, b))),
        RT::Union(ts) => RT::Union(ts.iter().map(|t| subst(t, b)).collect()),
        RT::Inter(ts) => RT::Inter(ts.iter().map(|t| subst(t, b)).collect()),
        RT::Fn(sig) => RT::Fn(Box::new(subst_sig(sig, b))),
        other => other.clone(),
    }
}

fn subst_sig(sig: &RSig, b: &Bindings) -> RSig {
    RSig { params: sig.params.iter().map(|p| RParam { ty: subst(&p.ty, b), ..p.clone() }).collect(), ret: subst(&sig.ret, b) }
}

const KEYWORDS: [&str; 28] = [
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn backtick(s: &str) -> String {
    if KEYWORDS.contains(&s) { format!("`{s}`") } else { s.to_owned() }
}

/// A JS name as a Kotlin identifier (backquoted if it is a keyword), or `None` if Kotlin can't name it.
fn identifier(s: &str) -> Option<String> {
    is_identifier(s).then(|| backtick(s))
}

/// A member's Kotlin name; the members of `Any` can't be redeclared by an external interface.
fn member_name(s: &str) -> Option<String> {
    if matches!(s, "toString" | "equals" | "hashCode") {
        return None;
    }
    identifier(s)
}

/// Kotlin parameter names: valid, not keywords, and distinct.
fn param_names(params: &[RParam]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let mut name = if is_identifier(&p.name) { p.name.clone() } else { format!("p{i}") };
        if KEYWORDS.contains(&name.as_str()) {
            name.push('_');
        }
        while out.contains(&name) {
            name.push('_');
        }
        out.push(name);
    }
    out
}

/// The Kotlin package of an npm package: `@scope/my-lib` is `scope.my_lib`.
pub fn kotlin_package(npm: &str) -> String {
    npm.trim_start_matches('@')
        .split('/')
        .map(|part| {
            let mut s: String = part.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
            if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
                s.insert(0, '_');
            }
            if KEYWORDS.contains(&s.as_str()) {
                s.push('_');
            }
            s
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The Kotlin name of a package's default export: `left-pad` is `leftPad`.
pub fn function_name(npm: &str) -> String {
    let last = npm.rsplit('/').next().unwrap_or(npm);
    let mut out = String::new();
    let mut upper = false;
    for c in last.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(if upper { c.to_ascii_uppercase() } else { c });
            upper = false;
        } else {
            upper = !out.is_empty();
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    backtick(&out)
}

/// A KDoc comment for a JSDoc one. Comment delimiters in the text are escaped: Kotlin comments nest.
fn kdoc(doc: &Option<String>, indent: &str) -> String {
    let Some(doc) = doc.as_deref().filter(|d| !d.trim().is_empty()) else { return String::new() };
    let mut out = format!("{indent}/**\n");
    for line in doc.replace("*/", "*&#47;").replace("/*", "/&#42;").lines() {
        if line.is_empty() {
            let _ = writeln!(out, "{indent} *");
        } else {
            let _ = writeln!(out, "{indent} * {line}");
        }
    }
    let _ = writeln!(out, "{indent} */");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(kotlin_package("@acme/my-lib"), "acme.my_lib");
        assert_eq!(kotlin_package("express-serve-static-core"), "express_serve_static_core");
        assert_eq!(kotlin_package("in"), "in_");
        assert_eq!(function_name("left-pad"), "leftPad");
        assert_eq!(function_name("@acme/3d"), "_3d");
        assert_eq!(identifier("in").as_deref(), Some("`in`"));
        assert_eq!(identifier("m-search"), None);
    }

    #[test]
    fn rendering() {
        let f = KT::Fun(vec![KT::Named("a.B".into()), KT::AnyQ], Box::new(KT::Unit));
        assert_eq!(f.render(), "(a.B, JsAny?) -> Unit");
        assert_eq!(f.clone().nullable().render(), "((a.B, JsAny?) -> Unit)?");
        assert_eq!(KT::Array(Box::new(KT::Str.element())).render(), "JsArray<JsString>");
        assert_eq!(f.erased(), "Function2");
    }
}
