//! Libraries without sources: their declarations, read from compiled code and written out as
//! stub source files, which [`super::library`] then reads like a sources jar.
//!
//! - In a jar, a class compiled from Kotlin carries its Kotlin declarations in its
//!   `kotlin.Metadata` annotation (see [`super::kmetadata`]): extension functions, nullability,
//!   `suspend`, default values and type aliases come out as they were written. Any other class
//!   is read as Java from the class file itself (see [`super::classfile`]).
//! - A klib (Wasm and Native dependencies, the Kotlin/Native platform libraries) carries the
//!   same metadata in `default/linkdata`, whether it is a zip file or an unpacked directory.
//!
//! Each package becomes one Kotlin file (and one Java file for its Java classes) with only
//! what code outside the library can use: public and protected declarations, with no bodies
//! and `...` for default values.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;

use super::classfile::{self, ClassFile, JavaType};
use super::kmetadata::{self as km, ClassKind, Modality, Names, Visibility};

/// A generated file: its path relative to the stub directory, and its text.
pub type Stub = (PathBuf, String);

// ----- names and imports -----

/// The imports of one generated file: a class is written by its simple name when that name
/// is free, and qualified when another class of the same name took it.
struct Imports {
    package: String,
    taken: HashMap<String, String>,
    imports: BTreeSet<String>,
    /// Packages the file sees without importing them.
    implicit: &'static [&'static str],
}

/// The packages every Kotlin file imports.
const KOTLIN_IMPLICIT: &[&str] = &[
    "kotlin",
    "kotlin.annotation",
    "kotlin.collections",
    "kotlin.comparisons",
    "kotlin.io",
    "kotlin.ranges",
    "kotlin.sequences",
    "kotlin.text",
];

impl Imports {
    /// `own` are the top-level classes of `package` declared in the file.
    fn new(package: &str, own: impl IntoIterator<Item = String>, implicit: &'static [&'static str]) -> Self {
        let taken = own.into_iter().map(|n| (n.clone(), format!("{package}.{n}"))).collect();
        Imports { package: package.to_owned(), taken, imports: BTreeSet::new(), implicit }
    }

    /// The name to write for the class `rel` (`Outer.Inner`) of `package`.
    fn name(&mut self, package: &str, rel: &str) -> String {
        let top = rel.split('.').next().unwrap_or(rel);
        let fq_top = if package.is_empty() { top.to_owned() } else { format!("{package}.{top}") };
        if package == self.package {
            if self.taken.get(top).is_none_or(|t| *t == fq_top) {
                self.taken.insert(top.to_owned(), fq_top);
                return rel.to_owned();
            }
        } else if self.taken.get(top).is_none_or(|t| *t == fq_top) && !package.is_empty() {
            if !self.implicit.contains(&package) {
                self.imports.insert(fq_top.clone());
            }
            self.taken.insert(top.to_owned(), fq_top);
            return rel.to_owned();
        }
        if package.is_empty() { rel.to_owned() } else { format!("{package}.{rel}") }
    }

    /// The file's header: the package and the imports, each line ended with `end` (`;` for Java).
    fn header(&self, comment: &str, end: &str) -> String {
        let mut out = format!("// {comment}\n");
        if !self.package.is_empty() {
            out.push_str(&format!("package {}{end}\n", escape_path(&self.package)));
        }
        out.push('\n');
        for import in &self.imports {
            out.push_str(&format!("import {}{end}\n", escape_path(import)));
        }
        if !self.imports.is_empty() {
            out.push('\n');
        }
        out
    }
}

/// Splits a class id (`kotlin/collections/Map.Entry`) into its package (`kotlin.collections`)
/// and its relative name (`Map.Entry`).
fn split_class_id(id: &str) -> (String, &str) {
    match id.rfind('/') {
        Some(i) => (id[..i].replace('/', "."), &id[i + 1..]),
        None => (String::new(), id),
    }
}

const KOTLIN_KEYWORDS: &[&str] = &[
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

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// A Kotlin name, in backticks when it needs them; `None` for a name no source can declare.
fn kotlin_name(name: &str) -> Option<String> {
    if name.is_empty() || name.starts_with('<') || name.contains(['`', '\n', '.', ';', '[', ']', '/', '<', '>', ':', '\\']) {
        return None;
    }
    Some(if is_identifier(name) && !KOTLIN_KEYWORDS.contains(&name) { name.to_owned() } else { format!("`{name}`") })
}

fn escape_path(path: &str) -> String {
    path.split('.').map(|p| kotlin_name(p).unwrap_or_else(|| p.to_owned())).collect::<Vec<_>>().join(".")
}

// ----- Kotlin -----

/// All classes of a library by class id, with the names their metadata refers to.
type ClassMap = HashMap<String, (km::Class, Arc<Names>)>;

struct KotlinWriter<'a> {
    imports: Imports,
    classes: &'a ClassMap,
    /// Type parameters in scope: id and name.
    type_params: Vec<(i32, String)>,
    out: String,
}

