//! A Kotlin tokenizer that never fails: identifiers (backquoted too), literals, string templates
//! with the code inside them, operators and brackets, with comments left out and KDoc kept on the side.
//!
//! Brackets are matched after lexing ([`Lexed::matching`]), recovering from the unbalanced ones
//! of code that is being typed.

use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    /// An identifier or keyword (soft or hard); backquoted ones keep their backquotes in the span.
    Ident,
    /// `name@`: a label.
    Label,
    /// `@name` after `return`, `break`, `continue`, `this` or `super`.
    LabelRef,
    Int,
    Float,
    Char,
    /// The opening quote(s) of a string.
    StrOpen,
    /// Literal text inside a string.
    Str,
    /// The closing quote(s) of a string.
    StrClose,
    /// `${` inside a string, closed by an [`Tok::RBrace`].
    TemplateOpen,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Dot,
    /// `?.`
    SafeDot,
    /// `::`
    ColonColon,
    Colon,
    Comma,
    Semi,
    Lt,
    Gt,
    /// `=`
    Eq,
    /// `->`
    Arrow,
    At,
    Question,
    /// `!!`
    BangBang,
    /// Any other operator.
    Op,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: Tok,
    pub start: usize,
    pub end: usize,
    /// Whether a line break comes between this token and the previous one.
    pub nl: bool,
}

impl Token {
    pub fn span(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// A lexed source file.
#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// The opening or closing bracket each bracket pairs with, by token index.
    pub matching: Vec<Option<usize>>,
    /// KDoc comments by the index of the token that follows them.
    pub docs: HashMap<usize, Range<usize>>,
    /// Comments, which get no completion.
    pub comments: Vec<Range<usize>>,
}

impl Lexed {
    /// The text of token `i`, without backquotes.
    pub fn text<'a>(&self, src: &'a str, i: usize) -> &'a str {
        let t = &self.tokens[i];
        let s = &src[t.span()];
        if t.kind == Tok::Ident && s.len() >= 2 && s.starts_with('`') && s.ends_with('`') { &s[1..s.len() - 1] } else { s }
    }

    pub fn kind(&self, i: usize) -> Option<Tok> {
        self.tokens.get(i).map(|t| t.kind)
    }

    /// Whether token `i` is the identifier or keyword `word` (not backquoted).
    pub fn is(&self, src: &str, i: usize, word: &str) -> bool {
        self.tokens.get(i).is_some_and(|t| t.kind == Tok::Ident && &src[t.span()] == word)
    }

    /// The index of the token at or after byte `offset`.
    pub fn at_or_after(&self, offset: usize) -> usize {
        self.tokens.partition_point(|t| t.end <= offset)
    }

    /// The index of the last token that ends at or before `offset`.
    pub fn before(&self, offset: usize) -> Option<usize> {
        self.tokens.partition_point(|t| t.end <= offset).checked_sub(1)
    }

    /// The identifier token whose span contains or ends at `offset`.
    pub fn ident_at(&self, offset: usize) -> Option<usize> {
        let i = self.tokens.partition_point(|t| t.end < offset);
        (i..self.tokens.len().min(i + 2)).find(|&i| {
            let t = &self.tokens[i];
            matches!(t.kind, Tok::Ident | Tok::Label | Tok::LabelRef) && t.start <= offset && offset <= t.end
        })
    }

    pub fn in_comment(&self, offset: usize) -> bool {
        self.comments.iter().any(|c| c.start < offset && offset <= c.end)
    }

    /// Whether `offset` is inside the literal text of a string (not in a template expression).
    pub fn in_string_text(&self, offset: usize) -> bool {
        let i = self.tokens.partition_point(|t| t.end < offset);
        (i.saturating_sub(1)..self.tokens.len().min(i + 2)).any(|k| {
            let t = &self.tokens[k];
            match t.kind {
                Tok::Str => t.start < offset && offset <= t.end,
                Tok::StrOpen => t.end == offset && matches!(self.kind(k + 1), Some(Tok::Str | Tok::StrClose)),
                _ => false,
            }
        })
    }
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

