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
//! - An object type literal (`opts: { a: number }`) becomes an interface named after where it is
//!   (`DoSomethingOpts`). An interface whose members are all properties also gets a factory, a
//!   function of the same name with one parameter per property (`Options(a = 1)`), imported from a
//!   JavaScript module generated next to the declarations ([`factory_module`]): it builds the
//!   object, leaving out the optional properties the call leaves out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::parser::{Decl, DeclKind, Interface, Member, MemberKind, Sig, Type};
use super::program::{DeclId, DeclNode, Ent, Program, ScopeId, package_of};

/// The directory below the project's JavaScript modules (`js/` next to the Kotlin module) that the
/// generated factory modules land in.
pub const FACTORIES_DIR: &str = "kargo-factories";

/// The JavaScript module of the factories of the Kotlin package `pkg`, as Kotlin imports it.
pub fn factory_module(pkg: &str) -> String {
    format!("./{}/{FACTORIES_DIR}/{pkg}.mjs", crate::js_sources::OUTPUT_DIR)
}

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
    /// Every property as JavaScript names it, for a factory: its name, type and whether it is optional.
    fields: Vec<(String, RT, bool)>,
    /// Whether there are members other than properties (methods, call, construct or index
    /// signatures), so that the object is not plain data and gets no factory.
    behavior: bool,
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
    /// A named export of the JavaScript module `.1`, generated into the Kotlin package `.0`.
    Named(String, String),
}

/// The generated Kotlin, by Kotlin package: its declarations and the JS module of its exports.
#[derive(Default)]
pub struct Output {
    pub packages: BTreeMap<String, KotlinPackage>,
}

#[derive(Default)]
pub struct KotlinPackage {
    /// The JavaScript module the Kotlin package is for: an npm package, or `./js/<file>.js`.
    pub npm: String,
    /// Interfaces, type aliases and extensions.
    pub types: Vec<String>,
    /// Functions, values and classes imported by name from `npm` (`@file:JsModule`).
    pub exports: Vec<String>,
    /// The module's default export (`@JsModule` on each declaration).
    pub default: Vec<String>,
    /// Factories of plain-data interfaces, imported by name from [`factory_module`] (`@file:JsModule`).
    pub factories: Vec<String>,
    /// The JavaScript functions of [`factory_module`], one per interface.
    pub factory_js: Vec<String>,
}

/// A factory, kept until the functions it must not clash with are all known.
struct Factory {
    pkg: String,
    funs: Vec<Fun>,
    js: String,
}

pub struct Generator<'a> {
    prog: &'a mut Program,
    out: Output,
    /// Kotlin names of interfaces and classes, by their first declaration.
    names: HashMap<DeclId, String>,
    taken: HashSet<(String, String)>,
    /// Names a variable holds, so an interface of the same name gets an `I` prefix.
    shadowed: HashSet<(String, String)>,
    queue: Vec<(Vec<DeclId>, String)>,
    classes: HashMap<DeclId, ClassImport>,
    /// The Kotlin package of the npm package being generated, where extensions go: code that
    /// imports from it (`import express.*`) gets them without importing the packages it builds on.
    root: String,
    /// The project's own modules: the Kotlin package and the JavaScript module of each file, whose
    /// types go in that package instead of one named after an npm package.
    locals: HashMap<PathBuf, (String, String)>,
    depth: usize,
    /// Where the type being resolved is (`["doSomething", "opts"]`): an object type literal is named after it.
    hint: Vec<String>,
    /// The interfaces made for object type literals, by their scope and members.
    literals: HashMap<(ScopeId, String), DeclId>,
    factories: Vec<Factory>,
    /// The functions generated in each Kotlin package (by [`Fun::key`]), which a factory must not clash with.
    values: HashSet<(String, String)>,
}

impl<'a> Generator<'a> {
    pub fn new(prog: &'a mut Program) -> Self {
        Generator {
            prog,
            out: Output::default(),
            names: HashMap::new(),
            taken: HashSet::new(),
            shadowed: HashSet::new(),
            queue: Vec::new(),
            classes: HashMap::new(),
            root: String::new(),
            locals: HashMap::new(),
            depth: 0,
            hint: Vec::new(),
            literals: HashMap::new(),
            factories: Vec::new(),
            values: HashSet::new(),
        }
    }

    /// Runs `f` with the naming hint `hint` in place of the current one.
    fn hinted<T>(&mut self, hint: Vec<String>, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::replace(&mut self.hint, hint);
        let out = f(self);
        self.hint = saved;
        out
    }

    /// Runs `f` with `part` added to the naming hint.
    fn hinted_in<T>(&mut self, part: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        self.hint.push(part.to_owned());
        let out = f(self);
        self.hint.pop();
        out
    }

