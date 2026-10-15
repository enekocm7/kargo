//! A reader for the parts of a JVM class file that declare things: the class's name, access
//! flags, supertypes and generic signature, its fields and methods (with their descriptors,
//! signatures and parameter names), its nested classes, and the `kotlin.Metadata` annotation
//! that the Kotlin compiler writes on every class it compiles (see [`super::kmetadata`]).
//!
//! Code, stack maps and every other attribute are skipped.

use anyhow::{Context, Result, bail};

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
pub const ACC_BRIDGE: u16 = 0x0040;
pub const ACC_VARARGS: u16 = 0x0080;
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400;
pub const ACC_SYNTHETIC: u16 = 0x1000;
pub const ACC_ANNOTATION: u16 = 0x2000;
pub const ACC_ENUM: u16 = 0x4000;

/// A class file, as far as declarations go. Names are internal names (`java/util/Map$Entry`).
#[derive(Debug, Clone, Default)]
pub struct ClassFile {
    pub access: u16,
    pub name: String,
    pub super_name: Option<String>,
    pub interfaces: Vec<String>,
    /// The generic signature, if the class has type parameters or generic supertypes.
    pub signature: Option<String>,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
    /// The `InnerClasses` entries.
    pub inner: Vec<InnerClass>,
    pub metadata: Option<Metadata>,
}

/// A field or a method.
#[derive(Debug, Clone, Default)]
pub struct Member {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    pub signature: Option<String>,
    /// Parameter names from the `MethodParameters` attribute (`javac -parameters`), if present.
    pub params: Vec<String>,
}

/// An entry of the `InnerClasses` attribute.
#[derive(Debug, Clone, Default)]
pub struct InnerClass {
    pub inner: String,
    pub outer: Option<String>,
    /// The simple name; `None` for an anonymous class.
    pub name: Option<String>,
    pub access: u16,
}

/// The arguments of a `@kotlin.Metadata` annotation.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// `k`: 1 a class, 2 a file facade, 3 a synthetic class, 4 a multi-file facade, 5 a multi-file class part.
    pub kind: i32,
    /// `mv`: the metadata version.
    pub version: Vec<i32>,
    /// `d1`: the protobuf message, bit-encoded into strings.
    pub data: Vec<String>,
    /// `d2`: the string table.
    pub strings: Vec<String>,
    /// `pn`: the Kotlin package name, when it differs from the JVM one.
    pub package: Option<String>,
}

#[derive(Debug, Clone)]
enum Const {
    Utf8(String),
    Class(u16),
    Int(i32),
    Other,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).context("truncated class file")?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// The constant pool, indexed from 1.
struct Pool(Vec<Const>);

impl Pool {
    fn utf8(&self, i: u16) -> Result<&str> {
        match self.0.get(i as usize) {
            Some(Const::Utf8(s)) => Ok(s),
            _ => bail!("constant {i} is not a string"),
        }
    }

    fn class(&self, i: u16) -> Result<&str> {
        match self.0.get(i as usize) {
            Some(Const::Class(name)) => self.utf8(*name),
            _ => bail!("constant {i} is not a class"),
        }
    }

    fn int(&self, i: u16) -> Option<i32> {
        match self.0.get(i as usize) {
            Some(Const::Int(v)) => Some(*v),
            _ => None,
        }
    }
}

