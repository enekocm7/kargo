//! Kotlin's compiled declarations: the protobuf messages of `metadata.proto` that the Kotlin
//! compiler writes into every class file (the `d1` and `d2` of `@kotlin.Metadata`, see
//! [`super::classfile`]) and into every klib (`default/linkdata/package_*/*.knm`).
//!
//! The messages are decoded into the plain declarations below, with type references (in place
//! or by index into a type table) already resolved, and names left as indexes into [`Names`].
//! Only what a declaration's signature needs is kept: flags, names, type parameters, types,
//! parameters, supertypes, members and enum entries. Annotations, contracts, version
//! requirements and compiler plugin data are skipped.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};

/// A message's fields, in order: field number and value.
type Fields<'a> = Vec<(u32, Wire<'a>)>;

#[derive(Debug, Clone, Copy)]
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut out = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *data.get(*pos).context("truncated varint")?;
        *pos += 1;
        out |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    bail!("varint too long")
}

fn fields(data: &[u8]) -> Result<Fields<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let key = varint(data, &mut pos)?;
        let field = (key >> 3) as u32;
        let wire = match key & 7 {
            0 => Wire::Varint(varint(data, &mut pos)?),
            1 => {
                pos += 8;
                Wire::Fixed
            }
            2 => {
                let len = varint(data, &mut pos)? as usize;
                let end = pos.checked_add(len).filter(|&e| e <= data.len()).context("truncated message")?;
                let bytes = &data[pos..end];
                pos = end;
                Wire::Bytes(bytes)
            }
            5 => {
                pos += 4;
                Wire::Fixed
            }
            w => bail!("unsupported wire type {w}"),
        };
        out.push((field, wire));
    }
    if pos > data.len() {
        bail!("truncated message");
    }
    Ok(out)
}

fn int(fields: &Fields, n: u32) -> Option<i32> {
    fields.iter().rev().find_map(|(f, w)| match (f, w) {
        (f, Wire::Varint(v)) if *f == n => Some(*v as i32),
        _ => None,
    })
}

fn message<'a>(fields: &Fields<'a>, n: u32) -> Option<&'a [u8]> {
    fields.iter().rev().find_map(|(f, w)| match (f, w) {
        (f, Wire::Bytes(b)) if *f == n => Some(*b),
        _ => None,
    })
}

fn messages<'a, 'b>(fields: &'b Fields<'a>, n: u32) -> impl Iterator<Item = &'a [u8]> + 'b {
    fields.iter().filter_map(move |(f, w)| match (f, w) {
        (f, Wire::Bytes(b)) if *f == n => Some(*b),
        _ => None,
    })
}

/// A repeated int field, packed or not.
fn ints(fields: &Fields, n: u32) -> Vec<i32> {
    let mut out = Vec::new();
    for (f, w) in fields {
        if *f != n {
            continue;
        }
        match w {
            Wire::Varint(v) => out.push(*v as i32),
            Wire::Bytes(b) => {
                let mut pos = 0;
                while pos < b.len() {
                    match varint(b, &mut pos) {
                        Ok(v) => out.push(v as i32),
                        Err(_) => break,
                    }
                }
            }
            Wire::Fixed => {}
        }
    }
    out
}

fn string(fields: &Fields, n: u32) -> Option<String> {
    message(fields, n).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// Whether the annotations at fields `ns` include `@Deprecated(level = DeprecationLevel.HIDDEN)`.
fn hidden(f: &Fields, ns: &[u32], names: &Names) -> bool {
    ns.iter().flat_map(|&n| messages(f, n)).any(|a| {
        let Ok(a) = fields(a) else { return false };
        if int(&a, 1).and_then(|id| names.class(id)) != Some("kotlin/Deprecated") {
            return false;
        }
        messages(&a, 2).any(|arg| {
            let Ok(arg) = fields(arg) else { return false };
            let value = message(&arg, 2).and_then(|v| fields(v).ok());
            names.string(int(&arg, 1).unwrap_or(-1)) == "level"
                && value.and_then(|v| int(&v, 7)).is_some_and(|e| names.string(e) == "HIDDEN")
        })
    })
}

/// The KDoc a klib keeps at field `n`, when it was compiled with it.
fn kdoc(f: &Fields, n: u32) -> Option<String> {
    string(f, n).filter(|d| !d.trim().is_empty())
}

/// A JVM method signature (`JvmMethodSignature`): name and descriptor.
fn jvm_method(bytes: &[u8], names: &Names) -> Option<(String, String)> {
    let f = fields(bytes).ok()?;
    Some((names.string(int(&f, 1)?).to_owned(), names.string(int(&f, 2)?).to_owned()))
}

// ----- flags (see `Flags.java` in the Kotlin compiler) -----

/// A declaration's visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Internal,
    Private,
    Protected,
    Public,
    PrivateToThis,
    Local,
}