    /// The interface for the object type literal `members` in `scope`, named after the current hint.
    /// A literal without properties, methods or signatures (`{}`) is any object.
    fn literal(&mut self, members: &[Member], scope: ScopeId) -> RT {
        if members.iter().all(|m| matches!(m.kind, MemberKind::Ctor(_))) {
            return RT::Opaque;
        }
        let key = (scope, format!("{members:?}"));
        let id = match self.literals.get(&key) {
            Some(&id) => id,
            None => {
                let name: String = self.hint.iter().map(|p| pascal(p)).collect();
                let name = if name.is_empty() { "Object".to_owned() } else { name };
                let iface = Interface { tparams: Vec::new(), extends: Vec::new(), members: members.to_vec(), is_class: false };
                let decl = Decl { name, kind: DeclKind::Interface(iface), doc: None, exported: false, default: false };
                self.prog.decls.push(DeclNode { decl, scope, body: None });
                let id = self.prog.decls.len() - 1;
                self.literals.insert(key, id);
                id
            }
        };
        RT::Ent(Ent::Decls(vec![id]), Vec::new())
    }

    /// Declares the file `path` one of the project's modules, imported as `spec` and generated into
    /// the Kotlin package `pkg`. Call it for every such file before generating any of them. Once
    /// there are such files, types of other files (npm packages) get no declarations.
    pub fn local(&mut self, path: &Path, pkg: &str, spec: &str) {
        self.locals.insert(super::program::normalize(path), (pkg.to_owned(), spec.to_owned()));
    }

    /// Declarations for the npm package `npm`, whose module is `module`.
    pub fn package(&mut self, npm: &str, module: ScopeId) {
        self.module(&kotlin_package(npm), npm, &function_name(npm), module);
    }