pub fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Kotlin's hard keywords: never names unless backquoted.
pub const HARD_KEYWORDS: &[&str] = &[
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

enum Mode {
    Code { braces: usize },
    Str { raw: bool },
}

pub fn lex(src: &str) -> Lexed {
    let mut lx = Lexer { src, bytes: src.as_bytes(), pos: 0, out: Lexed::default(), nl: false, doc: None };
    lx.run();
    let mut out = lx.out;
    out.matching = match_brackets(&out.tokens);
    out
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    out: Lexed,
    nl: bool,
    doc: Option<Range<usize>>,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> u8 {
        self.bytes.get(self.pos + ahead).copied().unwrap_or(0)
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.src.get(pos..).and_then(|s| s.chars().next())
    }

    fn push(&mut self, kind: Tok, start: usize, end: usize) {
        if let Some(doc) = self.doc.take() {
            self.out.docs.insert(self.out.tokens.len(), doc);
        }
        self.out.tokens.push(Token { kind, start, end, nl: std::mem::take(&mut self.nl) });
    }

    fn run(&mut self) {
        if self.src.starts_with("#!") {
            self.pos = self.src.find('\n').unwrap_or(self.src.len());
        }
        let mut modes = vec![Mode::Code { braces: 0 }];
        while self.pos < self.bytes.len() {
            match modes.last_mut() {
                Some(Mode::Str { raw }) => {
                    let raw = *raw;
                    match self.string_part(raw) {
                        StrEvent::Close => {
                            modes.pop();
                        }
                        StrEvent::Template => modes.push(Mode::Code { braces: 0 }),
                        StrEvent::Continue => {}
                    }
                }
                Some(Mode::Code { braces }) => {
                    let depth = *braces;
                    let in_template = modes.len() > 1;
                    let c = self.peek(0);
                    if c == b'}' && in_template && depth == 0 {
                        self.push(Tok::RBrace, self.pos, self.pos + 1);
                        self.pos += 1;
                        modes.pop();
                        continue;
                    }
                    if let Some(Mode::Code { braces }) = modes.last_mut() {
                        match c {
                            b'{' => *braces += 1,
                            b'}' => *braces = braces.saturating_sub(1),
                            _ => {}
                        }
                    }
                    if let Some(raw) = self.code() {
                        modes.push(Mode::Str { raw });
                    }
                }
                None => break,
            }
        }
    }

    /// Lexes one token of code. Returns `Some(raw)` when a string opened.
    fn code(&mut self) -> Option<bool> {
        let start = self.pos;
        let c = self.peek(0);
        match c {
            b'\n' => {
                self.nl = true;
                self.pos += 1;
            }
            b' ' | b'\t' | b'\r' | 0x0c => self.pos += 1,
            b'/' if self.peek(1) == b'/' => {
                let end = self.src[start..].find('\n').map_or(self.src.len(), |i| start + i);
                self.out.comments.push(start..end);
                self.pos = end;
            }
            b'/' if self.peek(1) == b'*' => {
                let mut depth = 0usize;
                let mut i = start;
                while i < self.bytes.len() {
                    if self.bytes[i] == b'/' && self.bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if self.bytes[i] == b'*' && self.bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                let end = i.min(self.bytes.len());
                if self.src[start..end].contains('\n') {
                    self.nl = true;
                }
                self.out.comments.push(start..end);
                let is_doc = self.src[start..].starts_with("/**") && !self.src[start..].starts_with("/**/");
                self.doc = if is_doc { Some(start..end) } else { self.doc.take() };
                self.pos = end;
            }
            b'"' => {
                let raw = self.src[start..].starts_with("\"\"\"");
                let len = if raw { 3 } else { 1 };
                self.push(Tok::StrOpen, start, start + len);
                self.pos += len;
                return Some(raw);
            }
            b'\'' => {
                let mut i = start + 1;
                while i < self.bytes.len() && self.bytes[i] != b'\'' && self.bytes[i] != b'\n' {
                    i += if self.bytes[i] == b'\\' { 2 } else { 1 };
                }
                let end = (i + 1).min(self.bytes.len());
                self.push(Tok::Char, start, end);
                self.pos = end;
            }
            b'`' => {
                let end = self.src[start + 1..].find(['`', '\n']).map_or(self.src.len(), |i| start + 1 + i + 1).min(self.src.len());
                self.push(Tok::Ident, start, end);
                self.pos = end;
                self.after_ident(start);
            }
            b'0'..=b'9' => self.number(),
            b'.' if self.peek(1).is_ascii_digit() && !self.prev_is_dot_operand() => self.number(),
            _ if c >= 0x80 || c == b'_' || c.is_ascii_alphabetic() => {
                let ch = self.char_at(start).unwrap_or(' ');
                if !is_ident_start(ch) {
                    self.pos += ch.len_utf8();
                    return None;
                }
                let mut end = start;
                for (i, ch) in self.src[start..].char_indices() {
                    if !is_ident_char(ch) {
                        break;
                    }
                    end = start + i + ch.len_utf8();
                }
                self.pos = end;
                if self.peek(0) == b'@' && !self.src[start..end].eq("this") {
                    // `loop@ for`, `label@{`: a label. `this@Outer` refers to one.
                    if !matches!(&self.src[start..end], "return" | "break" | "continue" | "super") {
                        self.push(Tok::Label, start, end);
                        self.pos = end + 1;
                        return None;
                    }
                }
                self.push(Tok::Ident, start, end);
                self.after_ident(start);
            }
            _ => self.operator(),
        }
        None
    }

    /// After `return`, `break`, `continue`, `this` or `super`, `@name` refers to a label.
    fn after_ident(&mut self, start: usize) {
        let word = &self.src[start..self.pos];
        if matches!(word, "return" | "break" | "continue" | "this" | "super") && self.peek(0) == b'@' {
            let name_start = self.pos + 1;
            if self.char_at(name_start).is_some_and(is_ident_start) {
                let mut end = name_start;
                for (i, ch) in self.src[name_start..].char_indices() {
                    if !is_ident_char(ch) {
                        break;
                    }
                    end = name_start + i + ch.len_utf8();
                }
                self.push(Tok::LabelRef, name_start, end);
                self.pos = end;
            }
        }
    }

    fn prev_is_dot_operand(&self) -> bool {
        // `1..2`, `x.0` don't happen; `a.5` would be odd. A `.` right after an operand is member access.
        self.out.tokens.last().is_some_and(|t| t.end == self.pos && matches!(t.kind, Tok::Ident | Tok::RParen | Tok::RBracket | Tok::Int))
    }

    fn number(&mut self) {
        let start = self.pos;
        let b = self.bytes;
        let mut i = start;
        let mut float = false;
        if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X' | b'b' | b'B')) {
            i += 2;
            while i < b.len() && (b[i].is_ascii_hexdigit() || b[i] == b'_') {
                i += 1;
            }
        } else {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                float = true;
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
            }
            if i < b.len() && matches!(b[i], b'e' | b'E') {
                let mut j = i + 1;
                if j < b.len() && matches!(b[j], b'+' | b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    float = true;
                    i = j;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                        i += 1;
                    }
                }
            }
        }
        while i < b.len() && matches!(b[i], b'f' | b'F' | b'L' | b'u' | b'U' | b'd' | b'D') {
            if matches!(b[i], b'f' | b'F' | b'd' | b'D') {
                float = true;
            }
            i += 1;
        }
        self.push(if float { Tok::Float } else { Tok::Int }, start, i);
        self.pos = i;
    }

    fn operator(&mut self) {
        let start = self.pos;
        let rest = &self.src[start..];
        const OPS: &[(&str, Tok)] = &[
            ("..<", Tok::Op),
            ("===", Tok::Op),
            ("!==", Tok::Op),
            ("?.", Tok::SafeDot),
            ("?:", Tok::Op),
            ("::", Tok::ColonColon),
            ("->", Tok::Arrow),
            ("!!", Tok::BangBang),
            ("..", Tok::Op),
            ("==", Tok::Op),
            ("!=", Tok::Op),
            ("<=", Tok::Op),
            (">=", Tok::Op),
            ("&&", Tok::Op),
            ("||", Tok::Op),
            ("++", Tok::Op),
            ("--", Tok::Op),
            ("+=", Tok::Op),
            ("-=", Tok::Op),
            ("*=", Tok::Op),
            ("/=", Tok::Op),
            ("%=", Tok::Op),
            ("(", Tok::LParen),
            (")", Tok::RParen),
            ("[", Tok::LBracket),
            ("]", Tok::RBracket),
            ("{", Tok::LBrace),
            ("}", Tok::RBrace),
            (".", Tok::Dot),
            (":", Tok::Colon),
            (",", Tok::Comma),
            (";", Tok::Semi),
            ("<", Tok::Lt),
            (">", Tok::Gt),
            ("=", Tok::Eq),
            ("@", Tok::At),
            ("?", Tok::Question),
        ];
        for (op, kind) in OPS {
            if rest.starts_with(op) {
                self.push(*kind, start, start + op.len());
                self.pos += op.len();
                return;
            }
        }
        let len = rest.chars().next().map_or(1, char::len_utf8);
        self.push(Tok::Op, start, start + len);
        self.pos += len;
    }

    fn string_part(&mut self, raw: bool) -> StrEvent {
        let start = self.pos;
        let b = self.bytes;
        let mut i = start;
        while i < b.len() {
            match b[i] {
                b'"' if !raw => {
                    self.flush_str(start, i);
                    self.push(Tok::StrClose, i, i + 1);
                    self.pos = i + 1;
                    return StrEvent::Close;
                }
                b'"' if raw && self.src[i..].starts_with("\"\"\"") => {
                    let run = self.src[i..].bytes().take_while(|&c| c == b'"').count();
                    let close = i + run - 3;
                    self.flush_str(start, close);
                    self.push(Tok::StrClose, close, i + run);
                    self.pos = i + run;
                    return StrEvent::Close;
                }
                b'\n' if !raw => {
                    // An unterminated string ends at the line.
                    self.flush_str(start, i);
                    self.push(Tok::StrClose, i, i);
                    self.pos = i;
                    return StrEvent::Close;
                }
                b'\\' if !raw => i += 2,
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    self.flush_str(start, i);
                    self.push(Tok::TemplateOpen, i, i + 2);
                    self.pos = i + 2;
                    return StrEvent::Template;
                }
                b'$' if self.char_at(i + 1).is_some_and(is_ident_start) => {
                    self.flush_str(start, i);
                    let name_start = i + 1;
                    let mut end = name_start;
                    for (k, ch) in self.src[name_start..].char_indices() {
                        if !is_ident_char(ch) {
                            break;
                        }
                        end = name_start + k + ch.len_utf8();
                    }
                    self.push(Tok::Ident, name_start, end);
                    self.pos = end;
                    return StrEvent::Continue;
                }
                _ => i += 1,
            }
        }
        let end = b.len();
        self.flush_str(start, end);
        self.push(Tok::StrClose, end, end);
        self.pos = end;
        StrEvent::Close
    }

    fn flush_str(&mut self, start: usize, end: usize) {
        if end > start {
            self.push(Tok::Str, start, end);
        }
    }
}