impl Visibility {
    fn of(flags: i32) -> Self {
        match (flags >> 1) & 7 {
            0 => Visibility::Internal,
            1 => Visibility::Private,
            2 => Visibility::Protected,
            4 => Visibility::PrivateToThis,
            5 => Visibility::Local,
            _ => Visibility::Public,
        }
    }

    /// Whether code outside the library can see it.
    pub fn is_api(self) -> bool {
        matches!(self, Visibility::Public | Visibility::Protected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Final,
    Open,
    Abstract,
    Sealed,
}

impl Modality {
    fn of(flags: i32) -> Self {
        match (flags >> 4) & 3 {
            1 => Modality::Open,
            2 => Modality::Abstract,
            3 => Modality::Sealed,
            _ => Modality::Final,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Interface,
    EnumClass,
    EnumEntry,
    AnnotationClass,
    Object,
    CompanionObject,
}

fn bit(flags: i32, n: u32) -> bool {
    flags & (1 << n) != 0
}

// ----- names -----

/// The strings and class names a message refers to by index.
///
/// Class names come in the form `kotlin/collections/Map.Entry`: the package with `/`, then
/// the class and its outer classes with `.`.
#[derive(Debug, Clone, Default)]
pub struct Names {
    strings: Vec<String>,
    /// Class names, for klibs, whose qualified names are a table of their own; on the JVM they are strings.
    classes: Option<Vec<String>>,
    local: HashSet<i32>,
}

/// The Kotlin built-in names that JVM string tables refer to by number (`JvmNameResolverBase`).
const PREDEFINED: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

impl Names {
    pub fn string(&self, i: i32) -> &str {
        usize::try_from(i).ok().and_then(|i| self.strings.get(i)).map_or("", String::as_str)
    }

    /// A class name, or `None` for a local or anonymous class.
    pub fn class(&self, i: i32) -> Option<&str> {
        if self.local.contains(&i) {
            return None;
        }
        let name = match &self.classes {
            Some(classes) => usize::try_from(i).ok().and_then(|i| classes.get(i)).map(String::as_str)?,
            None => self.string(i),
        };
        (!name.is_empty() && !name.starts_with('.')).then_some(name)
    }

    /// The names of a JVM class file: `d2`, rewritten by the `StringTableTypes` records.
    fn jvm(types: &[u8], d2: &[String]) -> Result<Self> {
        let f = fields(types)?;
        let mut strings = Vec::with_capacity(d2.len());
        for record in messages(&f, 1) {
            let r = fields(record)?;
            let range = int(&r, 1).unwrap_or(1).max(1);
            let predefined = int(&r, 2);
            let own = string(&r, 6);
            let operation = int(&r, 3).unwrap_or(0);
            let substring = ints(&r, 4);
            let replace = ints(&r, 5);
            for _ in 0..range {
                let index = strings.len();
                let mut s = match (&own, predefined) {
                    (Some(s), _) => s.clone(),
                    (None, Some(p)) if (p as usize) < PREDEFINED.len() => PREDEFINED[p as usize].to_owned(),
                    _ => d2.get(index).cloned().unwrap_or_default(),
                };
                if let [begin, end, ..] = substring[..] {
                    let chars: Vec<char> = s.chars().collect();
                    if 0 <= begin && begin <= end && (end as usize) <= chars.len() {
                        s = chars[begin as usize..end as usize].iter().collect();
                    }
                }
                if let [from, to, ..] = replace[..]
                    && let (Some(from), Some(to)) = (char::from_u32(from as u32), char::from_u32(to as u32))
                {
                    s = s.replace(from, &to.to_string());
                }
                match operation {
                    1 => s = s.replace('$', "."),
                    2 => {
                        let chars: Vec<char> = s.chars().collect();
                        if chars.len() >= 2 {
                            s = chars[1..chars.len() - 1].iter().collect();
                        }
                        s = s.replace('$', ".");
                    }
                    _ => {}
                }
                strings.push(s);
                if strings.len() > d2.len() + 100_000 {
                    bail!("string table too long");
                }
            }
        }
        // Strings past the records are used as they are.
        while strings.len() < d2.len() {
            strings.push(d2[strings.len()].clone());
        }
        Ok(Names { strings, classes: None, local: ints(&f, 5).into_iter().collect() })
    }

    /// The names of a klib package fragment: its string table and qualified name table.
    fn klib(strings: Option<&[u8]>, qualified: Option<&[u8]>) -> Result<Self> {
        let strings = match strings {
            Some(b) => fields(b)?.into_iter().filter(|(f, _)| *f == 1).filter_map(|(_, w)| match w {
                Wire::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            }),
            None => return Ok(Names::default()),
        }
        .collect::<Vec<_>>();
        // (parent, short name, kind: 0 class, 1 package, 2 local)
        let mut table: Vec<(i32, i32, i32)> = Vec::new();
        if let Some(q) = qualified {
            for entry in messages(&fields(q)?, 1) {
                let e = fields(entry)?;
                table.push((int(&e, 1).unwrap_or(-1), int(&e, 2).unwrap_or(0), int(&e, 3).unwrap_or(1)));
            }
        }
        let mut classes = Vec::with_capacity(table.len());
        let mut local = HashSet::new();
        for i in 0..table.len() {
            let (mut packages, mut nested) = (Vec::new(), Vec::new());
            let mut at = i as i32;
            let mut steps = 0;
            while at >= 0 && (at as usize) < table.len() && steps < 256 {
                let (parent, short, kind) = table[at as usize];
                let name = strings.get(short as usize).cloned().unwrap_or_default();
                match kind {
                    1 => packages.push(name),
                    k => {
                        if k == 2 {
                            local.insert(i as i32);
                        }
                        nested.push(name);
                    }
                }
                at = parent;
                steps += 1;
            }
            packages.reverse();
            nested.reverse();
            let package = packages.join("/");
            classes.push(if package.is_empty() { nested.join(".") } else { format!("{package}/{}", nested.join(".")) });
        }
        Ok(Names { strings, classes: Some(classes), local })
    }
}

// ----- declarations -----

#[derive(Debug, Clone, Default)]
pub struct Type {
    /// A class (by [`Names::class`] index), a type parameter (by id) or one by name.
    pub class: Option<i32>,
    pub type_param: Option<i32>,
    pub type_param_name: Option<i32>,
    pub args: Vec<TypeArg>,
    pub nullable: bool,
    pub suspend: bool,
    /// The `kotlin.ExtensionFunctionType` annotation: the first argument of a function type is its receiver.
    pub extension: bool,
    /// The abbreviation of a type alias that expands to this type.
    pub alias: Option<Box<Type>>,
    /// The class of a type alias (when this is an abbreviation).
    pub alias_name: Option<i32>,
}

#[derive(Debug, Clone)]
pub enum TypeArg {
    Star,
    In(Type),
    Out(Type),
    Inv(Type),
}

#[derive(Debug, Clone, Default)]
pub struct TypeParam {
    pub id: i32,
    pub name: i32,
    pub reified: bool,
    /// 0 in, 1 out, 2 invariant.
    pub variance: i32,
    pub bounds: Vec<Type>,
}

#[derive(Debug, Clone, Default)]
pub struct Param {
    pub name: i32,
    pub ty: Type,
    /// The element type, for a `vararg` parameter.
    pub vararg: Option<Type>,
    pub has_default: bool,
    pub crossinline: bool,
    pub noinline: bool,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub flags: i32,
    pub name: i32,
    pub type_params: Vec<TypeParam>,
    pub receiver: Option<Type>,
    pub params: Vec<Param>,
    pub ret: Type,
    /// Hidden by `@Deprecated(level = HIDDEN)`: compiled code still calls it, sources can't.
    pub hidden: bool,
    /// The JVM method, when the metadata names it.
    pub jvm: Option<(String, String)>,
    pub doc: Option<String>,
}

impl Function {
    pub fn visibility(&self) -> Visibility {
        Visibility::of(self.flags)
    }
    pub fn modality(&self) -> Modality {
        Modality::of(self.flags)
    }
    /// Declared here, delegated or synthesized (not a fake override).
    pub fn is_declared(&self) -> bool {
        (self.flags >> 6) & 3 != 1
    }
    pub fn is_operator(&self) -> bool {
        bit(self.flags, 8)
    }
    pub fn is_infix(&self) -> bool {
        bit(self.flags, 9)
    }
    pub fn is_inline(&self) -> bool {
        bit(self.flags, 10)
    }
    pub fn is_tailrec(&self) -> bool {
        bit(self.flags, 11)
    }
    pub fn is_external(&self) -> bool {
        bit(self.flags, 12)
    }
    pub fn is_suspend(&self) -> bool {
        bit(self.flags, 13)
    }
}

#[derive(Debug, Clone)]
pub struct Property {
    pub flags: i32,
    pub setter_flags: Option<i32>,
    pub name: i32,
    pub type_params: Vec<TypeParam>,
    pub receiver: Option<Type>,
    pub ty: Type,
    pub hidden: bool,
    /// The JVM getter, when the metadata names it.
    pub jvm_getter: Option<(String, String)>,
    pub doc: Option<String>,
}

impl Property {
    pub fn visibility(&self) -> Visibility {
        Visibility::of(self.flags)
    }
    pub fn modality(&self) -> Modality {
        Modality::of(self.flags)
    }
    pub fn is_declared(&self) -> bool {
        (self.flags >> 6) & 3 != 1
    }
    /// A `var` whose setter code outside the library can call.
    pub fn is_var(&self) -> bool {
        bit(self.flags, 8) && bit(self.flags, 10) && self.setter_flags.is_none_or(|f| Visibility::of(f).is_api())
    }
    pub fn is_const(&self) -> bool {
        bit(self.flags, 11)
    }
    pub fn is_lateinit(&self) -> bool {
        bit(self.flags, 12)
    }
}

#[derive(Debug, Clone)]
pub struct Constructor {
    pub flags: i32,
    pub params: Vec<Param>,
    pub hidden: bool,
    pub doc: Option<String>,
}

impl Constructor {
    pub fn visibility(&self) -> Visibility {
        Visibility::of(self.flags)
    }
    pub fn is_secondary(&self) -> bool {
        bit(self.flags, 4)
    }
}

#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub flags: i32,
    pub name: i32,
    pub type_params: Vec<TypeParam>,
    pub underlying: Type,
}

impl TypeAlias {
    pub fn visibility(&self) -> Visibility {
        Visibility::of(self.flags)
    }
}

/// Top-level functions, properties and type aliases.
#[derive(Debug, Clone, Default)]
pub struct Package {
    pub functions: Vec<Function>,
    pub properties: Vec<Property>,
    pub aliases: Vec<TypeAlias>,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub flags: i32,
    /// The class's name (see [`Names::class`]).
    pub name: i32,
    pub companion: Option<i32>,
    pub type_params: Vec<TypeParam>,
    pub supertypes: Vec<Type>,
    pub nested: Vec<i32>,
    pub constructors: Vec<Constructor>,
    pub members: Package,
    pub enum_entries: Vec<i32>,
    pub hidden: bool,
    pub doc: Option<String>,
}

impl Class {
    pub fn visibility(&self) -> Visibility {
        Visibility::of(self.flags)
    }
    pub fn modality(&self) -> Modality {
        Modality::of(self.flags)
    }
    pub fn kind(&self) -> ClassKind {
        match (self.flags >> 6) & 7 {
            1 => ClassKind::Interface,
            2 => ClassKind::EnumClass,
            3 => ClassKind::EnumEntry,
            4 => ClassKind::AnnotationClass,
            5 => ClassKind::Object,
            6 => ClassKind::CompanionObject,
            _ => ClassKind::Class,
        }
    }
    pub fn is_inner(&self) -> bool {
        bit(self.flags, 9)
    }
    pub fn is_data(&self) -> bool {
        bit(self.flags, 10)
    }
    pub fn is_expect(&self) -> bool {
        bit(self.flags, 12)
    }
    pub fn is_value(&self) -> bool {
        bit(self.flags, 13)
    }
    pub fn is_fun(&self) -> bool {
        bit(self.flags, 14)
    }
}

/// Resolves type references: types in place, or indexes into the nearest type table.
#[derive(Clone, Copy)]
struct Types<'a> {
    table: &'a [&'a [u8]],
    first_nullable: i32,
    depth: u32,
}

const EXTENSION_FUNCTION_TYPE: &str = "kotlin/ExtensionFunctionType";

impl<'a> Types<'a> {
    fn new(table: &'a [&'a [u8]], first_nullable: i32) -> Self {
        Types { table, first_nullable, depth: 0 }
    }