impl KotlinWriter<'_> {
    fn ty(&mut self, t: &km::Type, names: &Names) -> String {
        let mut out = self.ty_inner(t, names);
        if t.nullable && !out.ends_with('?') {
            if out.contains("->") {
                out = format!("({out})");
            }
            out.push('?');
        }
        out
    }

    fn ty_inner(&mut self, t: &km::Type, names: &Names) -> String {
        if let Some(id) = t.type_param {
            return self.type_params.iter().rev().find(|(i, _)| *i == id).map_or_else(|| "Any?".to_owned(), |(_, n)| n.clone());
        }
        if let Some(name) = t.type_param_name {
            return kotlin_name(names.string(name)).unwrap_or_else(|| "Any?".into());
        }
        let Some(id) = t.class.or(t.alias_name).and_then(|c| names.class(c)) else { return "Any".to_owned() };
        let args: Vec<Option<&km::Type>> = t
            .args
            .iter()
            .map(|a| match a {
                km::TypeArg::Star => None,
                km::TypeArg::In(t) | km::TypeArg::Out(t) | km::TypeArg::Inv(t) => Some(t),
            })
            .collect();
        if let Some(n) = id.strip_prefix("kotlin/Function").and_then(|n| n.parse::<usize>().ok())
            && args.len() == n + 1
        {
            let mut args = args;
            let mut ret = args.pop().flatten();
            // A suspend function type is kept the way the JVM sees it: its last parameter is a
            // `Continuation` of the real result, and it returns `Any?`.
            if t.suspend
                && let Some(Some(last)) = args.last()
                && last.class.and_then(|c| names.class(c)) == Some("kotlin/coroutines/Continuation")
            {
                ret = last.args.first().and_then(|a| match a {
                    km::TypeArg::Star => None,
                    km::TypeArg::In(t) | km::TypeArg::Out(t) | km::TypeArg::Inv(t) => Some(t),
                });
                args.pop();
            }
            let ret = ret.map_or_else(|| "Any?".to_owned(), |t| self.ty(t, names));
            let mut parts: Vec<String> = args.iter().map(|a| a.map_or_else(|| "Any?".to_owned(), |t| self.ty(t, names))).collect();
            let receiver = if t.extension && !parts.is_empty() { Some(parts.remove(0)) } else { None };
            let receiver = receiver.map(|r| if r.contains("->") { format!("({r})") } else { r });
            let suspend = if t.suspend { "suspend " } else { "" };
            return match receiver {
                Some(r) => format!("{suspend}{r}.({}) -> {ret}", parts.join(", ")),
                None => format!("{suspend}({}) -> {ret}", parts.join(", ")),
            };
        }
        let (package, rel) = split_class_id(id);
        let rel = rel.split('.').map(|p| kotlin_name(p).unwrap_or_else(|| p.to_owned())).collect::<Vec<_>>().join(".");
        let mut out = self.imports.name(&package, &rel);
        if !t.args.is_empty() {
            let args: Vec<String> = t
                .args
                .iter()
                .map(|a| match a {
                    km::TypeArg::Star => "*".to_owned(),
                    km::TypeArg::In(t) => format!("in {}", self.ty(t, names)),
                    km::TypeArg::Out(t) => format!("out {}", self.ty(t, names)),
                    km::TypeArg::Inv(t) => self.ty(t, names),
                })
                .collect();
            out.push_str(&format!("<{}>", args.join(", ")));
        }
        out
    }

    /// Declares `params` in scope and writes them: `<in T : Bound, reified U>`.
    fn type_params(&mut self, params: &[km::TypeParam], names: &Names, functions: bool) -> String {
        for p in params {
            let name = kotlin_name(names.string(p.name)).unwrap_or_else(|| format!("T{}", p.id));
            self.type_params.push((p.id, name));
        }
        if params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = params
            .iter()
            .map(|p| {
                let name = self.type_params.iter().rev().find(|(i, _)| *i == p.id).map(|(_, n)| n.clone()).unwrap_or_default();
                let mut s = String::new();
                if p.reified && functions {
                    s.push_str("reified ");
                }
                match p.variance {
                    0 => s.push_str("in "),
                    1 => s.push_str("out "),
                    _ => {}
                }
                s.push_str(&name);
                // A type parameter has one bound in angle brackets; more go in a `where`, which is left out.
                if let Some(bound) = p.bounds.first() {
                    let bound = self.ty(bound, names);
                    if bound != "Any?" {
                        s.push_str(&format!(" : {bound}"));
                    }
                }
                s
            })
            .collect();
        format!("<{}>", parts.join(", "))
    }

    fn params(&mut self, params: &[km::Param], names: &Names) -> String {
        let parts: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut s = String::new();
                if p.vararg.is_some() {
                    s.push_str("vararg ");
                }
                if p.crossinline {
                    s.push_str("crossinline ");
                }
                if p.noinline {
                    s.push_str("noinline ");
                }
                let name = kotlin_name(names.string(p.name)).unwrap_or_else(|| format!("p{i}"));
                let ty = self.ty(p.vararg.as_ref().unwrap_or(&p.ty), names);
                s.push_str(&format!("{name}: {ty}"));
                if p.has_default {
                    s.push_str(" = ...");
                }
                s
            })
            .collect();
        format!("({})", parts.join(", "))
    }

    fn visibility(v: Visibility) -> &'static str {
        if v == Visibility::Protected { "protected" } else { "public" }
    }

    /// A KDoc comment, as a klib may keep it.
    fn doc(&mut self, doc: &Option<String>, indent: &str) {
        let Some(doc) = doc else { return };
        let doc = doc.trim();
        if doc.starts_with("/**") && doc.ends_with("*/") {
            for line in doc.lines() {
                self.out.push_str(&format!("{indent}{}\n", line.trim()));
            }
        } else {
            self.out.push_str(&format!("{indent}/**\n"));
            for line in doc.replace("*/", "*&#47;").lines() {
                self.out.push_str(&format!("{indent} * {}\n", line.trim()));
            }
            self.out.push_str(&format!("{indent} */\n"));
        }
    }

    fn receiver(&mut self, receiver: &Option<km::Type>, names: &Names) -> String {
        match receiver {
            Some(r) => {
                let r = self.ty(r, names);
                if r.contains("->") || r.ends_with('?') && r.starts_with('(') { format!("({r}).") } else { format!("{r}.") }
            }
            None => String::new(),
        }
    }

    fn function(&mut self, f: &km::Function, names: &Names, indent: &str, in_class: bool) {
        if !f.visibility().is_api() || !f.is_declared() || f.hidden {
            return;
        }
        let Some(name) = kotlin_name(names.string(f.name)) else { return };
        let depth = self.type_params.len();
        let mut mods = vec![Self::visibility(f.visibility())];
        if in_class {
            match f.modality() {
                Modality::Open => mods.push("open"),
                Modality::Abstract => mods.push("abstract"),
                _ => {}
            }
        }
        for (on, word) in [
            (f.is_external(), "external"),
            (f.is_tailrec(), "tailrec"),
            (f.is_suspend(), "suspend"),
            (f.is_inline(), "inline"),
            (f.is_infix(), "infix"),
            (f.is_operator(), "operator"),
        ] {
            if on {
                mods.push(word);
            }
        }
        let tps = self.type_params(&f.type_params, names, true);
        let tps = if tps.is_empty() { tps } else { format!("{tps} ") };
        let receiver = self.receiver(&f.receiver, names);
        let params = self.params(&f.params, names);
        let ret = self.ty(&f.ret, names);
        self.doc(&f.doc, indent);
        self.out.push_str(&format!("{indent}{} fun {tps}{receiver}{name}{params}: {ret}\n", mods.join(" ")));
        self.type_params.truncate(depth);
    }

    fn property(&mut self, p: &km::Property, names: &Names, indent: &str, in_class: bool) {
        if !p.visibility().is_api() || !p.is_declared() || p.hidden {
            return;
        }
        let Some(name) = kotlin_name(names.string(p.name)) else { return };
        let depth = self.type_params.len();
        let mut mods = vec![Self::visibility(p.visibility())];
        if in_class {
            match p.modality() {
                Modality::Open => mods.push("open"),
                Modality::Abstract => mods.push("abstract"),
                _ => {}
            }
        }
        if p.is_const() {
            mods.push("const");
        }
        if p.is_lateinit() {
            mods.push("lateinit");
        }
        let keyword = if p.is_var() { "var" } else { "val" };
        let tps = self.type_params(&p.type_params, names, true);
        let tps = if tps.is_empty() { tps } else { format!("{tps} ") };
        let receiver = self.receiver(&p.receiver, names);
        let ty = self.ty(&p.ty, names);
        self.doc(&p.doc, indent);
        self.out.push_str(&format!("{indent}{} {keyword} {tps}{receiver}{name}: {ty}\n", mods.join(" ")));
        self.type_params.truncate(depth);
    }

    fn alias(&mut self, a: &km::TypeAlias, names: &Names, indent: &str) {
        if !a.visibility().is_api() {
            return;
        }
        let Some(name) = kotlin_name(names.string(a.name)) else { return };
        let depth = self.type_params.len();
        let tps = self.type_params(&a.type_params, names, false);
        let ty = self.ty(&a.underlying, names);
        self.out.push_str(&format!("{indent}public typealias {name}{tps} = {ty}\n"));
        self.type_params.truncate(depth);
    }

    fn members(&mut self, members: &km::Package, names: &Names, indent: &str, in_class: bool) {
        for p in &members.properties {
            self.property(p, names, indent, in_class);
        }
        for f in &members.functions {
            self.function(f, names, indent, in_class);
        }
        if !in_class {
            for a in &members.aliases {
                self.alias(a, names, indent);
            }
        }
    }

    fn class(&mut self, c: &km::Class, names: &Names, indent: &str) {
        let kind = c.kind();
        if !c.visibility().is_api() || kind == ClassKind::EnumEntry || c.hidden {
            return;
        }
        let Some(id) = names.class(c.name) else { return };
        let simple = id.rsplit(['.', '/']).next().unwrap_or(id);
        let Some(name) = kotlin_name(simple) else { return };
        let depth = self.type_params.len();
        let mut mods = vec![Self::visibility(c.visibility())];
        let interface = kind == ClassKind::Interface;
        match (kind, c.modality()) {
            (ClassKind::Class, Modality::Open) => mods.push("open"),
            (ClassKind::Class, Modality::Abstract) => mods.push("abstract"),
            (ClassKind::Class | ClassKind::Interface, Modality::Sealed) => mods.push("sealed"),
            _ => {}
        }
        if c.is_data() {
            mods.push("data");
        }
        if c.is_value() {
            mods.push("value");
        }
        if c.is_inner() {
            mods.push("inner");
        }
        if c.is_fun() {
            mods.push("fun");
        }
        let header = match kind {
            ClassKind::Interface => format!("interface {name}"),
            ClassKind::EnumClass => format!("enum class {name}"),
            ClassKind::AnnotationClass => format!("annotation class {name}"),
            ClassKind::Object => format!("object {name}"),
            ClassKind::CompanionObject if simple == "Companion" => "companion object".to_owned(),
            ClassKind::CompanionObject => format!("companion object {name}"),
            _ => format!("class {name}"),
        };
        let mut line = format!("{indent}{} {header}", mods.join(" "));
        if !matches!(kind, ClassKind::Object | ClassKind::CompanionObject) {
            line.push_str(&self.type_params(&c.type_params, names, false));
        }
        let has_constructor = matches!(kind, ClassKind::Class | ClassKind::EnumClass | ClassKind::AnnotationClass);
        if has_constructor && let Some(primary) = c.constructors.iter().find(|k| !k.is_secondary()) {
            if primary.visibility().is_api() {
                if primary.visibility() == Visibility::Protected {
                    line.push_str(" protected constructor");
                }
                line.push_str(&self.params(&primary.params, names));
            } else {
                line.push_str(" private constructor()");
            }
        }
        let skip = ["kotlin/Any", "kotlin/Enum", "kotlin/Annotation"];
        let supertypes: Vec<String> = c
            .supertypes
            .iter()
            .filter(|t| !t.class.and_then(|i| names.class(i)).is_some_and(|n| skip.contains(&n)))
            .map(|t| self.ty(t, names))
            .collect();
        if !supertypes.is_empty() {
            line.push_str(&format!(" : {}", supertypes.join(", ")));
        }
        if indent.is_empty() {
            self.out.push('\n');
        }
        self.doc(&c.doc, indent);
        self.out.push_str(&line);
        self.out.push_str(" {\n");
        let inner = format!("{indent}    ");
        if kind == ClassKind::EnumClass {
            let entries: Vec<String> = c.enum_entries.iter().filter_map(|&e| kotlin_name(names.string(e))).collect();
            self.out.push_str(&format!("{inner}{};\n", entries.join(", ")));
        }
        if has_constructor {
            for k in c.constructors.iter().filter(|k| k.is_secondary() && k.visibility().is_api() && !k.hidden) {
                let params = self.params(&k.params, names);
                self.doc(&k.doc, &inner);
                self.out.push_str(&format!("{inner}{} constructor{params}\n", Self::visibility(k.visibility())));
            }
        }
        self.members(&c.members, names, &inner, !interface);
        for &n in &c.nested {
            let nested_id = format!("{id}.{}", names.string(n));
            if let Some((nested, nested_names)) = self.classes.get(&nested_id) {
                let nested_names = Arc::clone(nested_names);
                self.class(nested, &nested_names, &inner);
            }
        }
        self.out.push_str(&format!("{indent}}}\n"));
        self.type_params.truncate(depth);
    }
}

