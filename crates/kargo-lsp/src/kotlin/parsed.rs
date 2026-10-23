//! A compact binary form of a parsed [`File`], and the per-library cache built on it.
//!
//! Parsing the sources of the standard library and of the dependencies is the bulk of the start
//! of `kargo lsp`. A library's sources never change (their directory under
//! `~/.kargo/cache/lsp-sources` is named after the jar), so the parsed files are written next to
//! them once, in one blob per directory, and read back on the next start instead of being lexed
//! and parsed again. The text is still read from the sources: only the syntax tree is stored.
//!
//! A blob that is unreadable, from another version of Kargo or for text of another length is
//! ignored and written again.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::lexer::{Lexed, Tok, Token};
use super::syntax::{Decl, File, FuncType, Import, InitKind, Kind, Scope, ScopeKind, TypeParam, TypeRef, Variance};

/// Bump when the syntax tree or its parser changes in a way the version of Kargo does not show.
const FORMAT: u32 = 6;
const MAGIC: &[u8; 4] = b"KPF\0";
/// The name of the blob in a directory of library sources.
const FILE_NAME: &str = ".kargo-parsed";

const TOKS: [Tok; 30] = [
    Tok::Ident,
    Tok::Label,
    Tok::LabelRef,
    Tok::Int,
    Tok::Float,
    Tok::Char,
    Tok::StrOpen,
    Tok::Str,
    Tok::StrClose,
    Tok::TemplateOpen,
    Tok::LParen,
    Tok::RParen,
    Tok::LBracket,
    Tok::RBracket,
    Tok::LBrace,
    Tok::RBrace,
    Tok::Dot,
    Tok::SafeDot,
    Tok::ColonColon,
    Tok::Colon,
    Tok::Comma,
    Tok::Semi,
    Tok::Lt,
    Tok::Gt,
    Tok::Eq,
    Tok::Arrow,
    Tok::At,
    Tok::Question,
    Tok::BangBang,
    Tok::Op,
];

const KINDS: [Kind; 13] = [
    Kind::Class,
    Kind::Interface,
    Kind::Object,
    Kind::Companion,
    Kind::Enum,
    Kind::Annotation,
    Kind::EnumEntry,
    Kind::TypeAlias,
    Kind::Function,
    Kind::Constructor,
    Kind::Property,
    Kind::Parameter,
    Kind::Variable,
];