    fn deeper(self) -> Result<Self> {
        if self.depth > 48 {
            bail!("type nested too deeply");
        }
        Ok(Types { depth: self.depth + 1, ..self })
    }

    fn by_id(self, id: i32, names: &Names) -> Result<Type> {
        let bytes = usize::try_from(id).ok().and_then(|i| self.table.get(i)).context("type index out of range")?;
        let mut ty = self.deeper()?.parse(bytes, names)?;
        if self.first_nullable >= 0 && id >= self.first_nullable {
            ty.nullable = true;
        }
        Ok(ty)
    }

    /// The type in field `inline`, or the one indexed by field `id`.
    fn field(self, f: &Fields, inline: u32, id: u32, names: &Names) -> Result<Option<Type>> {
        if let Some(b) = message(f, inline) {
            return self.deeper()?.parse(b, names).map(Some);
        }
        int(f, id).map(|i| self.by_id(i, names)).transpose()
    }

    fn parse(self, bytes: &[u8], names: &Names) -> Result<Type> {
        let f = fields(bytes)?;
        let flags = int(&f, 1).unwrap_or(0);
        let mut ty = Type {
            class: int(&f, 6),
            type_param: int(&f, 7),
            type_param_name: int(&f, 9),
            alias_name: int(&f, 12),
            nullable: int(&f, 3).unwrap_or(0) != 0,
            suspend: bit(flags, 0),
            ..Type::default()
        };
        for arg in messages(&f, 2) {
            let a = fields(arg)?;
            let projection = int(&a, 1).unwrap_or(2);
            let inner = self.field(&a, 2, 3, names)?;
            ty.args.push(match (projection, inner) {
                (3, _) | (_, None) => TypeArg::Star,
                (0, Some(t)) => TypeArg::In(t),
                (1, Some(t)) => TypeArg::Out(t),
                (_, Some(t)) => TypeArg::Inv(t),
            });
        }
        if let Some(alias) = self.field(&f, 13, 14, names)? {
            ty.alias = Some(Box::new(alias));
        }
        for annotation in messages(&f, 100).chain(messages(&f, 170)) {
            if int(&fields(annotation)?, 1).and_then(|id| names.class(id)) == Some(EXTENSION_FUNCTION_TYPE) {
                ty.extension = true;
            }
        }
        Ok(ty)
    }