/// The Kotlin file of one package: its top-level classes and declarations.
fn kotlin_file(package: &str, top: &[&str], members: &[(&km::Package, &Names)], classes: &ClassMap, comment: &str) -> String {
    let mut own: Vec<String> = top.iter().map(|id| split_class_id(id).1.to_owned()).collect();
    own.extend(members.iter().flat_map(|(p, names)| p.aliases.iter().map(|a| names.string(a.name).to_owned())));
    let mut w = KotlinWriter { imports: Imports::new(package, own, KOTLIN_IMPLICIT), classes, type_params: Vec::new(), out: String::new() };
    for (package, names) in members {
        w.members(package, names, "", false);
    }
    for id in top {
        if let Some((class, names)) = classes.get(*id) {
            w.class(class, names, "");
        }
    }
    let mut text = w.imports.header(comment, "");
    text.push_str(&w.out);
    text
}

// ----- Java -----

const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
];

fn java_name(name: &str) -> Option<&str> {
    (is_identifier(name) && !JAVA_KEYWORDS.contains(&name)).then_some(name)
}

/// Splits an internal name (`java/util/Map$Entry`) into its package and its relative name (`Map.Entry`).
fn split_internal(name: &str) -> (String, String) {
    let (package, rel) = match name.rfind('/') {
        Some(i) => (name[..i].replace('/', "."), &name[i + 1..]),
        None => (String::new(), name),
    };
    (package, rel.replace('$', "."))
}

struct JavaWriter<'a> {
    imports: Imports,
    classes: &'a HashMap<String, ClassFile>,
    out: String,
}