enum StrEvent {
    Close,
    Template,
    Continue,
}

fn match_brackets(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut matching = vec![None; tokens.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let opener = match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::TemplateOpen | Tok::StrOpen => {
                stack.push(i);
                continue;
            }
            Tok::RParen => &[Tok::LParen][..],
            Tok::RBracket => &[Tok::LBracket][..],
            Tok::RBrace => &[Tok::LBrace, Tok::TemplateOpen][..],
            Tok::StrClose => &[Tok::StrOpen][..],
            _ => continue,
        };
        // Recover from unbalanced brackets: close the nearest opener of the right kind.
        if let Some(pos) = stack.iter().rposition(|&o| opener.contains(&tokens[o].kind)) {
            let o = stack[pos];
            stack.truncate(pos);
            matching[o] = Some(i);
            matching[i] = Some(o);
        }
    }
    matching
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Tok, &str)> {
        let lexed = lex(src);
        lexed.tokens.iter().map(|t| (t.kind, &src[t.span()])).collect()
    }

    #[test]
    fn lexes_code_strings_and_templates() {
        let src = "val s = \"a $b ${c.d} e\" // x\n/** doc */ fun `f g`() = 1.5f + 0x1FL";
        let toks = kinds(src);
        assert_eq!(
            toks,
            [
                (Tok::Ident, "val"),
                (Tok::Ident, "s"),
                (Tok::Eq, "="),
                (Tok::StrOpen, "\""),
                (Tok::Str, "a "),
                (Tok::Ident, "b"),
                (Tok::Str, " "),
                (Tok::TemplateOpen, "${"),
                (Tok::Ident, "c"),
                (Tok::Dot, "."),
                (Tok::Ident, "d"),
                (Tok::RBrace, "}"),
                (Tok::Str, " e"),
                (Tok::StrClose, "\""),
                (Tok::Ident, "fun"),
                (Tok::Ident, "`f g`"),
                (Tok::LParen, "("),
                (Tok::RParen, ")"),
                (Tok::Eq, "="),
                (Tok::Float, "1.5f"),
                (Tok::Op, "+"),
                (Tok::Int, "0x1FL"),
            ]
        );
        let lexed = lex(src);
        assert_eq!(lexed.docs.get(&14).map(|r| &src[r.clone()]), Some("/** doc */"));
        assert!(lexed.tokens[14].nl);
        assert_eq!(lexed.text(src, 15), "f g");
        assert_eq!(lexed.matching[3], Some(13));
        assert_eq!(lexed.matching[7], Some(11));
    }

    #[test]
    fn lexes_raw_strings_labels_ranges_and_nested_comments() {
        let src = "\"\"\"a\"\"\"\" loop@ for (i in 1..2) { break@loop } /* a /* b */ c */ x?.y!!";
        let toks = kinds(src);
        assert!(toks.contains(&(Tok::StrClose, "\"\"\"")));
        assert!(toks.contains(&(Tok::Str, "a\"")));
        assert!(toks.contains(&(Tok::Label, "loop")));
        assert!(toks.contains(&(Tok::LabelRef, "loop")));
        assert!(toks.contains(&(Tok::Op, "..")));
        assert!(toks.contains(&(Tok::Int, "1")));
        assert!(toks.contains(&(Tok::SafeDot, "?.")));
        assert!(toks.contains(&(Tok::BangBang, "!!")));
        assert!(!toks.iter().any(|(_, s)| *s == "b"), "nested comments are skipped: {toks:?}");
    }

    #[test]
    fn recovers_from_unbalanced_brackets() {
        let lexed = lex("fun f() { g(1, \n }");
        let open_brace = 4;
        assert_eq!(lexed.tokens[open_brace].kind, Tok::LBrace);
        assert_eq!(lexed.matching[open_brace], Some(lexed.tokens.len() - 1));
    }

    #[test]
    fn knows_strings_and_comments() {
        let src = "val a = \"hello\" // note";
        let lexed = lex(src);
        assert!(lexed.in_string_text(10));
        assert!(!lexed.in_string_text(4));
        assert!(lexed.in_comment(20));
        assert!(!lexed.in_comment(4));
    }
}