    fn type_params(self, f: &Fields, n: u32, names: &Names) -> Result<Vec<TypeParam>> {
        let mut out = Vec::new();
        for b in messages(f, n) {
            let p = fields(b)?;
            let mut bounds = Vec::new();
            for bound in messages(&p, 5) {
                bounds.push(self.deeper()?.parse(bound, names)?);
            }
            for id in ints(&p, 6) {
                bounds.push(self.by_id(id, names)?);
            }
            out.push(TypeParam {
                id: int(&p, 1).unwrap_or(0),
                name: int(&p, 2).unwrap_or(0),
                reified: int(&p, 3).unwrap_or(0) != 0,
                variance: int(&p, 4).unwrap_or(2),
                bounds,
            });
        }
        Ok(out)
    }

    fn params(self, f: &Fields, n: u32, names: &Names) -> Result<Vec<Param>> {
        let mut out = Vec::new();
        for b in messages(f, n) {
            let p = fields(b)?;
            let flags = int(&p, 1).unwrap_or(0);
            out.push(Param {
                name: int(&p, 2).unwrap_or(0),
                ty: self.field(&p, 3, 5, names)?.unwrap_or_default(),
                vararg: self.field(&p, 4, 6, names)?,
                has_default: bit(flags, 1),
                crossinline: bit(flags, 2),
                noinline: bit(flags, 3),
            });
        }
        Ok(out)
    }