// ----- writing -----

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn u(&mut self, mut n: usize) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                self.out.push(byte);
                return;
            }
            self.out.push(byte | 0x80);
        }
    }

    fn variance(&mut self, v: Variance) {
        self.u(match v {
            Variance::Invariant => 0,
            Variance::Out => 1,
            Variance::In => 2,
        });
    }

    fn b(&mut self, v: bool) {
        self.out.push(v as u8);
    }

    fn s(&mut self, text: &str) {
        self.u(text.len());
        self.out.extend_from_slice(text.as_bytes());
    }

    fn range(&mut self, r: &Range<usize>) {
        self.u(r.start);
        self.u(r.end);
    }

    fn opt<T>(&mut self, v: &Option<T>, write: impl FnOnce(&mut Self, &T)) {
        match v {
            Some(v) => {
                self.b(true);
                write(self, v);
            }
            None => self.b(false),
        }
    }

    fn list<T>(&mut self, v: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.u(v.len());
        for item in v {
            write(self, item);
        }
    }

    fn strings(&mut self, v: &[String]) {
        self.list(v, |w, s| w.s(s));
    }

    fn usizes(&mut self, v: &[usize]) {
        self.list(v, |w, n| w.u(*n));
    }

    fn type_ref(&mut self, t: &TypeRef) {
        let TypeRef { name, args, nullable, non_null, projection, func, span } = t;
        self.s(name);
        self.list(args, Self::type_ref);
        self.b(*nullable);
        self.b(*non_null);
        self.variance(*projection);
        self.opt(func, |w, f| {
            let FuncType { receiver, params, names, ret, suspend } = &**f;
            w.opt(receiver, Self::type_ref);
            w.list(params, Self::type_ref);
            w.strings(names);
            w.type_ref(ret);
            w.b(*suspend);
        });
        self.range(span);
    }

    fn decl(&mut self, d: &Decl) {
        let Decl {
            kind,
            name,
            name_span,
            span,
            modifiers,
            mutable,
            hidden,
            type_params,
            receiver,
            params,
            ty,
            init,
            init_kind,
            supertypes,
            delegated,
            members,
            parent,
            local,
            has_default,
            vararg,
            doc,
            body,
            synthetic,
            generated,
            context,
            container,
        } = d;
        self.out.push(KINDS.iter().position(|k| k == kind).expect("a known kind") as u8);
        self.s(name);
        self.range(name_span);
        self.range(span);
        self.strings(modifiers);
        self.b(*mutable);
        self.b(*hidden);
        self.list(type_params, |w, p| {
            let TypeParam { name, bound, extra, variance, span } = p;
            w.s(name);
            w.opt(bound, Self::type_ref);
            w.list(extra, Self::type_ref);
            w.variance(*variance);
            w.range(span);
        });
        self.opt(receiver, Self::type_ref);
        self.usizes(params);
        self.opt(ty, Self::type_ref);
        self.opt(init, |w, n| w.u(*n));
        match init_kind {
            InitKind::Value => self.out.push(0),
            InitKind::Delegate => self.out.push(1),
            InitKind::Iterated => self.out.push(2),
            InitKind::IteratedComponent(n) => {
                self.out.push(3);
                self.u(*n);
            }
            InitKind::Component(n) => {
                self.out.push(4);
                self.u(*n);
            }
            InitKind::LambdaParam(n) => {
                self.out.push(5);
                self.u(*n);
            }
            InitKind::LambdaComponent(n, m) => {
                self.out.push(6);
                self.u(*n);
                self.u(*m);
            }
        }
        self.list(supertypes, Self::type_ref);
        self.list(delegated, |w, (n, r)| {
            w.u(*n);
            w.range(r);
        });
        self.usizes(members);
        self.opt(parent, |w, n| w.u(*n));
        self.b(*local);
        self.b(*has_default);
        self.b(*vararg);
        self.opt(doc, |w, r| w.range(r));
        self.opt(body, |w, n| w.u(*n));
        self.usizes(synthetic);
        self.b(*generated);
        self.list(context, Self::type_ref);
        self.opt(container, Self::type_ref);
    }
}

/// The bytes of `file`.
pub fn encode(file: &File) -> Vec<u8> {
    let File {
        lexed,
        package,
        package_tokens,
        imports,
        decls,
        top,
        scopes,
        import_offset,
        import_after_package,
        jvm_name,
        jvm_multifile,
        ..
    } = file;
    let mut w = Writer::default();
    let Lexed { tokens, matching, docs, comments, unterminated_comments } = lexed;
    w.list(tokens, |w, t| {
        w.out.push(TOKS.iter().position(|k| *k == t.kind).expect("a known token") as u8);
        w.u(t.start);
        w.u(t.end);
        w.b(t.nl);
    });
    w.list(matching, |w, m| w.opt(m, |w, n| w.u(*n)));
    let mut docs: Vec<(&usize, &Range<usize>)> = docs.iter().collect();
    docs.sort_by_key(|(k, _)| **k);
    w.list(&docs, |w, (k, r)| {
        w.u(**k);
        w.range(r);
    });
    w.list(comments, |w, r| w.range(r));
    w.list(unterminated_comments, |w, r| w.range(r));
    w.s(package);
    w.usizes(package_tokens);
    w.list(imports, |w, i| {
        let Import { path, star, alias, tokens, span } = i;
        w.strings(path);
        w.b(*star);
        w.opt(alias, |w, s| w.s(s));
        w.usizes(tokens);
        w.range(span);
    });
    w.list(decls, Writer::decl);
    w.usizes(top);
    w.list(scopes, |w, s| {
        let Scope { kind, range, parent, decls } = s;
        match kind {
            ScopeKind::File => w.out.push(0),
            ScopeKind::Class(d) => {
                w.out.push(1);
                w.u(*d);
            }
            ScopeKind::Function(d) => {
                w.out.push(2);
                w.u(*d);
            }
            ScopeKind::Block => w.out.push(3),
            ScopeKind::Lambda { brace, it } => {
                w.out.push(4);
                w.u(*brace);
                w.b(*it);
            }
        }
        w.range(range);
        w.opt(parent, |w, n| w.u(*n));
        w.usizes(decls);
    });
    w.u(*import_offset);
    w.b(*import_after_package);
    w.opt(jvm_name, |w, s| w.s(s));
    w.b(*jvm_multifile);
    w.out
}