/// Decodes the modified UTF-8 of a class file's strings. Characters are kept as UTF-16 code
/// units turned into `char`s where they can be, so that the bit-encoded `d1` strings of Kotlin
/// metadata, whose characters are bytes, survive.
fn modified_utf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i] as u16;
        if b & 0x80 == 0 {
            units.push(b);
            i += 1;
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            units.push(((b & 0x1F) << 6) | (bytes[i + 1] as u16 & 0x3F));
            i += 2;
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            units.push(((b & 0x0F) << 12) | ((bytes[i + 1] as u16 & 0x3F) << 6) | (bytes[i + 2] as u16 & 0x3F));
            i += 3;
        } else {
            units.push(0xFFFD);
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

/// Reads a class file.
pub fn parse(data: &[u8]) -> Result<ClassFile> {
    let mut r = Reader { data, pos: 0 };
    if r.u32()? != 0xCAFE_BABE {
        bail!("not a class file");
    }
    r.u16()?;
    r.u16()?;
    let count = r.u16()?;
    let mut pool = vec![Const::Other];
    while pool.len() < count as usize {
        let tag = r.u8()?;
        let (entry, wide) = match tag {
            1 => {
                let len = r.u16()? as usize;
                (Const::Utf8(modified_utf8(r.bytes(len)?)), false)
            }
            3 => (Const::Int(r.u32()? as i32), false),
            4 => {
                r.bytes(4)?;
                (Const::Other, false)
            }
            5 | 6 => {
                r.bytes(8)?;
                (Const::Other, true)
            }
            7 => (Const::Class(r.u16()?), false),
            8 | 16 | 19 | 20 => {
                r.bytes(2)?;
                (Const::Other, false)
            }
            9..=12 | 17 | 18 => {
                r.bytes(4)?;
                (Const::Other, false)
            }
            15 => {
                r.bytes(3)?;
                (Const::Other, false)
            }
            _ => bail!("unknown constant pool tag {tag}"),
        };
        pool.push(entry);
        if wide {
            pool.push(Const::Other);
        }
    }
    let pool = Pool(pool);
    let mut class = ClassFile { access: r.u16()?, ..ClassFile::default() };
    class.name = pool.class(r.u16()?)?.to_owned();
    let super_index = r.u16()?;
    class.super_name = (super_index != 0).then(|| pool.class(super_index).map(str::to_owned)).transpose()?;
    for _ in 0..r.u16()? {
        class.interfaces.push(pool.class(r.u16()?)?.to_owned());
    }
    class.fields = members(&mut r, &pool)?;
    class.methods = members(&mut r, &pool)?;
    for _ in 0..r.u16()? {
        let name = pool.utf8(r.u16()?)?.to_owned();
        let len = r.u32()? as usize;
        let body = r.bytes(len)?;
        let mut a = Reader { data: body, pos: 0 };
        match name.as_str() {
            "Signature" => class.signature = Some(pool.utf8(a.u16()?)?.to_owned()),
            "InnerClasses" => {
                for _ in 0..a.u16()? {
                    let inner = a.u16()?;
                    let outer = a.u16()?;
                    let name = a.u16()?;
                    let access = a.u16()?;
                    class.inner.push(InnerClass {
                        inner: pool.class(inner)?.to_owned(),
                        outer: (outer != 0).then(|| pool.class(outer).map(str::to_owned)).transpose()?,
                        name: (name != 0).then(|| pool.utf8(name).map(str::to_owned)).transpose()?,
                        access,
                    });
                }
            }
            "RuntimeVisibleAnnotations" => {
                for _ in 0..a.u16()? {
                    let ty = pool.utf8(a.u16()?)?.to_owned();
                    if ty == "Lkotlin/Metadata;" {
                        class.metadata = Some(metadata(&mut a, &pool)?);
                    } else {
                        for _ in 0..a.u16()? {
                            a.u16()?;
                            skip_value(&mut a)?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(class)
}

fn members(r: &mut Reader, pool: &Pool) -> Result<Vec<Member>> {
    let mut out = Vec::new();
    for _ in 0..r.u16()? {
        let mut m = Member { access: r.u16()?, ..Member::default() };
        m.name = pool.utf8(r.u16()?)?.to_owned();
        m.descriptor = pool.utf8(r.u16()?)?.to_owned();
        for _ in 0..r.u16()? {
            let name = pool.utf8(r.u16()?)?;
            let len = r.u32()? as usize;
            let body = r.bytes(len)?;
            let mut a = Reader { data: body, pos: 0 };
            match name {
                "Signature" => m.signature = Some(pool.utf8(a.u16()?)?.to_owned()),
                "MethodParameters" => {
                    for _ in 0..a.u8()? {
                        let name = a.u16()?;
                        a.u16()?;
                        m.params.push(if name == 0 { String::new() } else { pool.utf8(name)?.to_owned() });
                    }
                }
                _ => {}
            }
        }
        out.push(m);
    }
    Ok(out)
}

/// An annotation's element value, as far as `kotlin.Metadata` needs: strings, ints and arrays of them.
enum Value {
    Str(String),
    Int(i32),
    Array(Vec<Value>),
    Other,
}

fn value(r: &mut Reader, pool: &Pool) -> Result<Value> {
    let tag = r.u8()?;
    Ok(match tag {
        b's' => Value::Str(pool.utf8(r.u16()?)?.to_owned()),
        b'I' => pool.int(r.u16()?).map_or(Value::Other, Value::Int),
        b'[' => {
            let n = r.u16()?;
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                items.push(value(r, pool)?);
            }
            Value::Array(items)
        }
        _ => {
            r.pos -= 1;
            skip_value(r)?;
            Value::Other
        }
    })
}

fn skip_value(r: &mut Reader) -> Result<()> {
    match r.u8()? {
        b'e' => {
            r.bytes(4)?;
        }
        b'@' => {
            r.u16()?;
            for _ in 0..r.u16()? {
                r.u16()?;
                skip_value(r)?;
            }
        }
        b'[' => {
            for _ in 0..r.u16()? {
                skip_value(r)?;
            }
        }
        _ => {
            r.u16()?;
        }
    }
    Ok(())
}

fn metadata(r: &mut Reader, pool: &Pool) -> Result<Metadata> {
    let mut m = Metadata { kind: 1, ..Metadata::default() };
    let strings = |v: Value| match v {
        Value::Array(items) => items.into_iter().filter_map(|i| if let Value::Str(s) = i { Some(s) } else { None }).collect(),
        _ => Vec::new(),
    };
    for _ in 0..r.u16()? {
        let name = pool.utf8(r.u16()?)?.to_owned();
        let v = value(r, pool)?;
        match (name.as_str(), v) {
            ("k", Value::Int(k)) => m.kind = k,
            ("mv", Value::Array(items)) => {
                m.version = items.into_iter().filter_map(|i| if let Value::Int(n) = i { Some(n) } else { None }).collect()
            }
            ("d1", v) => m.data = strings(v),
            ("d2", v) => m.strings = strings(v),
            ("pn", Value::Str(s)) if !s.is_empty() => m.package = Some(s),
            _ => {}
        }
    }
    Ok(m)
}

/// A type in a field descriptor or a generic signature, as Java writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaType {
    /// A primitive, by its Java name (`int`, `void`).
    Primitive(&'static str),
    /// A class (internal name, `$` kept) with its type arguments.
    Class(String, Vec<TypeArg>),
    /// A type variable.
    Var(String),
    Array(Box<JavaType>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeArg {
    Star,
    Extends(JavaType),
    Super(JavaType),
    Exact(JavaType),
}

/// A generic method or class signature: type parameters (name and bounds), then the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    pub type_params: Vec<(String, Vec<JavaType>)>,
    /// A method's parameter types, or a class's superclass followed by its interfaces.
    pub types: Vec<JavaType>,
    /// A method's return type.
    pub ret: Option<JavaType>,
}

struct SigParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl SigParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn until(&mut self, stops: &[u8]) -> String {
        let start = self.i;
        while self.peek().is_some_and(|c| !stops.contains(&c)) {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    fn ty(&mut self) -> Option<JavaType> {
        let c = self.peek()?;
        self.i += 1;
        Some(match c {
            b'B' => JavaType::Primitive("byte"),
            b'C' => JavaType::Primitive("char"),
            b'D' => JavaType::Primitive("double"),
            b'F' => JavaType::Primitive("float"),
            b'I' => JavaType::Primitive("int"),
            b'J' => JavaType::Primitive("long"),
            b'S' => JavaType::Primitive("short"),
            b'Z' => JavaType::Primitive("boolean"),
            b'V' => JavaType::Primitive("void"),
            b'[' => JavaType::Array(Box::new(self.ty()?)),
            b'T' => {
                let name = self.until(b";");
                self.eat(b';');
                JavaType::Var(name)
            }
            b'L' => {
                let mut name = self.until(b"<;.");
                let mut args = Vec::new();
                loop {
                    if self.eat(b'<') {
                        args = self.args()?;
                    }
                    if self.eat(b'.') {
                        // An inner class of a generic class: `Outer<T>.Inner<U>`; the outer arguments are dropped.
                        name.push('$');
                        name.push_str(&self.until(b"<;."));
                        args.clear();
                        continue;
                    }
                    break;
                }
                if !self.eat(b';') {
                    return None;
                }
                JavaType::Class(name, args)
            }
            _ => return None,
        })
    }

    fn args(&mut self) -> Option<Vec<TypeArg>> {
        let mut out = Vec::new();
        while !self.eat(b'>') {
            out.push(match self.peek()? {
                b'*' => {
                    self.i += 1;
                    TypeArg::Star
                }
                b'+' => {
                    self.i += 1;
                    TypeArg::Extends(self.ty()?)
                }
                b'-' => {
                    self.i += 1;
                    TypeArg::Super(self.ty()?)
                }
                _ => TypeArg::Exact(self.ty()?),
            });
        }
        Some(out)
    }

    fn type_params(&mut self) -> Option<Vec<(String, Vec<JavaType>)>> {
        let mut out = Vec::new();
        if !self.eat(b'<') {
            return Some(out);
        }
        while !self.eat(b'>') {
            let name = self.until(b":");
            let mut bounds = Vec::new();
            while self.eat(b':') {
                // An empty class bound before an interface bound: `T::Ljava/lang/Comparable;`.
                if self.peek() == Some(b':') {
                    continue;
                }
                bounds.push(self.ty()?);
            }
            out.push((name, bounds));
        }
        Some(out)
    }
}

/// Parses a field descriptor or field signature.
pub fn field_type(sig: &str) -> Option<JavaType> {
    SigParser { s: sig.as_bytes(), i: 0 }.ty()
}

/// Parses a method descriptor or method signature. Thrown types are ignored.
pub fn method_signature(sig: &str) -> Option<Signature> {
    let mut p = SigParser { s: sig.as_bytes(), i: 0 };
    let type_params = p.type_params()?;
    if !p.eat(b'(') {
        return None;
    }
    let mut types = Vec::new();
    while !p.eat(b')') {
        types.push(p.ty()?);
    }
    let ret = p.ty()?;
    Some(Signature { type_params, types, ret: Some(ret) })
}

/// Parses a class signature: type parameters, superclass and interfaces.
pub fn class_signature(sig: &str) -> Option<Signature> {
    let mut p = SigParser { s: sig.as_bytes(), i: 0 };
    let type_params = p.type_params()?;
    let mut types = Vec::new();
    while p.peek().is_some() {
        types.push(p.ty()?);
    }
    Some(Signature { type_params, types, ret: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_signatures() {
        let sig = method_signature("<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/List<+TT;>;[I)TT;").unwrap();
        assert_eq!(sig.type_params.len(), 1);
        assert_eq!(sig.type_params[0].0, "T");
        assert_eq!(
            sig.type_params[0].1,
            vec![JavaType::Class("java/lang/Comparable".into(), vec![TypeArg::Super(JavaType::Var("T".into()))])]
        );
        assert_eq!(sig.types[0], JavaType::Class("java/util/List".into(), vec![TypeArg::Extends(JavaType::Var("T".into()))]));
        assert_eq!(sig.types[1], JavaType::Array(Box::new(JavaType::Primitive("int"))));
        assert_eq!(sig.ret, Some(JavaType::Var("T".into())));

        let class = class_signature("<K:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Map$Entry<TK;*>;").unwrap();
        assert_eq!(class.types.len(), 2);
        assert_eq!(
            class.types[1],
            JavaType::Class("java/util/Map$Entry".into(), vec![TypeArg::Exact(JavaType::Var("K".into())), TypeArg::Star])
        );

        let inner = field_type("Lp/Outer<TT;>.Inner<Ljava/lang/String;>;").unwrap();
        assert_eq!(
            inner,
            JavaType::Class("p/Outer$Inner".into(), vec![TypeArg::Exact(JavaType::Class("java/lang/String".into(), vec![]))])
        );
    }

    #[test]
    fn decodes_modified_utf8() {
        assert_eq!(modified_utf8(&[0xC0, 0x80, b'a']), "\u{0}a");
        assert_eq!(modified_utf8("é".as_bytes()), "é");
    }
}