    fn function(self, bytes: &[u8], names: &Names) -> Result<Function> {
        let f = fields(bytes)?;
        let own = table(&f)?;
        let t = match &own {
            Some((table, first)) => Types { table, first_nullable: *first, depth: self.depth },
            None => self,
        };
        Ok(Function {
            flags: int(&f, 9).unwrap_or(6),
            name: int(&f, 2).unwrap_or(0),
            type_params: t.type_params(&f, 4, names)?,
            receiver: t.field(&f, 5, 8, names)?,
            params: t.params(&f, 6, names)?,
            ret: t.field(&f, 3, 7, names)?.unwrap_or_default(),
            hidden: hidden(&f, &[12, 170], names),
            jvm: message(&f, 100).and_then(|b| jvm_method(b, names)),
            doc: kdoc(&f, 174),
        })
    }

    fn property(self, bytes: &[u8], names: &Names) -> Result<Property> {
        let f = fields(bytes)?;
        Ok(Property {
            flags: int(&f, 11).unwrap_or(518),
            setter_flags: int(&f, 8),
            name: int(&f, 2).unwrap_or(0),
            type_params: self.type_params(&f, 4, names)?,
            receiver: self.field(&f, 5, 10, names)?,
            ty: self.field(&f, 3, 9, names)?.unwrap_or_default(),
            hidden: hidden(&f, &[14, 170, 177], names),
            jvm_getter: message(&f, 100).and_then(|b| fields(b).ok()).and_then(|s| message(&s, 3)).and_then(|b| jvm_method(b, names)),
            doc: kdoc(&f, 180),
        })
    }