impl JavaWriter<'_> {
    fn ty(&mut self, t: &JavaType) -> String {
        match t {
            JavaType::Primitive(p) => (*p).to_owned(),
            JavaType::Var(v) => v.clone(),
            JavaType::Array(inner) => format!("{}[]", self.ty(inner)),
            JavaType::Class(name, args) => {
                let (package, rel) = split_internal(name);
                let mut out = self.imports.name(&package, &rel);
                if !args.is_empty() {
                    let args: Vec<String> = args
                        .iter()
                        .map(|a| match a {
                            classfile::TypeArg::Star => "?".to_owned(),
                            classfile::TypeArg::Extends(t) => format!("? extends {}", self.ty(t)),
                            classfile::TypeArg::Super(t) => format!("? super {}", self.ty(t)),
                            classfile::TypeArg::Exact(t) => self.ty(t),
                        })
                        .collect();
                    out.push_str(&format!("<{}>", args.join(", ")));
                }
                out
            }
        }
    }

    fn type_params(&mut self, params: &[(String, Vec<JavaType>)]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = params
            .iter()
            .map(|(name, bounds)| {
                let bounds: Vec<String> =
                    bounds.iter().filter(|b| !matches!(b, JavaType::Class(n, _) if n == "java/lang/Object")).map(|b| self.ty(b)).collect();
                if bounds.is_empty() { name.clone() } else { format!("{name} extends {}", bounds.join(" & ")) }
            })
            .collect();
        format!("<{}> ", parts.join(", "))
    }

    fn visibility(access: u16) -> Option<&'static str> {
        if access & classfile::ACC_PUBLIC != 0 {
            Some("public")
        } else if access & classfile::ACC_PROTECTED != 0 {
            Some("protected")
        } else {
            None
        }
    }

    /// Writes `class` (with `access` from its `InnerClasses` entry when it is nested).
    fn class(&mut self, class: &ClassFile, access: u16, nested: bool, indent: &str) {
        let Some(visibility) = Self::visibility(access) else { return };
        if access & classfile::ACC_SYNTHETIC != 0 {
            return;
        }
        let simple = class.name.rsplit(['/', '$']).next().unwrap_or(&class.name);
        let Some(name) = java_name(simple) else { return };
        let interface = access & classfile::ACC_INTERFACE != 0;
        let annotation = access & classfile::ACC_ANNOTATION != 0;
        let is_enum = access & classfile::ACC_ENUM != 0;
        let mut mods = vec![visibility];
        if nested && access & classfile::ACC_STATIC != 0 {
            mods.push("static");
        }
        if !interface && access & classfile::ACC_ABSTRACT != 0 {
            mods.push("abstract");
        }
        if !is_enum && access & classfile::ACC_FINAL != 0 {
            mods.push("final");
        }
        let keyword = if annotation {
            "@interface"
        } else if interface {
            "interface"
        } else if is_enum {
            "enum"
        } else {
            "class"
        };
        let sig = class.signature.as_deref().and_then(classfile::class_signature);
        let (type_params, superclass, interfaces) = match &sig {
            Some(sig) if !sig.types.is_empty() => (sig.type_params.clone(), Some(sig.types[0].clone()), sig.types[1..].to_vec()),
            _ => (
                Vec::new(),
                class.super_name.as_ref().map(|s| JavaType::Class(s.clone(), Vec::new())),
                class.interfaces.iter().map(|i| JavaType::Class(i.clone(), Vec::new())).collect(),
            ),
        };
        let tps = self.type_params(&type_params);
        let mut line = format!("{indent}{} {keyword} {name}", mods.join(" "));
        if !tps.is_empty() {
            line.push_str(tps.trim_end());
        }
        let skip_super = ["java/lang/Object", "java/lang/Enum", "java/lang/Record"];
        if let Some(JavaType::Class(s, args)) = &superclass
            && !interface
            && !skip_super.contains(&s.as_str())
        {
            let s = self.ty(&JavaType::Class(s.clone(), args.clone()));
            line.push_str(&format!(" extends {s}"));
        }
        let interfaces: Vec<String> = interfaces
            .iter()
            .filter(|i| !matches!(i, JavaType::Class(n, _) if n == "java/lang/annotation/Annotation"))
            .map(|i| self.ty(i))
            .collect();
        if !interfaces.is_empty() {
            line.push_str(&format!(" {} {}", if interface { "extends" } else { "implements" }, interfaces.join(", ")));
        }
        self.out.push_str(&line);
        self.out.push_str(" {\n");
        let inner = format!("{indent}    ");
        if is_enum {
            let constants: Vec<&str> =
                class.fields.iter().filter(|f| f.access & classfile::ACC_ENUM != 0).filter_map(|f| java_name(&f.name)).collect();
            self.out.push_str(&format!("{inner}{};\n", constants.join(", ")));
        }
        for f in &class.fields {
            if f.access & (classfile::ACC_SYNTHETIC | classfile::ACC_ENUM) != 0 {
                continue;
            }
            let (Some(visibility), Some(fname)) = (Self::visibility(f.access), java_name(&f.name)) else { continue };
            let Some(ty) = f.signature.as_deref().and_then(classfile::field_type).or_else(|| classfile::field_type(&f.descriptor)) else {
                continue;
            };
            let mut mods = vec![visibility];
            if f.access & classfile::ACC_STATIC != 0 {
                mods.push("static");
            }
            if f.access & classfile::ACC_FINAL != 0 {
                mods.push("final");
            }
            let ty = self.ty(&ty);
            self.out.push_str(&format!("{inner}{} {ty} {fname};\n", mods.join(" ")));
        }
        let inner_class = nested && access & classfile::ACC_STATIC == 0 && !interface;
        for m in &class.methods {
            if m.access & (classfile::ACC_SYNTHETIC | classfile::ACC_BRIDGE) != 0 || m.name == "<clinit>" {
                continue;
            }
            let Some(visibility) = Self::visibility(m.access) else { continue };
            let constructor = m.name == "<init>";
            if constructor && (is_enum || interface) {
                continue;
            }
            let mname = if constructor {
                name
            } else if let Some(n) = java_name(&m.name) {
                n
            } else {
                continue;
            };
            let generic = m.signature.as_deref().and_then(classfile::method_signature);
            let Some(mut sig) = generic.clone().or_else(|| classfile::method_signature(&m.descriptor)) else { continue };
            // Without a signature, an inner class's constructor takes its outer instance first.
            if constructor && inner_class && generic.is_none() && !sig.types.is_empty() {
                sig.types.remove(0);
            }
            let mut mods = vec![visibility];
            if m.access & classfile::ACC_STATIC != 0 {
                mods.push("static");
            } else if interface && m.access & classfile::ACC_ABSTRACT == 0 {
                mods.push("default");
            }
            let tps = self.type_params(&sig.type_params);
            let offset = m.params.len().saturating_sub(sig.types.len());
            let count = sig.types.len();
            let params: Vec<String> = sig
                .types
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let pname = m.params.get(i + offset).and_then(|n| java_name(n)).map_or_else(|| format!("arg{i}"), str::to_owned);
                    match t {
                        JavaType::Array(elem) if i + 1 == count && m.access & classfile::ACC_VARARGS != 0 => {
                            format!("{}... {pname}", self.ty(elem))
                        }
                        t => format!("{} {pname}", self.ty(t)),
                    }
                })
                .collect();
            if constructor {
                self.out.push_str(&format!("{inner}{} {tps}{mname}({});\n", mods.join(" "), params.join(", ")));
            } else {
                let ret = sig.ret.as_ref().map_or_else(|| "void".to_owned(), |r| self.ty(r));
                self.out.push_str(&format!("{inner}{} {tps}{ret} {mname}({});\n", mods.join(" "), params.join(", ")));
            }
        }
        for entry in &class.inner {
            if entry.outer.as_deref() != Some(class.name.as_str()) || entry.name.is_none() {
                continue;
            }
            if let Some(nested_class) = self.classes.get(&entry.inner) {
                self.class(nested_class, entry.access, true, &inner);
            }
        }
        self.out.push_str(&format!("{indent}}}\n"));
    }
}

/// The Java file of one package: its top-level classes.
fn java_file(package: &str, top: &[&str], classes: &HashMap<String, ClassFile>, comment: &str) -> String {
    let own: Vec<String> = top.iter().map(|name| split_internal(name).1).collect();
    let mut w = JavaWriter { imports: Imports::new(package, own, &["java.lang"]), classes, out: String::new() };
    for name in top {
        if let Some(class) = classes.get(*name) {
            w.out.push('\n');
            w.class(class, class.access, false, "");
        }
    }
    let mut text = w.imports.header(comment, ";");
    text.push_str(&w.out);
    text
}

/// The path of a package's generated file: `kotlinx/coroutines/kotlinx.coroutines.kt`.
fn stub_path(package: &str, extension: &str) -> PathBuf {
    let mut path: PathBuf = package.split('.').filter(|p| !p.is_empty()).collect();
    let name = if package.is_empty() { "_root" } else { package };
    path.push(format!("{name}.{extension}"));
    path
}

// ----- jars -----