    /// Declarations in the Kotlin package `pkg` for the JavaScript module imported as `npm`, whose
    /// declarations are `module`; its default export is named `default_name`.
    pub fn module(&mut self, pkg: &str, npm: &str, default_name: &str, module: ScopeId) {
        // Classes first, so that references to them use the class.
        self.classes(pkg, npm, module);
        let pkg = pkg.to_owned();
        self.root = pkg.clone();
        self.out.packages.entry(pkg.clone()).or_default().npm = npm.to_owned();
        let default = self.prog.default_export(module);
        let names = self.prog.export_names(module);
        let exports: Vec<(String, Option<Ent>)> = names.into_iter().map(|n| (n.clone(), self.prog.export(module, &n))).collect();

        // A name exported as both a variable and an interface (`const ColorManagement` next to
        // `interface ColorManagement`) cannot be both in one Kotlin package: the value keeps the
        // name and the interface gets a `I`-prefixed one.
        for (name, ent) in &exports {
            let Some(Ent::Decls(decls)) = ent else { continue };
            let has = |f: fn(&DeclKind) -> bool| decls.iter().any(|&d| f(&self.prog.decls[d].decl.kind));
            if has(|k| matches!(k, DeclKind::Var(_))) && has(|k| matches!(k, DeclKind::Interface(i) if !i.is_class)) {
                if let Some(simple) = identifier(name) {
                    let key = (pkg.clone(), simple.trim_matches('`').to_owned());
                    self.taken.insert(key.clone());
                    self.shadowed.insert(key);
                }
            }
        }

        if let Some(Ent::Decls(decls)) = default {
            for d in decls {
                let texts = self.value(&pkg, d, default_name, &format!("@JsModule(\"{npm}\")\n"));
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
                    let texts = self.value(&pkg, d, &name, "");
                    self.package_mut(&pkg).exports.extend(texts);
                }
            }
            self.export_type(&pkg, &name, &decls);
        }
        self.drain();
    }

    /// Records the classes the JavaScript module `npm` exports, generated into the Kotlin package
    /// `pkg`, so that references to them use the class. [`Generator::module`] does it for its
    /// module; call it for every module first when one module's types refer to another's classes.
    pub fn classes(&mut self, pkg: &str, npm: &str, module: ScopeId) {
        if let Some(Ent::Decls(decls)) = self.prog.default_export(module) {
            for d in decls {
                if self.is_class(d) {
                    self.classes.insert(d, ClassImport::Default(npm.to_owned()));
                }
            }
        }
        for name in self.prog.export_names(module) {
            if let Some(Ent::Decls(decls)) = self.prog.export(module, &name) {
                for d in decls {
                    if self.is_class(d) {
                        self.classes.entry(d).or_insert_with(|| ClassImport::Named(pkg.to_owned(), npm.to_owned()));
                    }
                }
            }
        }
    }

    pub fn finish(mut self) -> Output {
        self.drain();
        for factory in std::mem::take(&mut self.factories) {
            let funs: Vec<&Fun> = factory.funs.iter().filter(|f| !self.values.contains(&(factory.pkg.clone(), f.key()))).collect();
            if funs.is_empty() {
                continue;
            }
            let package = self.out.packages.entry(factory.pkg.clone()).or_default();
            for fun in funs {
                package.factories.push(format!("external fun {}({}): {}", fun.name, fun.render_params(), fun.ret.render()));
            }
            package.factory_js.push(factory.js);
        }
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

    /// A function or variable declaration as top-level Kotlin declarations named `name` in the
    /// Kotlin package `pkg`, each with `annotation` after its documentation.
    fn value(&mut self, pkg: &str, d: DeclId, name: &str, annotation: &str) -> Vec<String> {
        let Some(kname) = identifier(name) else { return Vec::new() };
        self.hinted(vec![name.to_owned()], |g| g.value_in(pkg, d, &kname, annotation))
    }

    fn value_in(&mut self, pkg: &str, d: DeclId, kname: &str, annotation: &str) -> Vec<String> {
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
            for fun in self.variants(kname, &sig, &None) {
                self.values.insert((pkg.to_owned(), fun.key()));
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
            Type::Object | Type::BigInt | Type::Symbol | Type::Ctor(_) => RT::Opaque,
            Type::Obj(members) => self.literal(members, scope),
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
            .map(|p| {
                let ty = self.hinted_in(&p.name, |g| g.rt(&p.ty, scope, &env));
                RParam { name: p.name.clone(), ty, optional: p.optional, rest: p.rest }
            })
            .collect();
        let ret = self.hinted_in("Result", |g| g.rt(&sig.ret, scope, &env));
        RSig { params, ret }
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
        let name = self.prog.decls[d].decl.name.clone();
        let body = self.hinted(vec![name], |g| g.rt(&ty, scope, &env));
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
                        let name = self.prog.decls[d].decl.name.clone();
                        out.push(self.hinted(vec![name], |g| g.rsig(&sig, scope, &Bindings::new())));
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
        let name = self.prog.decls[d].decl.name.clone();
        let mut out = Vec::new();
        for m in &iface.members {
            if let MemberKind::Call(sig) = &m.kind {
                let s = self.hinted(vec![name.clone()], |g| g.rsig(sig, scope, &env));
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
            // One overload: the first option of an optional parameter is "left out", which would drop every
            // parameter, so it keeps its type with a default. `Double` takes the fractions a number can have.
            for (o, p) in options.iter_mut().zip(&sig.params) {
                let kept = o.iter().filter(|(_, m)| *m != Mode::Omitted);
                let (ty, mode) =
                    kept.clone().find(|(k, _)| *k == KT::Num).or_else(|| kept.clone().next()).cloned().unwrap_or_else(|| o[0].clone());
                *o = vec![(ty, if p.optional && !p.rest { Mode::Optional } else { mode })];
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
        let (pkg, npm) = match self.locals.get(&file) {
            Some((pkg, spec)) => (pkg.clone(), spec.clone()),
            // The declarations of the project's own modules leave npm packages to theirs: their types are `JsAny`.
            None if !self.locals.is_empty() => return None,
            None => {
                let npm = package_of(&file).unwrap_or_else(|| "npm".to_owned());
                match self.classes.get(&key) {
                    // A class exported by a subpath of its package (`cubing/alg`) goes with that
                    // subpath's exports, so that it is imported from the subpath.
                    Some(ClassImport::Named(pkg, spec)) if spec.strip_prefix(npm.as_str()).is_some_and(|s| s.starts_with('/')) => {
                        (pkg.clone(), spec.clone())
                    }
                    _ => (kotlin_package(&npm), npm),
                }
            }
        };
        let mut parts = self.prog.namespaces(key);
        if parts.first().is_some_and(|ns| self.prog.assigned_name(node.scope).as_ref() == Some(ns)) {
            parts.remove(0);
        }
        parts.push(node.decl.name.clone());
        let base: String =
            parts.iter().map(|p| identifier(p).map(|s| s.trim_matches('`').to_owned()).unwrap_or_else(|| "Type".into())).collect();
        let base = if base == "default" { "Default".to_owned() } else { base };
        let base = if self.shadowed.contains(&(pkg.clone(), base.clone())) { format!("I{base}") } else { base };
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
        let (pkg, simple) = fq.rsplit_once('.').map(|(p, s)| (p.to_owned(), s.to_owned())).unwrap_or_default();
        self.hinted(vec![simple.trim_matches('`').to_owned()], |g| {
            for &d in decls {
                let b = g.bindings(d, &[]);
                g.collect(d, &b, &this, &mut members, &mut visited, 0);
            }
        });
        let key = *decls.iter().min().unwrap();
        let doc = decls.iter().find_map(|&d| self.prog.decls[d].decl.doc.clone());
        let class = self.classes.get(&key).cloned();
        let mut text = kdoc(&doc, "");
        match &class {
            Some(ClassImport::Default(npm)) => {
                let _ = writeln!(text, "@JsModule(\"{npm}\")");
                let _ = writeln!(text, "external class {simple} : JsAny {{");
            }
            Some(ClassImport::Named(..)) => {
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
        if class.is_none() {
            self.factory(&pkg, &simple, &this, &members);
        }
        let package = self.out.packages.entry(pkg.clone()).or_default();
        match class {
            Some(ClassImport::Default(_)) => package.default.push(text),
            Some(ClassImport::Named(..)) => package.exports.push(text),
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

    /// The factory of the interface `simple` of the Kotlin package `pkg` with `members`, when they
    /// are all properties: a Kotlin function per combination of their types (required ones first,
    /// optional ones defaulting to `definedExternally`), and the JavaScript function it calls, which
    /// sets a property only when its argument is not `undefined`.
    fn factory(&mut self, pkg: &str, simple: &str, this: &Option<String>, members: &Members) {
        if members.behavior || members.fields.is_empty() || simple.starts_with('`') || JS_RESERVED.contains(&simple) {
            return;
        }
        let mut fields = members.fields.clone();
        // Leaving out an optional parameter leaves out those after it, never a required one.
        fields.sort_by_key(|(_, _, optional)| *optional);
        let params =
            fields.iter().map(|(name, ty, optional)| RParam { name: name.clone(), ty: ty.clone(), optional: *optional, rest: false });
        let sig = RSig { params: params.collect(), ret: RT::This };
        let funs = self.variants(simple, &sig, this);
        let mut names: Vec<String> = Vec::new();
        for name in param_names(&sig.params) {
            let mut name = if JS_RESERVED.contains(&name.as_str()) { format!("{name}_") } else { name };
            while names.contains(&name) {
                name.push('_');
            }
            names.push(name);
        }
        let mut js = format!("export function {simple}({}) {{\n    const $o = {{}};\n", names.join(", "));
        for ((name, _, optional), param) in fields.iter().zip(&names) {
            let key = serde_json::to_string(name).expect("a string serializes");
            if *optional {
                let _ = writeln!(js, "    if ({param} !== undefined) $o[{key}] = {param};");
            } else {
                let _ = writeln!(js, "    $o[{key}] = {param};");
            }
        }
        js.push_str("    return $o;\n}");
        self.factories.push(Factory { pkg: pkg.to_owned(), funs, js });
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
                    let r = subst(&self.hinted_in(&m.name, |g| g.rt(ty, scope, &env)), b);
                    if members.fields.iter().all(|(n, ..)| *n != m.name) {
                        members.fields.push((m.name.clone(), r.clone(), *optional));
                    }
                    let Some(name) = member_name(&m.name) else { continue };
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
                    members.behavior = true;
                    let Some(name) = member_name(&m.name) else { continue };
                    let s = subst_sig(&self.hinted_in(&m.name, |g| g.rsig(sig, scope, &env)), b);
                    for fun in self.variants(&name, &s, this) {
                        members.add_fun(fun, m.doc.clone());
                    }
                }
                MemberKind::Ctor(sig) if iface.is_class && members.ctor_depth.is_none_or(|found| found == depth) => {
                    members.behavior = true;
                    members.ctor_depth = Some(depth);
                    let s = subst_sig(&self.rsig(sig, scope, &env), b);
                    for fun in self.variants("constructor", &s, this) {
                        if members.ctors.iter().all(|c| c.key() != fun.key()) {
                            members.ctors.push(fun);
                        }
                    }
                }
                MemberKind::Index(ty) if members.index.is_none() => {
                    members.behavior = true;
                    let r = subst(&self.rt(ty, scope, &env), b);
                    members.index = Some(self.kt(&r, Pos::Prop, this));
                }
                _ => members.behavior = true,
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

/// JavaScript's reserved words that Kotlin takes as names: a factory never uses them as its own
/// name or its parameters'.
const JS_RESERVED: [&str; 28] = [
    "arguments",
    "await",
    "case",
    "catch",
    "const",
    "debugger",
    "default",
    "delete",
    "enum",
    "eval",
    "export",
    "extends",
    "finally",
    "function",
    "implements",
    "import",
    "instanceof",
    "let",
    "new",
    "private",
    "protected",
    "public",
    "static",
    "switch",
    "undefined",
    "void",
    "with",
    "yield",
];

/// A part of a name in PascalCase, without the characters an identifier can't have (`on-load` is `OnLoad`).
fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(if upper { c.to_ascii_uppercase() } else { c });
            upper = false;
        } else {
            upper = true;
        }
    }
    out
}

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