    fn alias(self, bytes: &[u8], names: &Names) -> Result<TypeAlias> {
        let f = fields(bytes)?;
        Ok(TypeAlias {
            flags: int(&f, 1).unwrap_or(6),
            name: int(&f, 2).unwrap_or(0),
            type_params: self.type_params(&f, 3, names)?,
            underlying: self.field(&f, 4, 5, names)?.unwrap_or_default(),
        })
    }

    /// Functions, properties and type aliases at fields `fun`, `prop` and `alias`.
    fn members(self, f: &Fields, (fun, prop, alias): (u32, u32, u32), names: &Names) -> Package {
        // A declaration that doesn't decode is left out rather than losing the whole class.
        Package {
            functions: messages(f, fun).filter_map(|b| self.function(b, names).ok()).collect(),
            properties: messages(f, prop).filter_map(|b| self.property(b, names).ok()).collect(),
            aliases: messages(f, alias).filter_map(|b| self.alias(b, names).ok()).collect(),
        }
    }
}

/// A type table: its types, still encoded, and the index of the first nullable one (-1 for none).
type Table<'a> = (Vec<&'a [u8]>, i32);

/// The type table of a message (field 30), if it has one.
fn table<'a>(f: &Fields<'a>) -> Result<Option<Table<'a>>> {
    let Some(b) = message(f, 30) else { return Ok(None) };
    let t = fields(b)?;
    let types: Vec<&[u8]> = messages(&t, 1).collect();
    Ok(Some((types, int(&t, 2).unwrap_or(-1))))
}