/// The stub files of a jar of compiled classes.
pub fn jar_stubs(jar: &Path) -> Result<Vec<Stub>> {
    let file = std::fs::File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("failed to read {}", jar.display()))?;
    let mut raw: Vec<Vec<u8>> = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
        if !name.ends_with(".class")
            || name.starts_with("META-INF/")
            || name.ends_with("module-info.class")
            || name.ends_with("package-info.class")
        {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        raw.push(bytes);
    }
    let parsed: Vec<ClassFile> = raw.par_iter().filter_map(|b| classfile::parse(b).ok()).collect();
    let comment = format!(
        "Declarations of {}, generated by Kargo from its compiled classes because it has no sources jar.",
        jar.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    );
    Ok(render_classes(parsed, &comment))
}

/// The JVM descriptor of a Kotlin type in a parameter, or `None` when it can't be told without
/// resolving the type (a type parameter's erasure).
fn jvm_descriptor(t: &km::Type, names: &Names) -> Option<String> {
    let id = names.class(t.class?)?;
    let primitive = match id {
        "kotlin/Int" => Some(("I", "Integer")),
        "kotlin/Long" => Some(("J", "Long")),
        "kotlin/Boolean" => Some(("Z", "Boolean")),
        "kotlin/Byte" => Some(("B", "Byte")),
        "kotlin/Short" => Some(("S", "Short")),
        "kotlin/Char" => Some(("C", "Character")),
        "kotlin/Float" => Some(("F", "Float")),
        "kotlin/Double" => Some(("D", "Double")),
        _ => None,
    };
    if let Some((prim, boxed)) = primitive {
        return Some(if t.nullable { format!("Ljava/lang/{boxed};") } else { prim.to_owned() });
    }
    let arrays = [
        ("kotlin/IntArray", "[I"),
        ("kotlin/LongArray", "[J"),
        ("kotlin/BooleanArray", "[Z"),
        ("kotlin/ByteArray", "[B"),
        ("kotlin/ShortArray", "[S"),
        ("kotlin/CharArray", "[C"),
        ("kotlin/FloatArray", "[F"),
        ("kotlin/DoubleArray", "[D"),
    ];
    if let Some((_, d)) = arrays.iter().find(|(k, _)| *k == id) {
        return Some((*d).to_owned());
    }
    if id == "kotlin/Array" {
        let elem = match t.args.first()? {
            km::TypeArg::Star => return Some("[Ljava/lang/Object;".into()),
            km::TypeArg::In(e) | km::TypeArg::Out(e) | km::TypeArg::Inv(e) => e,
        };
        let mut boxed = elem.clone();
        boxed.nullable = true;
        return jvm_descriptor(&boxed, names).map(|d| format!("[{d}"));
    }
    let mapped = [
        ("kotlin/Any", "java/lang/Object"),
        ("kotlin/String", "java/lang/String"),
        ("kotlin/CharSequence", "java/lang/CharSequence"),
        ("kotlin/Throwable", "java/lang/Throwable"),
        ("kotlin/Number", "java/lang/Number"),
        ("kotlin/Comparable", "java/lang/Comparable"),
        ("kotlin/Enum", "java/lang/Enum"),
        ("kotlin/Annotation", "java/lang/annotation/Annotation"),
        ("kotlin/Cloneable", "java/lang/Cloneable"),
        ("kotlin/Nothing", "java/lang/Void"),
        ("kotlin/Unit", "kotlin/Unit"),
        ("kotlin/collections/Iterable", "java/lang/Iterable"),
        ("kotlin/collections/MutableIterable", "java/lang/Iterable"),
        ("kotlin/collections/Iterator", "java/util/Iterator"),
        ("kotlin/collections/MutableIterator", "java/util/Iterator"),
        ("kotlin/collections/ListIterator", "java/util/ListIterator"),
        ("kotlin/collections/MutableListIterator", "java/util/ListIterator"),
        ("kotlin/collections/Collection", "java/util/Collection"),
        ("kotlin/collections/MutableCollection", "java/util/Collection"),
        ("kotlin/collections/List", "java/util/List"),
        ("kotlin/collections/MutableList", "java/util/List"),
        ("kotlin/collections/Set", "java/util/Set"),
        ("kotlin/collections/MutableSet", "java/util/Set"),
        ("kotlin/collections/Map", "java/util/Map"),
        ("kotlin/collections/MutableMap", "java/util/Map"),
        ("kotlin/collections/Map.Entry", "java/util/Map$Entry"),
        ("kotlin/collections/MutableMap.MutableEntry", "java/util/Map$Entry"),
    ];
    if let Some((_, j)) = mapped.iter().find(|(k, _)| *k == id) {
        return Some(format!("L{j};"));
    }
    if let Some(n) = id.strip_prefix("kotlin/Function").and_then(|n| n.parse::<usize>().ok()) {
        // A suspend function type takes its continuation as one more parameter, already in its arguments.
        return Some(format!("Lkotlin/jvm/functions/Function{n};"));
    }
    if id.starts_with("kotlin/") && !id.contains('.') && !id[7..].contains('/') {
        // Other classes of package `kotlin` may be value classes or mapped; better not to guess.
        return None;
    }
    Some(format!("L{};", id.replace('.', "$")))
}

/// The JVM parameter descriptors of a Kotlin function, `None` where a parameter's can't be told.
fn jvm_params(f: &km::Function, names: &Names) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    if let Some(r) = &f.receiver {
        out.push(jvm_descriptor(r, names));
    }
    for p in &f.params {
        out.push(match &p.vararg {
            Some(elem) => {
                // `vararg x: Int` is an `IntArray`, `vararg x: T` an `Array<out T>`.
                match jvm_descriptor(elem, names).filter(|d| d.len() == 1) {
                    Some(primitive) => Some(format!("[{primitive}")),
                    None => {
                        let mut boxed = elem.clone();
                        boxed.nullable = true;
                        jvm_descriptor(&boxed, names).map(|d| format!("[{d}"))
                    }
                }
            }
            None => jvm_descriptor(&p.ty, names),
        });
    }
    if f.is_suspend() {
        out.push(Some("Lkotlin/coroutines/Continuation;".into()));
    }
    out
}