// ----- reading -----

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn u(&mut self) -> Option<usize> {
        let mut n = 0usize;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            n |= ((b & 0x7f) as usize).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(n);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }

    fn variance(&mut self) -> Option<Variance> {
        Some(match self.u()? {
            0 => Variance::Invariant,
            1 => Variance::Out,
            2 => Variance::In,
            _ => return None,
        })
    }

    fn b(&mut self) -> Option<bool> {
        Some(self.byte()? != 0)
    }

    fn s(&mut self) -> Option<String> {
        let len = self.u()?;
        let bytes = self.bytes.get(self.at..self.at.checked_add(len)?)?;
        self.at += len;
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn range(&mut self) -> Option<Range<usize>> {
        Some(self.u()?..self.u()?)
    }

    fn opt<T>(&mut self, read: impl FnOnce(&mut Self) -> Option<T>) -> Option<Option<T>> {
        if self.b()? { Some(Some(read(self)?)) } else { Some(None) }
    }

    fn list<T>(&mut self, mut read: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let len = self.u()?;
        // Every item takes a byte at least.
        if len > self.bytes.len() - self.at {
            return None;
        }
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(read(self)?);
        }
        Some(out)
    }

    fn strings(&mut self) -> Option<Vec<String>> {
        self.list(Self::s)
    }

    fn usizes(&mut self) -> Option<Vec<usize>> {
        self.list(Self::u)
    }

    fn type_ref(&mut self) -> Option<TypeRef> {
        Some(TypeRef {
            name: self.s()?,
            args: self.list(Self::type_ref)?,
            nullable: self.b()?,
            non_null: self.b()?,
            projection: self.variance()?,
            func: self.opt(|r| {
                Some(Box::new(FuncType {
                    receiver: r.opt(Self::type_ref)?,
                    params: r.list(Self::type_ref)?,
                    names: r.strings()?,
                    ret: r.type_ref()?,
                    suspend: r.b()?,
                }))
            })?,
            span: self.range()?,
        })
    }

    fn decl(&mut self) -> Option<Decl> {
        let kind = *KINDS.get(self.byte()? as usize)?;
        let name = self.s()?;
        let name_span = self.range()?;
        let span = self.range()?;
        let modifiers = self.strings()?;
        let mutable = self.b()?;
        let hidden = self.b()?;
        let type_params = self.list(|r| {
            let name = r.s()?;
            let bound = r.opt(Self::type_ref)?;
            let extra = r.list(Self::type_ref)?;
            let variance = r.variance()?;
            Some(TypeParam { name, bound, extra, variance, span: r.range()? })
        })?;
        let receiver = self.opt(Self::type_ref)?;
        let params = self.usizes()?;
        let ty = self.opt(Self::type_ref)?;
        let init = self.opt(Self::u)?;
        let init_kind = match self.byte()? {
            0 => InitKind::Value,
            1 => InitKind::Delegate,
            2 => InitKind::Iterated,
            3 => InitKind::IteratedComponent(self.u()?),
            4 => InitKind::Component(self.u()?),
            5 => InitKind::LambdaParam(self.u()?),
            6 => InitKind::LambdaComponent(self.u()?, self.u()?),
            _ => return None,
        };
        Some(Decl {
            kind,
            name,
            name_span,
            span,
            modifiers,
            mutable,
            hidden,
            type_params,
            receiver,
            params,
            ty,
            init,
            init_kind,
            supertypes: self.list(Self::type_ref)?,
            delegated: self.list(|r| Some((r.u()?, r.range()?)))?,
            members: self.usizes()?,
            parent: self.opt(Self::u)?,
            local: self.b()?,
            has_default: self.b()?,
            vararg: self.b()?,
            doc: self.opt(Self::range)?,
            body: self.opt(Self::u)?,
            synthetic: self.usizes()?,
            generated: self.b()?,
            context: self.list(Self::type_ref)?,
            container: self.opt(Self::type_ref)?,
        })
    }
}