pub fn parse_class(bytes: &[u8], names: &Names) -> Result<Class> {
    let f = fields(bytes)?;
    let own = table(&f)?;
    let (table, first): (&[&[u8]], i32) = match &own {
        Some((t, first)) => (t, *first),
        None => (&[], -1),
    };
    let t = Types::new(table, first);
    let mut supertypes = Vec::new();
    for b in messages(&f, 6) {
        supertypes.push(t.parse(b, names)?);
    }
    for id in ints(&f, 2) {
        supertypes.push(t.by_id(id, names)?);
    }
    let mut constructors = Vec::new();
    for b in messages(&f, 8) {
        let c = fields(b)?;
        constructors.push(Constructor {
            flags: int(&c, 1).unwrap_or(6),
            params: t.params(&c, 2, names)?,
            hidden: hidden(&c, &[3, 170], names),
            doc: kdoc(&c, 173),
        });
    }
    let enum_entries = messages(&f, 13).filter_map(|b| fields(b).ok().and_then(|e| int(&e, 1))).collect();
    Ok(Class {
        flags: int(&f, 1).unwrap_or(6),
        name: int(&f, 3).context("a class without a name")?,
        companion: int(&f, 4),
        type_params: t.type_params(&f, 5, names)?,
        supertypes,
        nested: ints(&f, 7),
        constructors,
        members: t.members(&f, (9, 10, 11), names),
        enum_entries,
        hidden: hidden(&f, &[25, 170], names),
        doc: kdoc(&f, 176),
    })
}

pub fn parse_package(bytes: &[u8], names: &Names) -> Result<Package> {
    let f = fields(bytes)?;
    let own = table(&f)?;
    let (table, first): (&[&[u8]], i32) = match &own {
        Some((t, first)) => (t, *first),
        None => (&[], -1),
    };
    Ok(Types::new(table, first).members(&f, (3, 4, 5), names))
}

// ----- JVM -----

/// The bytes of `d1` (see `BitEncoding.decodeBytes` in the Kotlin compiler).
pub fn decode_d1(d1: &[String]) -> Vec<u8> {
    let mut data: Vec<String> = d1.to_vec();
    if let Some(first) = data.first_mut()
        && let Some(marker) = first.chars().next()
    {
        if marker == '\u{0}' {
            first.remove(0);
            return data.iter().flat_map(|s| s.chars().map(|c| c as u32 as u8)).collect();
        }
        if marker == '\u{FFFF}' {
            first.remove(0);
        }
    }
    let bytes: Vec<u8> = data.iter().flat_map(|s| s.chars().map(|c| ((c as u32 as u8).wrapping_add(0x7F)) & 0x7F)).collect();
    // Seven bits per byte into eight.
    let len = 7 * bytes.len() / 8;
    let mut out = vec![0u8; len];
    let (mut index, mut bit) = (0usize, 0u32);
    for item in out.iter_mut() {
        let first = (bytes[index] as u32) >> bit;
        index += 1;
        let second = ((bytes.get(index).copied().unwrap_or(0) as u32) & ((1 << (bit + 1)) - 1)) << (7 - bit);
        *item = (first + second) as u8;
        if bit == 6 {
            index += 1;
            bit = 0;
        } else {
            bit += 1;
        }
    }
    out
}

/// What a class file's Kotlin metadata declares.
pub enum JvmDecls {
    Class(Class),
    /// A file facade or a part of a multi-file facade.
    Package(Package),
}