/// The parameter descriptors of a JVM method descriptor.
fn descriptor_params(desc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = desc.as_bytes();
    let mut i = 1;
    while i < b.len() && b[i] != b')' {
        let start = i;
        while b[i] == b'[' {
            i += 1;
        }
        if b[i] == b'L' {
            while i < b.len() && b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        out.push(desc[start..i.min(desc.len())].to_owned());
    }
    out
}

/// Whether the JVM methods that a declaration named `name` with parameters `params` compiles
/// to are all synthetic. `explicit` is the method's signature when the metadata names it.
fn compiled_synthetic(class: &ClassFile, explicit: &Option<(String, String)>, name: &str, params: &[Option<String>]) -> bool {
    if let Some((name, desc)) = explicit {
        return class.methods.iter().any(|m| m.name == *name && m.descriptor == *desc && m.access & classfile::ACC_SYNTHETIC != 0);
    }
    let candidates: Vec<&classfile::Member> = class
        .methods
        .iter()
        .filter(|m| m.name == name && m.access & classfile::ACC_BRIDGE == 0)
        .filter(|m| {
            let jvm = descriptor_params(&m.descriptor);
            jvm.len() == params.len() && jvm.iter().zip(params).all(|(j, k)| k.as_ref().is_none_or(|k| k == j))
        })
        .collect();
    !candidates.is_empty() && candidates.iter().all(|m| m.access & classfile::ACC_SYNTHETIC != 0)
}

/// Hides the declarations whose JVM methods are synthetic: Kotlin compiles a declaration
/// deprecated with `level = HIDDEN` that way.
fn hide_synthetic(members: &mut km::Package, class: &ClassFile, names: &Names) {
    for f in &mut members.functions {
        let params = jvm_params(f, names);
        f.hidden |= compiled_synthetic(class, &f.jvm, names.string(f.name), &params);
    }
    for p in &mut members.properties {
        let name = names.string(p.name);
        let getter = match name.strip_prefix("is") {
            Some(rest) if rest.starts_with(|c: char| c.is_uppercase()) => name.to_owned(),
            _ => {
                let mut chars = name.chars();
                chars.next().map_or_else(String::new, |c| format!("get{}{}", c.to_uppercase(), chars.as_str()))
            }
        };
        let params: Vec<Option<String>> = p.receiver.iter().map(|r| jvm_descriptor(r, names)).collect();
        p.hidden |= compiled_synthetic(class, &p.jvm_getter, &getter, &params);
    }
}

pub(crate) fn render_classes(parsed: Vec<ClassFile>, comment: &str) -> Vec<Stub> {
    let mut kotlin: ClassMap = HashMap::new();
    let mut facades: BTreeMap<String, Vec<(km::Package, Arc<Names>)>> = BTreeMap::new();
    let mut java: HashMap<String, ClassFile> = HashMap::new();
    for class in parsed {
        if let Some(meta) = &class.metadata {
            match km::read_jvm(meta.kind, &meta.data, &meta.strings) {
                Ok(Some((km::JvmDecls::Class(mut c), names))) => {
                    hide_synthetic(&mut c.members, &class, &names);
                    if let Some(id) = names.class(c.name).map(str::to_owned) {
                        kotlin.insert(id, (c, Arc::new(names)));
                    }
                }
                Ok(Some((km::JvmDecls::Package(mut p), names))) => {
                    hide_synthetic(&mut p, &class, &names);
                    let package = meta.package.clone().unwrap_or_else(|| split_internal(&class.name).0);
                    facades.entry(package).or_default().push((p, Arc::new(names)));
                }
                _ => {}
            }
        } else if class.access & classfile::ACC_SYNTHETIC == 0 {
            java.insert(class.name.clone(), class);
        }
    }
    let mut packages: BTreeMap<String, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
    for id in kotlin.keys() {
        let (package, rel) = split_class_id(id);
        if !rel.contains('.') {
            packages.entry(package).or_default().0.push(id);
        }
    }
    for name in facades.keys() {
        packages.entry(name.clone()).or_default();
    }
    for (name, class) in &java {
        // A nested, local or anonymous class names itself in its `InnerClasses`.
        if class.inner.iter().any(|e| e.inner == *name) {
            continue;
        }
        packages.entry(split_internal(name).0).or_default().1.push(name);
    }
    packages
        .into_par_iter()
        .flat_map_iter(|(package, (mut kt, mut jv))| {
            kt.sort();
            jv.sort();
            let mut out = Vec::new();
            let members: Vec<(&km::Package, &Names)> = facades.get(&package).into_iter().flatten().map(|(p, n)| (p, n.as_ref())).collect();
            if !kt.is_empty() || !members.is_empty() {
                out.push((stub_path(&package, "kt"), kotlin_file(&package, &kt, &members, &kotlin, comment)));
            }
            if !jv.is_empty() {
                out.push((stub_path(&package, "java"), java_file(&package, &jv, &java, comment)));
            }
            out
        })
        .collect()
}

// ----- klibs -----

/// The `.knm` files of a klib (a zip file or an unpacked directory): package and contents.
fn klib_fragments(klib: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let package_of = |path: &str| -> Option<String> {
        let rest = path.strip_prefix("default/linkdata/package_")?;
        let (package, file) = rest.split_once('/')?;
        file.ends_with(".knm").then(|| package.to_owned())
    };
    let mut out = Vec::new();
    if klib.is_dir() {
        let root = klib.join("default").join("linkdata");
        for path in kargo_util::fs::collect_files(&root, &["knm"]) {
            let rel = path.strip_prefix(klib).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if let Some(package) = package_of(&rel) {
                out.push((package, std::fs::read(&path)?));
            }
        }
    } else {
        let file = std::fs::File::open(klib).with_context(|| format!("failed to open {}", klib.display()))?;
        let mut zip = zip::ZipArchive::new(file).with_context(|| format!("failed to read {}", klib.display()))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let Some(package) = package_of(&entry.name().map(|n| n.into_owned()).unwrap_or_default()) else { continue };
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            out.push((package, bytes));
        }
    }
    Ok(out)
}

/// The stub files of a klib.
pub fn klib_stubs(klib: &Path) -> Result<Vec<Stub>> {
    let fragments: Vec<(String, km::Fragment)> =
        klib_fragments(klib)?.into_par_iter().filter_map(|(package, bytes)| km::read_fragment(&bytes).ok().map(|f| (package, f))).collect();
    let mut classes: ClassMap = HashMap::new();
    let mut members: BTreeMap<String, Vec<(km::Package, Arc<Names>)>> = BTreeMap::new();
    for (package, fragment) in fragments {
        let names = Arc::new(fragment.names);
        for class in fragment.classes {
            if let Some(id) = names.class(class.name).map(str::to_owned) {
                classes.insert(id, (class, Arc::clone(&names)));
            }
        }
        let p = &fragment.package;
        if !p.functions.is_empty() || !p.properties.is_empty() || !p.aliases.is_empty() {
            members.entry(package).or_default().push((fragment.package, names));
        } else {
            members.entry(package).or_default();
        }
    }
    let mut packages: BTreeMap<String, Vec<&str>> = members.keys().map(|p| (p.clone(), Vec::new())).collect();
    for id in classes.keys() {
        let (package, rel) = split_class_id(id);
        if !rel.contains('.') {
            packages.entry(package).or_default().push(id);
        }
    }
    let name = klib.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let comment = format!("Declarations of {name}, generated by Kargo from its compiled metadata because it has no sources jar.");
    Ok(packages
        .into_par_iter()
        .filter_map(|(package, mut top)| {
            top.sort();
            let decls: Vec<(&km::Package, &Names)> = members.get(&package).into_iter().flatten().map(|(p, n)| (p, n.as_ref())).collect();
            if top.is_empty() && decls.is_empty() {
                return None;
            }
            Some((stub_path(&package, "kt"), kotlin_file(&package, &top, &decls, &classes, &comment)))
        })
        .collect())
}

