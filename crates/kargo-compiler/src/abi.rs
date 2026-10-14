//! ABI fingerprints of JVM jars, so a package's consumers recompile only when what they compile
//! against changes, not on every change of its bodies.
//!
//! After a package's jar is written, [`update_stamp`] stores the ABI fingerprint of its classes
//! next to it (`<name>.jar.abi`). A consumer's up-to-date check ([`crate::fingerprint`]) uses that
//! fingerprint instead of the jar's modification time.
//!
//! The ABI of a class is what other code can see: its name, flags, supertypes, generic signature
//! and annotations (which include the Kotlin metadata), its inner classes, and every field and
//! method that is not private, with their signatures, annotations and constant values. Method
//! bodies are left out, with two exceptions that consumers compile into their own classes:
//!
//! - **Kotlin `inline` functions**, found from the `kotlin.Metadata` annotation; when it can't be
//!   read, every non-private body of the class counts.
//! - **Anonymous classes of an inline function**, which the compiler copies into callers.
//!
//! Constant-pool references in a body are hashed by what they point to, so an unrelated change
//! that renumbers the pool does not change the fingerprint. A class Kargo can't parse counts with
//! all its bytes. Missing a real change would leave stale classes behind, so every doubt falls on
//! the side of recompiling.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_util::hash::Fingerprint;

/// Bumped when what the fingerprint covers changes, so stamps written by older Kargos don't match.
const ABI_VERSION: &str = "kargo-abi-1";

const ACC_PRIVATE: u16 = 0x0002;
const ACC_SUPER: u16 = 0x0020;

/// The ABI stamp of `jar`: `<jar>.abi`.
pub fn stamp_path(jar: &Path) -> PathBuf {
    let mut path = jar.as_os_str().to_owned();
    path.push(".abi");
    PathBuf::from(path)
}

/// Writes the ABI fingerprint of `jar` to its stamp. Call it every time the jar is written.
pub fn update_stamp(jar: &Path) -> Result<()> {
    let abi = jar_abi(jar).with_context(|| format!("failed to read the classes of {}", jar.display()))?;
    kargo_util::fs::write_string_atomic(&stamp_path(jar), &abi)
}

/// Adds a classpath entry to an up-to-date check: the ABI fingerprint of a jar whose stamp is at
/// least as new as the jar, else the entry's size and modification time.
pub fn stamp_classpath_entry(fp: &mut Fingerprint, entry: &Path) {
    let stamp = stamp_path(entry);
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(stamped), Some(written)) = (modified(&stamp), modified(entry))
        && stamped >= written
        && let Ok(abi) = std::fs::read_to_string(&stamp)
    {
        fp.str(&entry.to_string_lossy()).str("abi").str(&abi);
        return;
    }
    fp.file_stamp(entry);
}

/// The ABI fingerprint of the classes in `jar`.
pub fn jar_abi(jar: &Path) -> Result<String> {
    let file = std::fs::File::open(jar)?;
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(std::io::Error::other)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(std::io::Error::other)?.into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        entries.push((name, bytes));
    }
    Ok(abi_of(&entries))
}

/// The ABI fingerprint of the files of a jar (`(path in the jar, bytes)`).
pub fn abi_of(entries: &[(String, Vec<u8>)]) -> String {
    let mut fp = Fingerprint::new();
    fp.str(ABI_VERSION);
    let mut classes: BTreeMap<&str, (&[u8], Option<Class>)> = BTreeMap::new();
    for (name, bytes) in entries {
        if name.ends_with(".class") {
            classes.insert(name, (bytes, Class::parse(bytes).ok()));
        } else if name.ends_with(".kotlin_module") || name.ends_with(".kotlin_builtins") {
            // Maps packages to the classes holding their top-level declarations.
            fp.str(name).bytes(bytes);
        }
    }

    // The methods whose bodies callers copy, by class name (`com/example/Foo`).
    let mut copied: HashMap<String, HashSet<(String, String)>> = HashMap::new();
    for (_, class) in classes.values() {
        if let Some(class) = class {
            let bodies = Bodies::of(class);
            let methods = class
                .methods
                .iter()
                .filter(|m| m.access & ACC_PRIVATE == 0 && bodies.matter(&m.name))
                .map(|m| (m.name.clone(), m.desc.clone()))
                .collect();
            copied.insert(class.this.clone(), methods);
        }
    }
    // Anonymous and local classes are invisible to consumers, unless an inline function (or a
    // class copied with one) declares them: those are copied whole.
    let mut whole: HashSet<String> = HashSet::new();
    loop {
        let before = whole.len();
        for (_, class) in classes.values() {
            let Some(class) = class else { continue };
            if let Some((outer, method)) = &class.enclosing
                && !whole.contains(&class.this)
            {
                let copied_with = match method {
                    _ if whole.contains(outer) => true,
                    Some(method) => copied.get(outer).is_none_or(|m| m.contains(method)),
                    None => !copied.contains_key(outer),
                };
                if copied_with {
                    whole.insert(class.this.clone());
                }
            }
        }
        if whole.len() == before {
            break;
        }
    }

    for (name, (bytes, class)) in &classes {
        match class {
            None => {
                fp.str(name).str("raw").bytes(bytes);
            }
            Some(class) if whole.contains(&class.this) => {
                fp.str(name).str("raw").bytes(bytes);
            }
            Some(class) if class.enclosing.is_some() => {}
            Some(class) => {
                fp.str(name);
                let empty = HashSet::new();
                let copied = copied.get(&class.this).unwrap_or(&empty);
                if class.hash_abi(&mut fp, copied).is_err() {
                    fp.str("raw").bytes(bytes);
                }
            }
        }
    }
    fp.finish()
}

/// Which non-private method bodies of a class are part of its ABI.
enum Bodies {
    /// None: a Java class, or a Kotlin class without inline functions.
    None,
    /// The methods of these Kotlin `inline` functions (by Kotlin or JVM name).
    Named(HashSet<String>),
    /// All of them: the Kotlin metadata could not be read.
    All,
}