/// The file `bytes` hold, or `None` when they are not what [`encode`] wrote.
pub fn decode(bytes: &[u8]) -> Option<File> {
    let mut r = Reader { bytes, at: 0 };
    let tokens = r.list(|r| {
        let kind = *TOKS.get(r.byte()? as usize)?;
        Some(Token { kind, start: r.u()?, end: r.u()?, nl: r.b()? })
    })?;
    let matching = r.list(|r| r.opt(Reader::u))?;
    let docs: HashMap<usize, Range<usize>> = r.list(|r| Some((r.u()?, r.range()?)))?.into_iter().collect();
    let comments = r.list(Reader::range)?;
    let unterminated_comments = r.list(Reader::range)?;
    let package = r.s()?;
    let package_tokens = r.usizes()?;
    let imports =
        r.list(|r| Some(Import { path: r.strings()?, star: r.b()?, alias: r.opt(Reader::s)?, tokens: r.usizes()?, span: r.range()? }))?;
    let decls = r.list(Reader::decl)?;
    let top = r.usizes()?;
    let scopes = r.list(|r| {
        let kind = match r.byte()? {
            0 => ScopeKind::File,
            1 => ScopeKind::Class(r.u()?),
            2 => ScopeKind::Function(r.u()?),
            3 => ScopeKind::Block,
            4 => ScopeKind::Lambda { brace: r.u()?, it: r.b()? },
            _ => return None,
        };
        Some(Scope { kind, range: r.range()?, parent: r.opt(Reader::u)?, decls: r.usizes()? })
    })?;
    let import_offset = r.u()?;
    let import_after_package = r.b()?;
    let jvm_name = r.opt(Reader::s)?;
    let jvm_multifile = r.b()?;
    (r.at == bytes.len()).then_some(File {
        lexed: Lexed { tokens, matching, docs, comments, unterminated_comments },
        package,
        package_tokens,
        imports,
        decls,
        top,
        scopes,
        import_offset,
        import_after_package,
        jvm_name,
        jvm_multifile,
        ..File::default()
    })
}

// ----- the blob of a directory -----

/// The parsed files of one directory of library sources, by path relative to it, with the length
/// of the text each was parsed from.
#[derive(Default)]
pub struct Blob {
    files: HashMap<String, (usize, Vec<u8>)>,
    /// Whether it holds a file the blob on disk lacks.
    dirty: bool,
}

impl Blob {
    /// The blob stored in `dir`, or an empty one.
    pub fn read(dir: &Path) -> Blob {
        std::fs::read(dir.join(FILE_NAME)).ok().and_then(|bytes| Self::parse(&bytes)).unwrap_or_default()
    }

    fn parse(bytes: &[u8]) -> Option<Blob> {
        let header = Self::header();
        let body = bytes.strip_prefix(header.as_slice())?;
        let mut r = Reader { bytes: body, at: 0 };
        let files = r.list(|r| {
            let name = r.s()?;
            let len = r.u()?;
            let size = r.u()?;
            let data = r.bytes.get(r.at..r.at.checked_add(size)?)?.to_vec();
            r.at += size;
            Some((name, (len, data)))
        })?;
        Some(Blob { files: files.into_iter().collect(), dirty: false })
    }