/// Decodes the `kotlin.Metadata` of a class file of kind `k` (1, 2 or 5; others are `None`).
pub fn read_jvm(kind: i32, d1: &[String], d2: &[String]) -> Result<Option<(JvmDecls, Names)>> {
    if !matches!(kind, 1 | 2 | 5) {
        return Ok(None);
    }
    let bytes = decode_d1(d1);
    let mut pos = 0;
    let len = varint(&bytes, &mut pos)? as usize;
    let end = pos.checked_add(len).filter(|&e| e <= bytes.len()).context("truncated metadata")?;
    let names = Names::jvm(&bytes[pos..end], d2)?;
    let rest = &bytes[end..];
    let decls = if kind == 1 { JvmDecls::Class(parse_class(rest, &names)?) } else { JvmDecls::Package(parse_package(rest, &names)?) };
    Ok(Some((decls, names)))
}

// ----- klibs -----

/// A klib package fragment (a `.knm` file): its names, top-level declarations and classes.
pub struct Fragment {
    pub names: Names,
    pub package: Package,
    pub classes: Vec<Class>,
}

pub fn read_fragment(bytes: &[u8]) -> Result<Fragment> {
    let f = fields(bytes)?;
    let names = Names::klib(message(&f, 1), message(&f, 2))?;
    let package = message(&f, 3).map(|b| parse_package(b, &names)).transpose()?.unwrap_or_default();
    let classes = messages(&f, 4).filter_map(|b| parse_class(b, &names).ok()).collect();
    Ok(Fragment { names, package, classes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(field: u32, wire: u32) -> u8 {
        ((field << 3) | wire) as u8
    }

    fn bytes(field: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![key(field, 2), data.len() as u8];
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn decodes_d1_in_both_encodings() {
        let data = [1u8, 0, 200, 7, 0x7F, 0x80];
        let utf8 = format!("\u{0}{}", data.iter().map(|&b| b as char).collect::<String>());
        assert_eq!(decode_d1(&[utf8]), data);

        // 8-to-7 encoding as `BitEncoding.encodeBytes` does it with `kotlin.jvm.serialization.use8to7`.
        let mut seven = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for &b in &data {
            acc |= (b as u32) << bits;
            bits += 8;
            while bits >= 7 {
                seven.push((acc & 0x7F) as u8);
                acc >>= 7;
                bits -= 7;
            }
        }
        if bits > 0 {
            seven.push((acc & 0x7F) as u8);
        }
        let encoded: String = std::iter::once('\u{FFFF}').chain(seven.iter().map(|&b| ((b + 1) & 0x7F) as char)).collect();
        assert_eq!(decode_d1(&[encoded]), data);
    }

    #[test]
    fn resolves_jvm_string_records() {
        // Record 1: two strings as they are; record 2: predefined "kotlin/Int"; record 3: "Lp/A$B;" as a class id.
        let mut types = Vec::new();
        types.extend(bytes(1, &[key(1, 0), 2]));
        types.extend(bytes(1, &[key(2, 0), 8]));
        types.extend(bytes(1, &[key(3, 0), 2]));
        let d2: Vec<String> = ["a", "b", "", "Lp/A$B;"].iter().map(|s| s.to_string()).collect();
        let names = Names::jvm(&types, &d2).unwrap();
        assert_eq!(names.string(0), "a");
        assert_eq!(names.string(1), "b");
        assert_eq!(names.class(2), Some("kotlin/Int"));
        assert_eq!(names.class(3), Some("p/A.B"));
    }

    #[test]
    fn builds_klib_qualified_names() {
        let mut strings = Vec::new();
        for s in ["com", "example", "Outer", "Inner"] {
            strings.extend(bytes(1, s.as_bytes()));
        }
        let mut qualified = Vec::new();
        // com (package), com.example (package), Outer (class), Outer.Inner (class)
        qualified.extend(bytes(1, &[key(2, 0), 0]));
        qualified.extend(bytes(1, &[key(1, 0), 0, key(2, 0), 1]));
        qualified.extend(bytes(1, &[key(1, 0), 1, key(2, 0), 2, key(3, 0), 0]));
        qualified.extend(bytes(1, &[key(1, 0), 2, key(2, 0), 3, key(3, 0), 0]));
        let names = Names::klib(Some(&strings), Some(&qualified)).unwrap();
        assert_eq!(names.class(2), Some("com/example/Outer"));
        assert_eq!(names.class(3), Some("com/example/Outer.Inner"));
    }
}