impl Bodies {
    fn of(class: &Class) -> Self {
        let Some(metadata) = class.annotations.iter().find(|a| a.ty == "Lkotlin/Metadata;") else { return Bodies::None };
        let kind = metadata.int("k").unwrap_or(1);
        match kind {
            // A synthetic class (a lambda, `$WhenMappings`, `$DefaultImpls`) declares no inline functions.
            3 => Bodies::None,
            // A class, a file facade or a part of a multi-file facade.
            1 | 2 | 5 => match inline_functions(metadata, kind == 1) {
                Some(names) if names.is_empty() => Bodies::None,
                Some(names) => Bodies::Named(names),
                None => Bodies::All,
            },
            _ => Bodies::All,
        }
    }

    /// Whether the body of the JVM method `name` is part of the ABI. The methods of an inline
    /// function `foo` are `foo`, `foo$default` and mangled names such as `foo-abc123` or `foo$lib`.
    fn matter(&self, name: &str) -> bool {
        match self {
            Bodies::None => false,
            Bodies::All => true,
            Bodies::Named(names) => names
                .iter()
                .any(|n| name == n || name.strip_prefix(n.as_str()).is_some_and(|rest| rest.starts_with('$') || rest.starts_with('-'))),
        }
    }
}

/// The names of the inline functions declared in Kotlin metadata, or `None` when it can't tell
/// (an encoding Kargo doesn't read, inline property accessors, a predefined string).
fn inline_functions(metadata: &Annotation, class: bool) -> Option<HashSet<String>> {
    let d1 = metadata.strings("d1")?;
    let d2 = metadata.strings("d2").unwrap_or_default();
    // Kotlin writes the protobuf bytes one per character, after a `\0` marker.
    let joined: String = d1.concat();
    let encoded = joined.strip_prefix('\u{0}')?;
    let bytes: Vec<u8> = encoded.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect::<Option<_>>()?;

    let mut input = Proto::new(&bytes);
    let table = input.delimited().ok()?;
    let strings = StringTable::parse(table, &d2).ok()?;
    // `Class` keeps functions in field 9 and properties in field 10; `Package` in 3 and 4.
    let (functions, properties) = if class { (9, 10) } else { (3, 4) };
    let mut names = HashSet::new();
    while let Some((field, wire)) = input.field().ok()? {
        if field == functions && wire == 2 {
            let function = input.delimited().ok()?;
            if let Some(found) = inline_function(function, &strings)? {
                names.extend(found);
            }
        } else if field == properties && wire == 2 {
            let property = input.delimited().ok()?;
            if has_inline_accessor(property)? {
                return None;
            }
        } else {
            input.skip(wire).ok()?;
        }
    }
    Some(names)
}

/// The Kotlin and JVM names of a `Function` message when it is inline; `None` (outer) when it can't tell.
fn inline_function(message: &[u8], strings: &StringTable) -> Option<Option<Vec<String>>> {
    const IS_INLINE: u64 = 1 << 10;
    let mut input = Proto::new(message);
    let (mut flags, mut old_flags, mut name, mut jvm_name) = (None, None, None, None);
    while let Some((field, wire)) = input.field().ok()? {
        match (field, wire) {
            (9, 0) => flags = Some(input.varint().ok()?),
            (1, 0) => old_flags = Some(input.varint().ok()?),
            (2, 0) => name = Some(input.varint().ok()?),
            // The JVM signature (`JvmProtoBuf.methodSignature`), when the JVM name differs.
            (100, 2) => {
                let mut signature = Proto::new(input.delimited().ok()?);
                while let Some((field, wire)) = signature.field().ok()? {
                    if (field, wire) == (1, 0) {
                        jvm_name = Some(signature.varint().ok()?);
                    } else {
                        signature.skip(wire).ok()?;
                    }
                }
            }
            _ => input.skip(wire).ok()?,
        }
    }
    let flags = match (flags, old_flags) {
        (Some(flags), _) => flags,
        // Metadata from before Kotlin 1.1 lays its flags out differently.
        (None, Some(_)) => return None,
        (None, None) => 6,
    };
    if flags & IS_INLINE == 0 {
        return Some(None);
    }
    let mut names = vec![strings.get(name? as usize)?];
    if let Some(jvm) = jvm_name {
        names.push(strings.get(jvm as usize)?);
    }
    Some(Some(names))
}

/// Whether a `Property` message has an inline getter or setter (`None` when it can't tell).
fn has_inline_accessor(message: &[u8]) -> Option<bool> {
    const IS_INLINE_ACCESSOR: u64 = 1 << 8;
    let mut input = Proto::new(message);
    let mut inline = false;
    while let Some((field, wire)) = input.field().ok()? {
        match (field, wire) {
            (7 | 8, 0) => inline |= input.varint().ok()? & IS_INLINE_ACCESSOR != 0,
            _ => input.skip(wire).ok()?,
        }
    }
    Some(inline)
}

/// The string table of Kotlin JVM metadata (`JvmProtoBuf.StringTableTypes` over `d2`).
struct StringTable {
    records: Vec<Record>,
    strings: Vec<String>,
}

#[derive(Clone, Default)]
struct Record {
    string: Option<String>,
    predefined: bool,
    operation: u64,
    substring: Vec<u64>,
    replace_char: Vec<u64>,
}

impl StringTable {
    fn parse(message: &[u8], strings: &[String]) -> Result<Self> {
        let mut records = Vec::new();
        let mut input = Proto::new(message);
        while let Some((field, wire)) = input.field()? {
            if (field, wire) != (1, 2) {
                input.skip(wire)?;
                continue;
            }
            let mut record = Proto::new(input.delimited()?);
            let (mut range, mut parsed) = (1, Record::default());
            while let Some((field, wire)) = record.field()? {
                match (field, wire) {
                    (1, 0) => range = record.varint()?,
                    (2, 0) => {
                        record.varint()?;
                        parsed.predefined = true;
                    }
                    (6, 2) => parsed.string = Some(String::from_utf8_lossy(record.delimited()?).into_owned()),
                    (3, 0) => parsed.operation = record.varint()?,
                    (4, _) => parsed.substring.extend(record.repeated(wire)?),
                    (5, _) => parsed.replace_char.extend(record.repeated(wire)?),
                    _ => record.skip(wire)?,
                }
            }
            if range > 1 << 20 {
                bail!("implausible string table range");
            }
            records.extend(std::iter::repeat_n(parsed, range as usize));
        }
        Ok(Self { records, strings: strings.to_vec() })
    }