    fn header() -> Vec<u8> {
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&FORMAT.to_le_bytes());
        header.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
        header.push(0);
        header
    }

    /// The parsed file `rel` of text of `len` bytes, if the blob has it.
    pub fn get(&self, rel: &str, len: usize) -> Option<File> {
        let (stored, data) = self.files.get(rel)?;
        if *stored != len {
            return None;
        }
        decode(data)
    }

    pub fn insert(&mut self, rel: String, len: usize, file: &File) {
        self.files.insert(rel, (len, encode(file)));
        self.dirty = true;
    }

    /// Writes the blob to `dir` if it gained files; a failure only costs the next start the parse.
    pub fn save(&self, dir: &Path) {
        if !self.dirty {
            return;
        }
        let mut w = Writer { out: Self::header() };
        let mut files: Vec<(&String, &(usize, Vec<u8>))> = self.files.iter().collect();
        files.sort_by_key(|(name, _)| *name);
        w.list(&files, |w, (name, (len, data))| {
            w.s(name);
            w.u(*len);
            w.u(data.len());
            w.out.extend_from_slice(data);
        });
        let target = dir.join(FILE_NAME);
        let staging = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
        if std::fs::write(&staging, &w.out).is_ok() && kargo_util::fs::rename_with_retries(&staging, &target).is_err() {
            let _ = std::fs::remove_file(&staging);
        }
    }
}

/// The path of `file` relative to `dir`, with `/` as separator.
pub fn relative(dir: &Path, file: &Path) -> Option<String> {
    let rel: PathBuf = file.strip_prefix(dir).ok()?.to_path_buf();
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin::syntax;

    #[test]
    fn tok_table_matches_the_enum() {
        for (i, tok) in TOKS.iter().enumerate() {
            assert_eq!(*tok as usize, i, "{tok:?}");
        }
        assert_eq!(Tok::Op as usize, 29, "a token kind was added: extend TOKS");
        for (i, kind) in KINDS.iter().enumerate() {
            assert_eq!(*kind as usize, i, "{kind:?}");
        }
        assert_eq!(Kind::Variable as usize, 12, "a declaration kind was added: extend KINDS");
    }

    const SOURCE: &str = "package a.b\n\nimport x.Y as Z\nimport q.*\n\n/** Doc. */\nclass C<T : Any>(val n: Int = 1) : Base<T>(), I {\n    enum class E { A, B }\n    fun <R> f(x: (Int) -> R, y: String?): R = x(n)\n    val p by lazy { it + 1 }\n}\n\nfun String.ext(): Int { for ((k, v) in m) { val z = k }; return 1 }\nfun g() = listOf(1).map { a, (b, c) -> a }\n";

    #[test]
    fn a_file_survives_the_round_trip() {
        let file = syntax::parse(SOURCE);
        let bytes = encode(&file);
        let back = decode(&bytes).expect("decodes");
        assert_eq!(encode(&back), bytes);
        assert_eq!(back.decls.len(), file.decls.len());
        assert_eq!(back.scopes.len(), file.scopes.len());
        assert_eq!(back.lexed.tokens, file.lexed.tokens);
        assert_eq!(back.lexed.docs, file.lexed.docs);
        assert_eq!(format!("{:?}", back.decls), format!("{:?}", file.decls));
        assert_eq!(format!("{:?}", back.scopes), format!("{:?}", file.scopes));
        assert_eq!(format!("{:?}", back.imports), format!("{:?}", file.imports));
        // Cut short or damaged, it is refused.
        assert!(decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(decode(&[0xff; 12]).is_none());
    }

    #[test]
    fn a_blob_is_read_back_unless_the_text_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = syntax::parse(SOURCE);
        let mut blob = Blob::read(dir.path());
        assert!(blob.get("a/B.kt", SOURCE.len()).is_none());
        blob.insert("a/B.kt".into(), SOURCE.len(), &file);
        blob.save(dir.path());
        let blob = Blob::read(dir.path());
        assert_eq!(blob.get("a/B.kt", SOURCE.len()).map(|f| f.decls.len()), Some(file.decls.len()));
        assert!(blob.get("a/B.kt", SOURCE.len() + 1).is_none());
        std::fs::write(dir.path().join(FILE_NAME), b"junk").unwrap();
        assert!(Blob::read(dir.path()).get("a/B.kt", SOURCE.len()).is_none());
    }
}