/// Compiled libraries made by hand for tests: protobuf messages, class files with Kotlin
/// metadata, and klib fragments, as the Kotlin and Java compilers write them.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;

    use super::super::classfile::{self, ClassFile};

    // ----- protobuf -----

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    /// A varint field.
    pub fn int(field: u32, value: i64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, (field << 3) as u64);
        varint(&mut out, value as u64);
        out
    }

    /// A length-delimited field.
    pub fn bytes(field: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, ((field << 3) | 2) as u64);
        varint(&mut out, data.len() as u64);
        out.extend_from_slice(data);
        out
    }

    pub fn msg(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// A `Type` of class `name` (a string index), nullable or not.
    pub fn class_type(name: i64, nullable: bool) -> Vec<u8> {
        let mut parts = vec![int(6, name)];
        if nullable {
            parts.push(int(3, 1));
        }
        msg(&parts)
    }

    /// A `ValueParameter`.
    pub fn param(name: i64, ty: Vec<u8>, has_default: bool) -> Vec<u8> {
        let mut parts = vec![int(2, name), bytes(3, &ty)];
        if has_default {
            parts.push(int(1, 2));
        }
        msg(&parts)
    }

    // ----- class files -----

    #[derive(Default)]
    struct Pool {
        bytes: Vec<u8>,
        count: u16,
        utf8: HashMap<String, u16>,
    }

    impl Pool {
        fn utf8(&mut self, s: &str) -> u16 {
            if let Some(&i) = self.utf8.get(s) {
                return i;
            }
            // Modified UTF-8: NUL takes two bytes.
            let mut enc = Vec::new();
            for c in s.chars() {
                let c = c as u32;
                match c {
                    1..=0x7F => enc.push(c as u8),
                    0 | 0x80..=0x7FF => enc.extend([0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]),
                    _ => enc.extend([0xE0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]),
                }
            }
            self.bytes.push(1);
            self.bytes.extend((enc.len() as u16).to_be_bytes());
            self.bytes.extend(enc);
            self.count += 1;
            self.utf8.insert(s.to_owned(), self.count);
            self.count
        }

        fn class(&mut self, name: &str) -> u16 {
            let n = self.utf8(name);
            self.bytes.push(7);
            self.bytes.extend(n.to_be_bytes());
            self.count += 1;
            self.count
        }

        fn int(&mut self, v: i32) -> u16 {
            self.bytes.push(3);
            self.bytes.extend(v.to_be_bytes());
            self.count += 1;
            self.count
        }
    }

    /// A method: access, name, descriptor, generic signature and parameter names.
    pub struct Method<'a> {
        pub access: u16,
        pub name: &'a str,
        pub desc: &'a str,
        pub signature: Option<&'a str>,
        pub params: &'a [&'a str],
    }

    /// A class file. `metadata` is `k`, the bytes of `d1` and `d2`; `inner` lists `InnerClasses`
    /// entries (inner, outer, simple name, access).
    pub fn class_file(
        access: u16,
        name: &str,
        methods: &[Method],
        metadata: Option<(i32, &[u8], &[&str])>,
        inner: &[(&str, &str, &str, u16)],
    ) -> Vec<u8> {
        let mut pool = Pool::default();
        let this = pool.class(name);
        let sup = pool.class("java/lang/Object");
        let mut body = Vec::new();
        body.extend(access.to_be_bytes());
        body.extend(this.to_be_bytes());
        body.extend(sup.to_be_bytes());
        body.extend(0u16.to_be_bytes()); // interfaces
        body.extend(0u16.to_be_bytes()); // fields
        body.extend((methods.len() as u16).to_be_bytes());
        for m in methods {
            body.extend(m.access.to_be_bytes());
            body.extend(pool.utf8(m.name).to_be_bytes());
            body.extend(pool.utf8(m.desc).to_be_bytes());
            let mut attrs: Vec<(u16, Vec<u8>)> = Vec::new();
            if let Some(sig) = m.signature {
                let n = pool.utf8("Signature");
                attrs.push((n, pool.utf8(sig).to_be_bytes().to_vec()));
            }
            if !m.params.is_empty() {
                let n = pool.utf8("MethodParameters");
                let mut a = vec![m.params.len() as u8];
                for p in m.params {
                    a.extend(pool.utf8(p).to_be_bytes());
                    a.extend(0u16.to_be_bytes());
                }
                attrs.push((n, a));
            }
            body.extend((attrs.len() as u16).to_be_bytes());
            for (n, a) in attrs {
                body.extend(n.to_be_bytes());
                body.extend((a.len() as u32).to_be_bytes());
                body.extend(a);
            }
        }
        let mut attrs: Vec<(u16, Vec<u8>)> = Vec::new();
        if let Some((k, d1, d2)) = metadata {
            let n = pool.utf8("RuntimeVisibleAnnotations");
            let mut a = Vec::new();
            a.extend(1u16.to_be_bytes());
            a.extend(pool.utf8("Lkotlin/Metadata;").to_be_bytes());
            a.extend(3u16.to_be_bytes());
            a.extend(pool.utf8("k").to_be_bytes());
            a.push(b'I');
            a.extend(pool.int(k).to_be_bytes());
            // `d1` in UTF-8 mode: a NUL, then one character per byte.
            let d1: String = std::iter::once('\u{0}').chain(d1.iter().map(|&b| b as char)).collect();
            for (key, values) in [("d1", vec![d1]), ("d2", d2.iter().map(|s| s.to_string()).collect())] {
                a.extend(pool.utf8(key).to_be_bytes());
                a.push(b'[');
                a.extend((values.len() as u16).to_be_bytes());
                for v in values {
                    a.push(b's');
                    a.extend(pool.utf8(&v).to_be_bytes());
                }
            }
            attrs.push((n, a));
        }
        if !inner.is_empty() {
            let n = pool.utf8("InnerClasses");
            let mut a = (inner.len() as u16).to_be_bytes().to_vec();
            for (i, o, s, acc) in inner {
                a.extend(pool.class(i).to_be_bytes());
                a.extend(pool.class(o).to_be_bytes());
                a.extend(pool.utf8(s).to_be_bytes());
                a.extend(acc.to_be_bytes());
            }
            attrs.push((n, a));
        }
        body.extend((attrs.len() as u16).to_be_bytes());
        for (n, a) in attrs {
            body.extend(n.to_be_bytes());
            body.extend((a.len() as u32).to_be_bytes());
            body.extend(a);
        }
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52];
        out.extend((pool.count + 1).to_be_bytes());
        out.extend(pool.bytes);
        out.extend(body);
        out
    }

    /// The `d1` of a class file: an empty string table (`d2` as it is), then the message.
    fn d1(message: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend(message);
        out
    }

    const PUBLIC: u16 = classfile::ACC_PUBLIC;
    const STATIC: u16 = classfile::ACC_STATIC;
    const SYNTHETIC: u16 = classfile::ACC_SYNTHETIC;

    /// A small library, as the class files of a jar:
    ///
    /// ```kotlin
    /// package p
    /// class Greeter(name: String) {
    ///     val name: String
    ///     fun greet(times: Int = 1): String
    ///     suspend fun pause(): Unit
    /// }
    /// fun String.shout(): String
    /// @Deprecated("", level = DeprecationLevel.HIDDEN) fun old(): Unit
    /// ```
    ///
    /// and, in Java, `public class JavaThing` with a constructor, `static JavaThing of(int value)`,
    /// `<T> void wrap(List<T> items)`, a private method and a nested `public static class Builder`.
    pub fn sample_classes() -> Vec<ClassFile> {
        let d2 = ["p/Greeter", "name", "kotlin/String", "greet", "times", "kotlin/Int", "kotlin/Unit", "pause"];
        let class = msg(&[
            int(3, 0),
            bytes(8, &bytes(2, &param(1, class_type(2, false), false))),
            bytes(10, &msg(&[int(11, 518), int(2, 1), bytes(3, &class_type(2, false))])),
            bytes(9, &msg(&[int(2, 3), bytes(3, &class_type(2, false)), bytes(6, &param(4, class_type(5, false), true))])),
            bytes(9, &msg(&[int(9, 6 | 1 << 13), int(2, 7), bytes(3, &class_type(6, false))])),
        ]);
        let greeter = class_file(
            PUBLIC,
            "p/Greeter",
            &[
                Method { access: PUBLIC, name: "<init>", desc: "(Ljava/lang/String;)V", signature: None, params: &[] },
                Method { access: PUBLIC, name: "greet", desc: "(I)Ljava/lang/String;", signature: None, params: &[] },
            ],
            Some((1, &d1(&class), &d2)),
            &[],
        );
        let d2 = ["shout", "kotlin/String", "old", "kotlin/Unit"];
        let package = msg(&[
            bytes(3, &msg(&[int(2, 0), bytes(5, &class_type(1, false)), bytes(3, &class_type(1, false))])),
            bytes(3, &msg(&[int(2, 2), bytes(3, &class_type(3, false))])),
        ]);
        let facade = class_file(
            PUBLIC,
            "p/UtilsKt",
            &[
                Method {
                    access: PUBLIC | STATIC,
                    name: "shout",
                    desc: "(Ljava/lang/String;)Ljava/lang/String;",
                    signature: None,
                    params: &[],
                },
                Method { access: PUBLIC | STATIC | SYNTHETIC, name: "old", desc: "()V", signature: None, params: &[] },
            ],
            Some((2, &d1(&package), &d2)),
            &[],
        );
        let builder_entry = ("p/JavaThing$Builder", "p/JavaThing", "Builder", PUBLIC | STATIC);
        let java = class_file(
            PUBLIC,
            "p/JavaThing",
            &[
                Method { access: PUBLIC, name: "<init>", desc: "()V", signature: None, params: &[] },
                Method { access: PUBLIC | STATIC, name: "of", desc: "(I)Lp/JavaThing;", signature: None, params: &["value"] },
                Method {
                    access: PUBLIC,
                    name: "wrap",
                    desc: "(Ljava/util/List;)V",
                    signature: Some("<T:Ljava/lang/Object;>(Ljava/util/List<TT;>;)V"),
                    params: &[],
                },
                Method { access: classfile::ACC_PRIVATE, name: "secret", desc: "()V", signature: None, params: &[] },
                Method { access: PUBLIC | SYNTHETIC, name: "lambda$0", desc: "()V", signature: None, params: &[] },
            ],
            None,
            &[builder_entry],
        );
        let builder = class_file(
            PUBLIC,
            "p/JavaThing$Builder",
            &[Method { access: PUBLIC, name: "build", desc: "()Lp/JavaThing;", signature: None, params: &[] }],
            None,
            &[builder_entry],
        );
        [greeter, facade, java, builder].iter().map(|b| classfile::parse(b).expect("a class file")).collect()
    }

    /// A klib package fragment of package `com.example`:
    ///
    /// ```kotlin
    /// enum class Color { RED, GREEN }
    /// /** Paints. */ fun Color.paint(vararg layers: Int): Boolean
    /// ```
    pub fn sample_fragment() -> Vec<u8> {
        let strings: Vec<Vec<u8>> = ["com", "example", "Color", "RED", "GREEN", "paint", "layers", "kotlin", "Int", "Boolean"]
            .iter()
            .map(|s| bytes(1, s.as_bytes()))
            .collect();
        // Qualified names: 0 com, 1 com.example, 2 com.example.Color, 3 kotlin, 4 kotlin.Int, 5 kotlin.Boolean.
        let qualified = msg(&[
            bytes(1, &int(2, 0)),
            bytes(1, &msg(&[int(1, 0), int(2, 1)])),
            bytes(1, &msg(&[int(1, 1), int(2, 2), int(3, 0)])),
            bytes(1, &int(2, 7)),
            bytes(1, &msg(&[int(1, 3), int(2, 8), int(3, 0)])),
            bytes(1, &msg(&[int(1, 3), int(2, 9), int(3, 0)])),
        ]);
        let enum_class = msg(&[int(1, 6 | 2 << 6), int(3, 2), bytes(13, &int(1, 3)), bytes(13, &int(1, 4))]);
        let vararg = msg(&[int(2, 6), bytes(3, &class_type(4, false)), bytes(4, &class_type(4, false))]);
        let paint = msg(&[
            int(2, 5),
            bytes(5, &class_type(2, false)),
            bytes(3, &class_type(5, false)),
            bytes(6, &vararg),
            bytes(174, b"/** Paints. */"),
        ]);
        msg(&[bytes(1, &msg(&strings)), bytes(2, &qualified), bytes(3, &bytes(3, &paint)), bytes(4, &enum_class)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_take_free_names_and_qualify_the_rest() {
        let mut imports = Imports::new("p", ["List".to_owned()], KOTLIN_IMPLICIT);
        assert_eq!(imports.name("p", "List"), "List");
        assert_eq!(imports.name("kotlin.collections", "List"), "kotlin.collections.List");
        assert_eq!(imports.name("kotlin.collections", "Map.Entry"), "Map.Entry");
        assert_eq!(imports.name("java.util", "Map"), "java.util.Map");
        assert_eq!(imports.name("java.io", "File"), "File");
        assert_eq!(imports.header("c", ""), "// c\npackage p\n\nimport java.io.File\n\n");
    }

    #[test]
    fn escapes_names() {
        assert_eq!(kotlin_name("in").as_deref(), Some("`in`"));
        assert_eq!(kotlin_name("my name").as_deref(), Some("`my name`"));
        assert_eq!(kotlin_name("<set-?>"), None);
        assert_eq!(stub_path("a.b", "kt"), PathBuf::from("a/b/a.b.kt"));
        assert_eq!(stub_path("", "java"), PathBuf::from("_root.java"));
    }

    #[test]
    fn renders_kotlin_and_java_classes() {
        let stubs: HashMap<PathBuf, String> = render_classes(testing::sample_classes(), "generated").into_iter().collect();
        let kotlin = &stubs[&PathBuf::from("p/p.kt")];
        let expected = [
            "public fun String.shout(): String",
            "public class Greeter(name: String) {",
            "    public val name: String",
            "    public fun greet(times: Int = ...): String",
            "    public suspend fun pause(): Unit",
        ];
        for line in expected {
            assert!(kotlin.lines().any(|l| l == line), "`{line}` missing from:\n{kotlin}");
        }
        assert!(!kotlin.contains("old"), "a hidden function is left out:\n{kotlin}");

        let java = &stubs[&PathBuf::from("p/p.java")];
        let expected = [
            "package p;",
            "import java.util.List;",
            "public class JavaThing {",
            "    public JavaThing();",
            "    public static JavaThing of(int value);",
            "    public <T> void wrap(List<T> arg0);",
            "    public static class Builder {",
            "        public JavaThing build();",
        ];
        for line in expected {
            assert!(java.lines().any(|l| l == line), "`{line}` missing from:\n{java}");
        }
        assert!(!java.contains("secret") && !java.contains("lambda"), "{java}");
        assert_eq!(java.matches("class Builder").count(), 1, "a nested class is written inside its outer class:\n{java}");
    }

    #[test]
    fn renders_klib_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let klib = dir.path().join("example");
        let package = klib.join("default").join("linkdata").join("package_com.example");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("0_example.knm"), testing::sample_fragment()).unwrap();
        let stubs = klib_stubs(&klib).unwrap();
        assert_eq!(stubs.len(), 1);
        let (path, text) = &stubs[0];
        assert_eq!(path, &PathBuf::from("com/example/com.example.kt"));
        let expected = [
            "package com.example",
            "/** Paints. */",
            "public fun Color.paint(vararg layers: Int): Boolean",
            "public enum class Color {",
            "    RED, GREEN;",
        ];
        for line in expected {
            assert!(text.lines().any(|l| l == line), "`{line}` missing from:\n{text}");
        }
    }
}