    /// The string at `index`, or `None` when it is one of Kotlin's predefined strings.
    fn get(&self, index: usize) -> Option<String> {
        let record = self.records.get(index);
        let mut s = match record {
            Some(r) if r.string.is_some() => r.string.clone()?,
            Some(r) if r.predefined => return None,
            _ => self.strings.get(index)?.clone(),
        };
        if let Some(r) = record {
            if let [begin, end, ..] = r.substring[..] {
                s = s.chars().skip(begin as usize).take(end.saturating_sub(begin) as usize).collect();
            }
            if let [from, to, ..] = r.replace_char[..] {
                let (from, to) = (char::from_u32(from as u32)?, char::from_u32(to as u32)?);
                s = s.replace(from, &to.to_string());
            }
            match r.operation {
                0 => {}
                1 => s = s.replace('$', "."),
                2 => {
                    let mut chars = s.chars();
                    chars.next();
                    chars.next_back();
                    s = chars.as_str().replace('$', ".");
                }
                _ => return None,
            }
        }
        Some(s)
    }
}

/// A protobuf reader.
struct Proto<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Proto<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.data.get(self.pos).context("truncated varint")?;
            self.pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("varint too long")
    }

    /// The next field number and wire type, or `None` at the end.
    fn field(&mut self) -> Result<Option<(u64, u8)>> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        Ok(Some((key >> 3, (key & 7) as u8)))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).context("truncated message")?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn delimited(&mut self) -> Result<&'a [u8]> {
        let len = usize::try_from(self.varint()?)?;
        self.take(len)
    }

    /// A repeated varint field, packed (wire type 2) or not (0).
    fn repeated(&mut self, wire: u8) -> Result<Vec<u64>> {
        match wire {
            0 => Ok(vec![self.varint()?]),
            2 => {
                let mut packed = Proto::new(self.delimited()?);
                let mut out = Vec::new();
                while packed.pos < packed.data.len() {
                    out.push(packed.varint()?);
                }
                Ok(out)
            }
            _ => bail!("unexpected wire type {wire}"),
        }
    }

    fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => self.varint().map(drop),
            1 => self.take(8).map(drop),
            2 => self.delimited().map(drop),
            5 => self.take(4).map(drop),
            _ => bail!("unsupported wire type {wire}"),
        }
    }
}

/// A big-endian reader over class file bytes.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).context("truncated class file")?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u1(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u2(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u4(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }
}

/// A constant pool entry.
#[derive(Debug, Clone)]
enum Constant {
    /// The unusable slot after a long or double (and slot 0).
    Empty,
    Utf8(String),
    Integer(i32),
    Float(u32),
    Long(i64),
    Double(u64),
    Class(u16),
    String(u16),
    /// A field, method or interface method reference: tag, class, name and type.
    Member(u8, u16, u16),
    NameAndType(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    /// `Dynamic` or `InvokeDynamic`: tag, bootstrap method, name and type.
    Dynamic(u8, u16, u16),
    Module(u16),
    Package(u16),
}

struct Member<'a> {
    access: u16,
    name: String,
    desc: String,
    attributes: Vec<(String, &'a [u8])>,
}

struct Annotation {
    ty: String,
    pairs: Vec<(String, Element)>,
}

impl Annotation {
    fn get(&self, name: &str) -> Option<&Element> {
        self.pairs.iter().find(|(n, _)| n == name).map(|(_, e)| e)
    }

    fn int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            Element::Const(b'I', value) => value.parse().ok(),
            _ => None,
        }
    }

    fn strings(&self, name: &str) -> Option<Vec<String>> {
        match self.get(name)? {
            Element::Array(items) => items
                .iter()
                .map(|e| match e {
                    Element::Const(b's', s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    }
}

/// An annotation element value. The ABI hashes it through `Debug`, which the dead-code lint ignores.
#[allow(dead_code)]
#[derive(Debug)]
enum Element {
    Const(u8, String),
    Enum(String, String),
    Class(String),
    Annotation(String, Vec<(String, Element)>),
    Array(Vec<Element>),
}

/// The parts of a class file the ABI covers.
struct Class<'a> {
    constants: Vec<Constant>,
    access: u16,
    this: String,
    superclass: Option<String>,
    interfaces: Vec<String>,
    fields: Vec<Member<'a>>,
    methods: Vec<Member<'a>>,
    attributes: Vec<(String, &'a [u8])>,
    /// Runtime-visible annotations of the class (where `kotlin.Metadata` is).
    annotations: Vec<Annotation>,
    /// For an anonymous or local class: the enclosing class and, if any, method (name, descriptor).
    enclosing: Option<(String, Option<(String, String)>)>,
    /// `BootstrapMethods`: each method handle with its arguments.
    bootstrap: Vec<(u16, Vec<u16>)>,
}

impl<'a> Class<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.u4()? != 0xCAFE_BABE {
            bail!("not a class file");
        }
        r.u2()?;
        r.u2()?;
        let count = r.u2()?;
        let mut constants = vec![Constant::Empty];
        while constants.len() < usize::from(count) {
            let tag = r.u1()?;
            let constant = match tag {
                1 => {
                    let len = r.u2()?;
                    Constant::Utf8(modified_utf8(r.take(usize::from(len))?))
                }
                3 => Constant::Integer(r.u4()? as i32),
                4 => Constant::Float(r.u4()?),
                5 => Constant::Long((u64::from(r.u4()?) << 32 | u64::from(r.u4()?)) as i64),
                6 => Constant::Double(u64::from(r.u4()?) << 32 | u64::from(r.u4()?)),
                7 => Constant::Class(r.u2()?),
                8 => Constant::String(r.u2()?),
                9..=11 => Constant::Member(tag, r.u2()?, r.u2()?),
                12 => Constant::NameAndType(r.u2()?, r.u2()?),
                15 => Constant::MethodHandle(r.u1()?, r.u2()?),
                16 => Constant::MethodType(r.u2()?),
                17 | 18 => Constant::Dynamic(tag, r.u2()?, r.u2()?),
                19 => Constant::Module(r.u2()?),
                20 => Constant::Package(r.u2()?),
                _ => bail!("unknown constant pool tag {tag}"),
            };
            let wide = matches!(constant, Constant::Long(_) | Constant::Double(_));
            constants.push(constant);
            if wide {
                constants.push(Constant::Empty);
            }
        }
        let mut class = Class {
            constants,
            access: 0,
            this: String::new(),
            superclass: None,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
            annotations: Vec::new(),
            enclosing: None,
            bootstrap: Vec::new(),
        };
        class.access = r.u2()?;
        class.this = class.class_name(r.u2()?)?;
        let superclass = r.u2()?;
        class.superclass = if superclass == 0 { None } else { Some(class.class_name(superclass)?) };
        for _ in 0..r.u2()? {
            let interface = r.u2()?;
            class.interfaces.push(class.class_name(interface)?);
        }
        for _ in 0..r.u2()? {
            let field = class.member(&mut r)?;
            class.fields.push(field);
        }
        for _ in 0..r.u2()? {
            let method = class.member(&mut r)?;
            class.methods.push(method);
        }
        class.attributes = class.attributes(&mut r)?;
        if !r.done() {
            bail!("trailing bytes after the class file");
        }
        for (name, data) in class.attributes.clone() {
            match name.as_str() {
                "RuntimeVisibleAnnotations" => class.annotations = class.annotation_list(&mut Reader::new(data))?,
                "EnclosingMethod" => {
                    let mut r = Reader::new(data);
                    let outer = class.class_name(r.u2()?)?;
                    let method = match r.u2()? {
                        0 => None,
                        index => Some(class.name_and_type(index)?),
                    };
                    class.enclosing = Some((outer, method));
                }
                "BootstrapMethods" => {
                    let mut r = Reader::new(data);
                    for _ in 0..r.u2()? {
                        let handle = r.u2()?;
                        let args = (0..r.u2()?).map(|_| r.u2()).collect::<Result<Vec<_>>>()?;
                        class.bootstrap.push((handle, args));
                    }
                }
                _ => {}
            }
        }
        Ok(class)
    }

    fn member(&self, r: &mut Reader<'a>) -> Result<Member<'a>> {
        let access = r.u2()?;
        let name = self.utf8(r.u2()?)?.to_owned();
        let desc = self.utf8(r.u2()?)?.to_owned();
        let attributes = self.attributes(r)?;
        Ok(Member { access, name, desc, attributes })
    }

    fn attributes(&self, r: &mut Reader<'a>) -> Result<Vec<(String, &'a [u8])>> {
        let mut out = Vec::new();
        for _ in 0..r.u2()? {
            let name = self.utf8(r.u2()?)?.to_owned();
            let len = r.u4()?;
            out.push((name, r.take(len as usize)?));
        }
        Ok(out)
    }

    fn constant(&self, index: u16) -> Result<&Constant> {
        self.constants.get(usize::from(index)).with_context(|| format!("constant pool index {index} out of range"))
    }

    fn utf8(&self, index: u16) -> Result<&str> {
        match self.constant(index)? {
            Constant::Utf8(s) => Ok(s),
            other => bail!("expected a UTF-8 constant, got {other:?}"),
        }
    }

    fn class_name(&self, index: u16) -> Result<String> {
        match self.constant(index)? {
            Constant::Class(name) => Ok(self.utf8(*name)?.to_owned()),
            other => bail!("expected a class constant, got {other:?}"),
        }
    }

    fn name_and_type(&self, index: u16) -> Result<(String, String)> {
        match self.constant(index)? {
            Constant::NameAndType(name, ty) => Ok((self.utf8(*name)?.to_owned(), self.utf8(*ty)?.to_owned())),
            other => bail!("expected a name-and-type constant, got {other:?}"),
        }
    }

    /// A constant with every index it holds replaced by what it points to.
    fn render(&self, index: u16, depth: u8) -> Result<String> {
        if depth > 8 {
            bail!("constant pool nesting too deep");
        }
        let d = depth + 1;
        Ok(match self.constant(index)? {
            Constant::Empty => bail!("reference to an empty constant pool slot"),
            Constant::Utf8(s) => format!("utf8:{s}"),
            Constant::Integer(v) => format!("int:{v}"),
            Constant::Float(v) => format!("float:{v:08x}"),
            Constant::Long(v) => format!("long:{v}"),
            Constant::Double(v) => format!("double:{v:016x}"),
            Constant::Class(name) => format!("class:{}", self.utf8(*name)?),
            Constant::String(s) => format!("string:{}", self.utf8(*s)?),
            Constant::Member(tag, class, nt) => format!("ref{tag}:{}.{}", self.render(*class, d)?, self.render(*nt, d)?),
            Constant::NameAndType(name, ty) => format!("{}:{}", self.utf8(*name)?, self.utf8(*ty)?),
            Constant::MethodHandle(kind, reference) => format!("handle{kind}:{}", self.render(*reference, d)?),
            Constant::MethodType(desc) => format!("methodtype:{}", self.utf8(*desc)?),
            Constant::Dynamic(tag, bootstrap, nt) => {
                let (handle, args) = self.bootstrap.get(usize::from(*bootstrap)).context("bootstrap method index out of range")?;
                let args = args.iter().map(|a| self.render(*a, d)).collect::<Result<Vec<_>>>()?;
                format!("dynamic{tag}:{}[{}]({})", self.render(*nt, d)?, self.render(*handle, d)?, args.join(","))
            }
            Constant::Module(name) => format!("module:{}", self.utf8(*name)?),
            Constant::Package(name) => format!("package:{}", self.utf8(*name)?),
        })
    }

    fn annotation_list(&self, r: &mut Reader) -> Result<Vec<Annotation>> {
        (0..r.u2()?).map(|_| self.annotation(r)).collect()
    }

    fn annotation(&self, r: &mut Reader) -> Result<Annotation> {
        let ty = self.utf8(r.u2()?)?.to_owned();
        let mut pairs = Vec::new();
        for _ in 0..r.u2()? {
            let name = self.utf8(r.u2()?)?.to_owned();
            pairs.push((name, self.element(r)?));
        }
        Ok(Annotation { ty, pairs })
    }

    fn element(&self, r: &mut Reader) -> Result<Element> {
        let tag = r.u1()?;
        Ok(match tag {
            b's' => Element::Const(tag, self.utf8(r.u2()?)?.to_owned()),
            b'B' | b'C' | b'I' | b'S' | b'Z' => match self.constant(r.u2()?)? {
                Constant::Integer(v) => Element::Const(tag, v.to_string()),
                other => bail!("expected an integer constant, got {other:?}"),
            },
            b'D' | b'F' | b'J' => {
                let index = r.u2()?;
                Element::Const(tag, self.render(index, 0)?)
            }
            b'e' => Element::Enum(self.utf8(r.u2()?)?.to_owned(), self.utf8(r.u2()?)?.to_owned()),
            b'c' => Element::Class(self.utf8(r.u2()?)?.to_owned()),
            b'@' => {
                let a = self.annotation(r)?;
                Element::Annotation(a.ty, a.pairs)
            }
            b'[' => Element::Array((0..r.u2()?).map(|_| self.element(r)).collect::<Result<_>>()?),
            _ => bail!("unknown annotation element tag {tag}"),
        })
    }

    /// Hashes a list of annotations, leaving out the SMAP Kotlin writes for inlined code, which
    /// moves with every change to a body that calls an inline function.
    fn hash_annotations(&self, fp: &mut Fingerprint, data: &[u8]) -> Result<()> {
        for a in self.annotation_list(&mut Reader::new(data))? {
            if a.ty != "Lkotlin/jvm/internal/SourceDebugExtension;" {
                fp.str(&format!("@{}{:?}", a.ty, a.pairs));
            }
        }
        Ok(())
    }

    /// Hashes the attributes of a class, field or method that consumers can see. `body` hashes a
    /// method's `Code`; it is left out otherwise.
    fn hash_attributes(&self, fp: &mut Fingerprint, attributes: &[(String, &[u8])], body: bool) -> Result<()> {
        for (name, data) in attributes {
            let mut r = Reader::new(data);
            match name.as_str() {
                "Signature" => {
                    fp.str("signature").str(self.utf8(r.u2()?)?);
                }
                "ConstantValue" => {
                    fp.str("constant").str(&self.render(r.u2()?, 0)?);
                }
                "Exceptions" | "PermittedSubclasses" => {
                    fp.str(name);
                    for _ in 0..r.u2()? {
                        let class = r.u2()?;
                        fp.str(&self.class_name(class)?);
                    }
                }
                "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                    fp.str(name);
                    self.hash_annotations(fp, data)?;
                }
                "RuntimeVisibleParameterAnnotations" | "RuntimeInvisibleParameterAnnotations" => {
                    fp.str(name);
                    for parameter in 0..r.u1()? {
                        fp.str(&format!("parameter {parameter}"));
                        for a in self.annotation_list(&mut r)? {
                            fp.str(&format!("@{}{:?}", a.ty, a.pairs));
                        }
                    }
                }
                "AnnotationDefault" => {
                    fp.str(&format!("default {:?}", self.element(&mut r)?));
                }
                "InnerClasses" => {
                    fp.str(name);
                    for _ in 0..r.u2()? {
                        let (inner, outer, simple, access) = (r.u2()?, r.u2()?, r.u2()?, r.u2()?);
                        // Anonymous and local classes (no outer class) and private ones are not visible.
                        if outer == 0 || access & ACC_PRIVATE != 0 {
                            continue;
                        }
                        let simple = if simple == 0 { String::new() } else { self.utf8(simple)?.to_owned() };
                        fp.str(&format!("{} {} {simple} {access:x}", self.class_name(inner)?, self.class_name(outer)?));
                    }
                }
                "Record" => {
                    fp.str(name);
                    for _ in 0..r.u2()? {
                        let component = self.member_without_access(&mut r)?;
                        fp.str(&component.name).str(&component.desc);
                        self.hash_attributes(fp, &component.attributes, false)?;
                    }
                }
                "Code" if body => self.hash_code(fp, data)?,
                // Debug information, compiler bookkeeping and bodies.
                "Code"
                | "SourceFile"
                | "SourceDebugExtension"
                | "LineNumberTable"
                | "LocalVariableTable"
                | "LocalVariableTypeTable"
                | "StackMapTable"
                | "MethodParameters"
                | "EnclosingMethod"
                | "NestHost"
                | "NestMembers"
                | "BootstrapMethods"
                | "RuntimeVisibleTypeAnnotations"
                | "RuntimeInvisibleTypeAnnotations" => {}
                // `Deprecated`, `Synthetic`, `Module`...: their presence.
                other => {
                    fp.str(other);
                }
            }
        }
        Ok(())
    }

    fn member_without_access(&self, r: &mut Reader<'a>) -> Result<Member<'a>> {
        let name = self.utf8(r.u2()?)?.to_owned();
        let desc = self.utf8(r.u2()?)?.to_owned();
        let attributes = self.attributes(r)?;
        Ok(Member { access: 0, name, desc, attributes })
    }

    /// Hashes a method body instruction by instruction, with constant-pool operands resolved.
    fn hash_code(&self, fp: &mut Fingerprint, data: &[u8]) -> Result<()> {
        let mut r = Reader::new(data);
        let (max_stack, max_locals) = (r.u2()?, r.u2()?);
        fp.str(&format!("code {max_stack} {max_locals}"));
        let len = r.u4()? as usize;
        let code = r.take(len)?;
        let mut c = Reader::new(code);
        let mut out = Vec::with_capacity(len);
        while !c.done() {
            let at = c.pos;
            let op = c.u1()?;
            out.push(op);
            match op {
                0x00..=0x0f | 0x1a..=0x35 | 0x3b..=0x83 | 0x85..=0x98 | 0xac..=0xb1 | 0xbe | 0xbf | 0xc2 | 0xc3 => {}
                0x10 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => out.extend_from_slice(c.take(1)?),
                0x11 | 0x84 | 0x99..=0xa8 | 0xc6 | 0xc7 => out.extend_from_slice(c.take(2)?),
                0xc8 | 0xc9 => out.extend_from_slice(c.take(4)?),
                0x12 => {
                    let index = u16::from(c.u1()?);
                    self.push_constant(&mut out, index)?;
                }
                0x13 | 0x14 | 0xb2..=0xb8 | 0xbb | 0xbd | 0xc0 | 0xc1 => {
                    let index = c.u2()?;
                    self.push_constant(&mut out, index)?;
                }
                0xb9 => {
                    let index = c.u2()?;
                    self.push_constant(&mut out, index)?;
                    out.extend_from_slice(c.take(2)?);
                }
                0xba => {
                    let index = c.u2()?;
                    self.push_constant(&mut out, index)?;
                    c.take(2)?;
                }
                0xc5 => {
                    let index = c.u2()?;
                    self.push_constant(&mut out, index)?;
                    out.extend_from_slice(c.take(1)?);
                }
                0xc4 => {
                    let wide = c.u1()?;
                    out.push(wide);
                    out.extend_from_slice(c.take(if wide == 0x84 { 4 } else { 2 })?);
                }
                0xaa | 0xab => {
                    // Padding up to a multiple of four from the start of the code.
                    c.take((4 - (at + 1) % 4) % 4)?;
                    let default = c.take(4)?;
                    out.extend_from_slice(default);
                    if op == 0xaa {
                        let (low, high) = (c.u4()? as i32, c.u4()? as i32);
                        out.extend_from_slice(&low.to_be_bytes());
                        out.extend_from_slice(&high.to_be_bytes());
                        let count = usize::try_from(i64::from(high) - i64::from(low) + 1)?;
                        out.extend_from_slice(c.take(count.checked_mul(4).context("tableswitch too large")?)?);
                    } else {
                        let pairs = c.u4()? as usize;
                        out.extend_from_slice(&(pairs as u32).to_be_bytes());
                        out.extend_from_slice(c.take(pairs.checked_mul(8).context("lookupswitch too large")?)?);
                    }
                }
                _ => bail!("unknown opcode {op:#x}"),
            }
        }
        fp.bytes(&out);
        for _ in 0..r.u2()? {
            let (start, end, handler, catch) = (r.u2()?, r.u2()?, r.u2()?, r.u2()?);
            let catch = if catch == 0 { "finally".to_owned() } else { self.class_name(catch)? };
            fp.str(&format!("try {start} {end} {handler} {catch}"));
        }
        Ok(())
    }

    /// Appends a constant-pool operand as what it points to.
    fn push_constant(&self, out: &mut Vec<u8>, index: u16) -> Result<()> {
        out.extend_from_slice(self.render(index, 0)?.as_bytes());
        out.push(0);
        Ok(())
    }

    /// Hashes everything consumers of this class can see; `copied` are the methods whose bodies count.
    fn hash_abi(&self, fp: &mut Fingerprint, copied: &HashSet<(String, String)>) -> Result<()> {
        fp.str(&format!("{:x}", self.access & !ACC_SUPER)).str(&self.this);
        fp.str(self.superclass.as_deref().unwrap_or("-"));
        for interface in &self.interfaces {
            fp.str(interface);
        }
        self.hash_attributes(fp, &self.attributes, false)?;
        for field in visible(&self.fields) {
            fp.str("field").str(&format!("{:x}", field.access)).str(&field.name).str(&field.desc);
            self.hash_attributes(fp, &field.attributes, false)?;
        }
        for method in visible(&self.methods) {
            fp.str("method").str(&format!("{:x}", method.access)).str(&method.name).str(&method.desc);
            let body = copied.contains(&(method.name.clone(), method.desc.clone()));
            self.hash_attributes(fp, &method.attributes, body)?;
        }
        Ok(())
    }
}

/// The fields or methods that are not private, sorted, so their order in the class doesn't matter.
fn visible<'m, 'a>(members: &'m [Member<'a>]) -> Vec<&'m Member<'a>> {
    let mut members: Vec<&Member> = members.iter().filter(|m| m.access & ACC_PRIVATE == 0).collect();
    members.sort_by(|a, b| (&a.name, &a.desc).cmp(&(&b.name, &b.desc)));
    members
}

/// Decodes the modified UTF-8 of class files (`\0` is `C0 80`; characters beyond the BMP are
/// surrogate pairs).
fn modified_utf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, len) = if b & 0x80 == 0 {
            (u16::from(b), 1)
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            ((u16::from(b & 0x1F) << 6) | u16::from(bytes[i + 1] & 0x3F), 2)
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            ((u16::from(b & 0x0F) << 12) | (u16::from(bytes[i + 1] & 0x3F) << 6) | u16::from(bytes[i + 2] & 0x3F), 3)
        } else {
            (0xFFFD, 1)
        };
        units.push(unit);
        i += len;
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds class files for the tests, with a constant pool that grows as members are added.
    #[derive(Default)]
    struct ClassBuilder {
        pool: Vec<Vec<u8>>,
        fields: Vec<Vec<u8>>,
        methods: Vec<Vec<u8>>,
        attributes: Vec<Vec<u8>>,
        this: u16,
    }

    impl ClassBuilder {
        fn new(name: &str) -> Self {
            let mut b = ClassBuilder::default();
            b.this = b.class(name);
            b
        }

        fn constant(&mut self, bytes: Vec<u8>) -> u16 {
            if let Some(i) = self.pool.iter().position(|c| *c == bytes) {
                return i as u16 + 1;
            }
            self.pool.push(bytes);
            self.pool.len() as u16
        }

        fn utf8(&mut self, s: &str) -> u16 {
            let mut bytes = vec![1];
            bytes.extend_from_slice(&(s.len() as u16).to_be_bytes());
            bytes.extend_from_slice(s.as_bytes());
            self.constant(bytes)
        }

        fn class(&mut self, name: &str) -> u16 {
            let name = self.utf8(name);
            let mut bytes = vec![7];
            bytes.extend_from_slice(&name.to_be_bytes());
            self.constant(bytes)
        }

        fn string(&mut self, s: &str) -> u16 {
            let s = self.utf8(s);
            let mut bytes = vec![8];
            bytes.extend_from_slice(&s.to_be_bytes());
            self.constant(bytes)
        }

        fn attribute(&mut self, name: &str, data: &[u8]) -> Vec<u8> {
            let mut out = self.utf8(name).to_be_bytes().to_vec();
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(data);
            out
        }

        /// A method whose body loads the string constant `text` and returns.
        fn method(&mut self, access: u16, name: &str, text: &str) -> &mut Self {
            let (n, d) = (self.utf8(name), self.utf8("()V"));
            let s = self.string(text);
            let mut code = vec![0, 1, 0, 1];
            let body = [0x13, (s >> 8) as u8, s as u8, 0x57, 0xb1];
            code.extend_from_slice(&(body.len() as u32).to_be_bytes());
            code.extend_from_slice(&body);
            code.extend_from_slice(&[0, 0, 0, 0]);
            let code = self.attribute("Code", &code);
            let mut m = access.to_be_bytes().to_vec();
            m.extend_from_slice(&n.to_be_bytes());
            m.extend_from_slice(&d.to_be_bytes());
            m.extend_from_slice(&1u16.to_be_bytes());
            m.extend_from_slice(&code);
            self.methods.push(m);
            self
        }

        fn field(&mut self, access: u16, name: &str) -> &mut Self {
            let (n, d) = (self.utf8(name), self.utf8("I"));
            let mut f = access.to_be_bytes().to_vec();
            f.extend_from_slice(&n.to_be_bytes());
            f.extend_from_slice(&d.to_be_bytes());
            f.extend_from_slice(&0u16.to_be_bytes());
            self.fields.push(f);
            self
        }

        /// `@kotlin.Metadata(k = kind, d1 = ["\0" + bytes], d2 = d2)`.
        fn metadata(&mut self, kind: i32, d1: &[u8], d2: &[&str]) -> &mut Self {
            let ty = self.utf8("Lkotlin/Metadata;");
            let (k, d1_name, d2_name) = (self.utf8("k"), self.utf8("d1"), self.utf8("d2"));
            let mut int = vec![3];
            int.extend_from_slice(&kind.to_be_bytes());
            let kind = self.constant(int);
            // Modified UTF-8: one character per byte, `\0` as `C0 80`.
            let mut encoded = vec![0xC0, 0x80];
            for &b in d1 {
                if b == 0 {
                    encoded.extend_from_slice(&[0xC0, 0x80]);
                } else if b < 0x80 {
                    encoded.push(b);
                } else {
                    encoded.extend_from_slice(&[0xC0 | (b >> 6), 0x80 | (b & 0x3F)]);
                }
            }
            let mut utf8 = vec![1];
            utf8.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
            utf8.extend_from_slice(&encoded);
            let d1_value = self.constant(utf8);
            let mut a = ty.to_be_bytes().to_vec();
            a.extend_from_slice(&3u16.to_be_bytes());
            a.extend_from_slice(&k.to_be_bytes());
            a.push(b'I');
            a.extend_from_slice(&kind.to_be_bytes());
            a.extend_from_slice(&d1_name.to_be_bytes());
            a.extend_from_slice(&[b'[', 0, 1, b's']);
            a.extend_from_slice(&d1_value.to_be_bytes());
            a.extend_from_slice(&d2_name.to_be_bytes());
            a.push(b'[');
            a.extend_from_slice(&(d2.len() as u16).to_be_bytes());
            for s in d2 {
                let s = self.utf8(s);
                a.push(b's');
                a.extend_from_slice(&s.to_be_bytes());
            }
            let mut data = 1u16.to_be_bytes().to_vec();
            data.extend_from_slice(&a);
            let attribute = self.attribute("RuntimeVisibleAnnotations", &data);
            self.attributes.push(attribute);
            self
        }

        fn enclosed_by(&mut self, outer: &str, method: &str) -> &mut Self {
            let outer = self.class(outer);
            let (n, d) = (self.utf8(method), self.utf8("()V"));
            let mut nt = vec![12];
            nt.extend_from_slice(&n.to_be_bytes());
            nt.extend_from_slice(&d.to_be_bytes());
            let nt = self.constant(nt);
            let mut data = outer.to_be_bytes().to_vec();
            data.extend_from_slice(&nt.to_be_bytes());
            let attribute = self.attribute("EnclosingMethod", &data);
            self.attributes.push(attribute);
            self
        }

        fn build(&mut self) -> Vec<u8> {
            let object = self.class("java/lang/Object");
            let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61];
            out.extend_from_slice(&(self.pool.len() as u16 + 1).to_be_bytes());
            for c in &self.pool {
                out.extend_from_slice(c);
            }
            out.extend_from_slice(&0x0021u16.to_be_bytes());
            out.extend_from_slice(&self.this.to_be_bytes());
            out.extend_from_slice(&object.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            for list in [&self.fields, &self.methods, &self.attributes] {
                out.extend_from_slice(&(list.len() as u16).to_be_bytes());
                for item in list {
                    out.extend_from_slice(item);
                }
            }
            out
        }
    }

    fn abi(classes: &[(&str, Vec<u8>)]) -> String {
        abi_of(&classes.iter().map(|(n, b)| (format!("{n}.class"), b.clone())).collect::<Vec<_>>())
    }

    const PUBLIC: u16 = 0x0001;
    const PRIVATE: u16 = 0x0002;

    /// A Java-like class: public `api`, private `helper` with the given body text.
    fn core(api_body: &str, helper_body: &str) -> Vec<u8> {
        ClassBuilder::new("core/Core").method(PRIVATE, "helper", helper_body).method(PUBLIC, "api", api_body).build()
    }

    #[test]
    fn private_members_and_bodies_are_not_part_of_the_abi() {
        let base = abi(&[("core/Core", core("a", "x"))]);
        assert_eq!(base, abi(&[("core/Core", core("a", "a different private body"))]));
        assert_eq!(base, abi(&[("core/Core", core("a public body is not the ABI either", "x"))]));
        let with_private_field =
            ClassBuilder::new("core/Core").method(PRIVATE, "helper", "x").method(PUBLIC, "api", "a").field(PRIVATE, "cache").build();
        assert_eq!(base, abi(&[("core/Core", with_private_field)]));

        let with_public_field =
            ClassBuilder::new("core/Core").method(PRIVATE, "helper", "x").method(PUBLIC, "api", "a").field(PUBLIC, "count").build();
        assert_ne!(base, abi(&[("core/Core", with_public_field)]));
        let renamed = ClassBuilder::new("core/Core").method(PRIVATE, "helper", "x").method(PUBLIC, "api2", "a").build();
        assert_ne!(base, abi(&[("core/Core", renamed)]));
        // An unreadable class counts with all its bytes.
        assert_ne!(abi(&[("core/Broken", vec![1, 2, 3])]), abi(&[("core/Broken", vec![1, 2, 4])]));
    }

    /// `Package` metadata (k = 2) with one function named by d2[0], with the given flags.
    fn package_metadata(flags: u64) -> Vec<u8> {
        // StringTableTypes: one record (range 1, no string: use d2[0]).
        let table = [0x0a, 0x02, 0x08, 0x01];
        // Function { flags (9) = flags, name (2) = 0 }.
        let mut function = vec![0x48];
        let mut f = flags;
        loop {
            let byte = (f & 0x7f) as u8;
            f >>= 7;
            if f == 0 {
                function.push(byte);
                break;
            }
            function.push(byte | 0x80);
        }
        function.extend_from_slice(&[0x10, 0x00]);
        let mut bytes = vec![table.len() as u8];
        bytes.extend_from_slice(&table);
        bytes.push(0x1a); // Package.function (3), length-delimited
        bytes.push(function.len() as u8);
        bytes.extend_from_slice(&function);
        bytes
    }

    fn facade(flags: u64, body: &str, helper: &str) -> Vec<u8> {
        ClassBuilder::new("core/CoreKt")
            .metadata(2, &package_metadata(flags), &["twice"])
            .method(PUBLIC | 0x0008, "twice", body)
            .method(PUBLIC | 0x0008, "plain", body)
            .method(PRIVATE | 0x0008, "helper", helper)
            .build()
    }

    #[test]
    fn kotlin_inline_function_bodies_are_part_of_the_abi() {
        const PUBLIC_FLAGS: u64 = 6;
        const INLINE: u64 = 1 << 10;
        // Not inline: no body counts.
        let plain = abi(&[("core/CoreKt", facade(PUBLIC_FLAGS, "1", "x"))]);
        assert_eq!(plain, abi(&[("core/CoreKt", facade(PUBLIC_FLAGS, "2", "y"))]));
        // `twice` is inline: its body counts, the private helper's still doesn't.
        let inline = abi(&[("core/CoreKt", facade(PUBLIC_FLAGS | INLINE, "1", "x"))]);
        assert_eq!(inline, abi(&[("core/CoreKt", facade(PUBLIC_FLAGS | INLINE, "1", "y"))]));
        assert_ne!(inline, abi(&[("core/CoreKt", facade(PUBLIC_FLAGS | INLINE, "2", "x"))]));
        let bodies = Bodies::of(&Class::parse(&facade(PUBLIC_FLAGS | INLINE, "1", "x")).unwrap());
        assert!(bodies.matter("twice") && bodies.matter("twice$default") && bodies.matter("twice-x1y2") && !bodies.matter("plain"));

        // Unreadable metadata: every non-private body counts.
        let unreadable = || ClassBuilder::new("core/CoreKt").metadata(2, &[0xff], &[]).method(PUBLIC, "plain", "1").build();
        assert!(matches!(Bodies::of(&Class::parse(&unreadable()).unwrap()), Bodies::All));
    }

    #[test]
    fn anonymous_classes_count_only_inside_inline_functions() {
        const INLINE: u64 = 6 | 1 << 10;
        let lambda = |text: &str, method: &str| {
            ClassBuilder::new("core/CoreKt$1").enclosed_by("core/CoreKt", method).method(PUBLIC, "invoke", text).build()
        };
        let jar = |text: &str, method: &str| abi(&[("core/CoreKt", facade(INLINE, "1", "x")), ("core/CoreKt$1", lambda(text, method))]);
        // Declared in the private helper: invisible.
        assert_eq!(jar("a", "helper"), jar("b", "helper"));
        // Declared in the inline function: copied into callers.
        assert_ne!(jar("a", "twice"), jar("b", "twice"));
    }

    #[test]
    fn decodes_modified_utf8() {
        assert_eq!(modified_utf8(&[0x61, 0xC0, 0x80, 0xC3, 0xA9]), "a\u{0}é");
        assert_eq!(modified_utf8(&[0xE2, 0x82, 0xAC]), "€");
    }

    #[test]
    fn classpath_entries_use_fresh_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("core.jar");
        let entry = |fp_jar: &Path| {
            let mut fp = Fingerprint::new();
            stamp_classpath_entry(&mut fp, fp_jar);
            fp.finish()
        };
        std::fs::write(&jar, "v1").unwrap();
        let unstamped = entry(&jar);
        std::fs::write(stamp_path(&jar), "abi-1").unwrap();
        let stamped = entry(&jar);
        assert_ne!(unstamped, stamped);
        // Rewriting the stamp with the same ABI keeps the fingerprint.
        std::fs::write(stamp_path(&jar), "abi-1").unwrap();
        assert_eq!(stamped, entry(&jar));
        std::fs::write(stamp_path(&jar), "abi-2").unwrap();
        assert_ne!(stamped, entry(&jar));
    }
}
