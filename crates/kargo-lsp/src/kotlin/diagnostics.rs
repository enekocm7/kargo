//! Diagnostics for a Kotlin file that need no resolution, so they can be reported as you type:
//! brackets and strings that never close, a declaration without a name, imports nothing uses,
//! local variables, private declarations and parameters nothing reads, and code after a jump.
//! Deprecated usages are the exception: they need the project's declarations ([`deprecated_usages`]).
//! Everything here leans towards saying nothing: the compiler's errors still come from `kargo check`
//! on save.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use serde_json::{Value, json};

use super::features::{self, Target};
use super::index::{SymId, UnitId};
use super::lexer::{Lexed, Tok, is_ident_char};
use super::resolve::Analyzer;
use super::syntax::{File, InitKind, Kind};
use crate::text;

/// How serious a finding is: the LSP `DiagnosticSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Hint = 4,
}

/// The LSP `DiagnosticTag`s a finding carries: editors grey out unnecessary code and strike
/// through deprecated names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Unnecessary = 1,
    Deprecated = 2,
}

/// One finding: the bytes it covers, how serious it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub span: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub tags: Vec<Tag>,
}

impl Problem {
    pub fn new(span: Range<usize>, severity: Severity, message: impl Into<String>) -> Self {
        Self { span, severity, message: message.into(), tags: Vec::new() }
    }

    pub fn tagged(mut self, tag: Tag) -> Self {
        self.tags.push(tag);
        self
    }

    /// The LSP diagnostic for the problem in `src`.
    pub fn to_json(&self, src: &str) -> Value {
        let mut d = json!({
            "range": text::range(src, self.span.clone()),
            "severity": self.severity as u8,
            "source": "kargo",
            "message": self.message,
        });
        if !self.tags.is_empty() {
            d["tags"] = json!(self.tags.iter().map(|t| *t as u8).collect::<Vec<u8>>());
        }
        d
    }
}

/// What the server reports beyond the syntax problems, from the editor's `initializationOptions`:
/// `unusedCode` and `deprecated`, both on unless set to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub unused: bool,
    pub deprecated: bool,
}

impl Options {
    pub fn from_json(options: &Value) -> Self {
        Self { unused: options["unusedCode"] != false, deprecated: options["deprecated"] != false }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self { unused: true, deprecated: true }
    }
}

/// The problems of a Kotlin file, in source order, that need no resolution.
pub fn problems(src: &str, file: &File, options: Options) -> Vec<Problem> {
    let mut out = syntax_errors(src, file);
    if options.unused {
        out.extend(unused_imports(src, file));
        out.extend(unused_locals(src, file));
        out.extend(unused_private(src, file));
        out.extend(unused_parameters(src, file));
        out.extend(unreachable_code(src, file));
    }
    out.sort_by_key(|p| (p.span.start, p.span.end));
    out
}

/// The LSP diagnostics of a Kotlin file.
pub fn diagnostics(src: &str, file: &File, options: Options) -> Vec<Value> {
    problems(src, file, options).iter().map(|p| p.to_json(src)).collect()
}

fn syntax_errors(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let mut out = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        let (open, name) = match t.kind {
            Tok::LParen => (true, "("),
            Tok::RParen => (false, ")"),
            Tok::LBracket => (true, "["),
            Tok::RBracket => (false, "]"),
            Tok::LBrace => (true, "{"),
            Tok::RBrace => (false, "}"),
            Tok::TemplateOpen => (true, "${"),
            // A string that ends at the line or at the end of the file has an empty closing quote.
            Tok::StrClose if t.start == t.end => {
                let start = lx.matching[i].map_or(t.start, |o| lx.tokens[o].start);
                out.push(Problem::new(start..start + 1, Severity::Error, "unterminated string literal"));
                continue;
            }
            _ => continue,
        };
        if lx.matching[i].is_some() {
            continue;
        }
        let message = if open { format!("unclosed `{name}`") } else { format!("unmatched `{name}`") };
        out.push(Problem::new(t.span(), Severity::Error, message));
    }
    for c in &lx.unterminated_comments {
        out.push(Problem::new(c.start..c.start + 2, Severity::Error, "unterminated comment"));
    }
    out.extend(nameless_declarations(src, lx));
    out
}

/// A `class`, `interface` or `typealias` keyword with no name after it.
fn nameless_declarations(src: &str, lx: &Lexed) -> Vec<Problem> {
    let mut out = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind != Tok::Ident || !matches!(&src[t.span()], "class" | "interface" | "typealias") {
            continue;
        }
        // `Foo::class` and `x.class` are not declarations.
        if i > 0 && matches!(lx.tokens[i - 1].kind, Tok::ColonColon | Tok::Dot | Tok::SafeDot) {
            continue;
        }
        if lx.tokens.get(i + 1).is_some_and(|n| n.kind == Tok::Ident) {
            continue;
        }
        out.push(Problem::new(t.span(), Severity::Error, format!("expecting a name after `{}`", &src[t.span()])));
    }
    out
}

/// Operator functions and delegate conventions, which an import brings into use without naming it.
const IMPLICIT: &[&str] = &[
    "getValue",
    "setValue",
    "provideDelegate",
    "plus",
    "minus",
    "times",
    "div",
    "rem",
    "mod",
    "rangeTo",
    "rangeUntil",
    "unaryPlus",
    "unaryMinus",
    "not",
    "inc",
    "dec",
    "get",
    "set",
    "contains",
    "invoke",
    "iterator",
    "hasNext",
    "next",
    "compareTo",
    "equals",
    "plusAssign",
    "minusAssign",
    "timesAssign",
    "divAssign",
    "remAssign",
];

/// Single-name imports whose name appears nowhere else in the file, not even in a comment (KDoc
/// links name imports). Star imports and the operator functions that are used without being named
/// are left alone.
fn unused_imports(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let in_import = |i: usize| file.imports.iter().any(|im| im.span.start <= lx.tokens[i].start && lx.tokens[i].end <= im.span.end);
    let mut used: HashMap<&str, usize> = HashMap::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if matches!(t.kind, Tok::Ident | Tok::Label | Tok::LabelRef) && !in_import(i) && !file.package_tokens.contains(&i) {
            *used.entry(lx.text(src, i)).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    for import in &file.imports {
        let Some(name) = import.name() else { continue };
        let simple = import.path.last().map(String::as_str).unwrap_or_default();
        if name.is_empty() || IMPLICIT.contains(&simple) || used.contains_key(name) {
            continue;
        }
        if lx.comments.iter().any(|c| mentions(&src[c.clone()], name)) {
            continue;
        }
        out.push(Problem::new(import.span.clone(), Severity::Warning, format!("unused import `{}`", import.fq())).tagged(Tag::Unnecessary));
    }
    out
}

/// Whether `word` occurs in `text` as a whole word.
fn mentions(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
    })
}

/// The number of identifier tokens of each name in the file.
fn name_counts<'a>(src: &'a str, lx: &Lexed) -> HashMap<&'a str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind == Tok::Ident {
            *counts.entry(lx.text(src, i)).or_default() += 1;
        }
    }
    counts
}

/// Whether a comment of the file names `name` (KDoc links name the declarations they document).
fn named_in_comments(src: &str, lx: &Lexed, name: &str) -> bool {
    lx.comments.iter().any(|c| mentions(&src[c.clone()], name))
}

/// Local `val` and `var` declarations whose name appears nowhere else in the file. Matching by
/// name rather than by resolution means a variable that shares its name with anything else is
/// never reported, which is the safe side.
fn unused_locals(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let counts = name_counts(src, lx);
    let mut out = Vec::new();
    for d in &file.decls {
        if d.kind != Kind::Variable || !d.local || d.name.is_empty() || d.name.starts_with('_') {
            continue;
        }
        if !matches!(d.init_kind, InitKind::Value | InitKind::Delegate) || counts.get(d.name.as_str()) != Some(&1) {
            continue;
        }
        if named_in_comments(src, lx, &d.name) {
            continue;
        }
        out.push(
            Problem::new(d.name_span.clone(), Severity::Warning, format!("variable `{}` is never used", d.name)).tagged(Tag::Unnecessary),
        );
    }
    out
}

/// The JVM's serialization hooks and the field that marks a serializable class: the runtime finds
/// them by name.
const SERIALIZATION: &[&str] = &["readObject", "readObjectNoData", "readResolve", "writeObject", "writeReplace", "serialVersionUID"];

/// Annotations (serialization, tests, entry points found by reflection) and modifiers whose use
/// the file doesn't show: a declaration with one of them is never reported as unused.
fn implicitly_used(d: &super::syntax::Decl) -> bool {
    d.modifiers.iter().any(|m| m.starts_with('@') || matches!(m.as_str(), "operator" | "infix" | "override" | "expect" | "actual"))
        || SERIALIZATION.contains(&d.name.as_str())
}

/// Whether `name` is a `componentN` function, which destructuring calls without naming it.
fn is_component(name: &str) -> bool {
    name.strip_prefix("component").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Private functions, properties and classes whose name appears nowhere else in the file. A
/// private declaration is visible only in its own file, so the file is all there is to search.
/// The members of a `data` class are used by its generated functions, so they are left alone.
fn unused_private(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let counts = name_counts(src, lx);
    let mut out = Vec::new();
    for d in &file.decls {
        let what = match d.kind {
            Kind::Function => "function",
            Kind::Property => "property",
            Kind::Class | Kind::Interface | Kind::Enum | Kind::Annotation | Kind::TypeAlias => "class",
            _ => continue,
        };
        if !d.has("private") || d.local || implicitly_used(d) || d.name.is_empty() || d.name.starts_with('_') {
            continue;
        }
        if d.name == "main" || is_component(&d.name) || d.parent.is_some_and(|p| file.decls[p].has("data")) {
            continue;
        }
        if counts.get(d.name.as_str()) != Some(&1) || named_in_comments(src, lx, &d.name) {
            continue;
        }
        out.push(
            Problem::new(d.name_span.clone(), Severity::Warning, format!("private {what} `{}` is never used", d.name))
                .tagged(Tag::Unnecessary),
        );
    }
    out
}

/// Parameters of private and local functions whose name appears nowhere else in the file. The
/// functions that are called by name from elsewhere (overrides, operators, annotated functions)
/// keep their parameters.
fn unused_parameters(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let counts = name_counts(src, lx);
    let mut out = Vec::new();
    for f in &file.decls {
        if f.kind != Kind::Function || !(f.has("private") || f.local) || implicitly_used(f) {
            continue;
        }
        for &p in &f.params {
            let d = &file.decls[p];
            if d.kind != Kind::Parameter || d.generated || d.name.is_empty() || d.name.starts_with('_') {
                continue;
            }
            if counts.get(d.name.as_str()) != Some(&1) || named_in_comments(src, lx, &d.name) {
                continue;
            }
            out.push(
                Problem::new(d.name_span.clone(), Severity::Warning, format!("parameter `{}` is never used", d.name))
                    .tagged(Tag::Unnecessary),
            );
        }
    }
    out
}

/// Keywords that can end a line without ending the expression on it, so a line after them goes on
/// (`else`, `try`) or the line is not an expression at all.
const KEYWORDS: &[&str] = &[
    "else",
    "do",
    "try",
    "finally",
    "if",
    "while",
    "for",
    "when",
    "catch",
    "return",
    "throw",
    "break",
    "continue",
    "in",
    "is",
    "as",
    "val",
    "var",
    "fun",
    "class",
    "object",
    "interface",
    "import",
    "package",
    "typealias",
    "override",
    "private",
    "public",
    "internal",
    "protected",
    "abstract",
    "open",
    "suspend",
    "inline",
    "operator",
    "infix",
    "data",
    "enum",
    "sealed",
    "companion",
    "lateinit",
    "const",
    "vararg",
    "out",
    "where",
];

/// Whether the token `i` can be the last token of an expression, so a statement may end there.
fn ends_expression(src: &str, lx: &Lexed, i: usize) -> bool {
    let t = lx.tokens[i];
    match t.kind {
        Tok::Ident => !KEYWORDS.contains(&lx.text(src, i)),
        Tok::LabelRef | Tok::Int | Tok::Float | Tok::Char | Tok::StrClose | Tok::RBrace | Tok::RBracket => true,
        // `if (c)` and `while (c)` are followed by their bodies, not by the statement's end.
        Tok::RParen => lx.matching[i].is_none_or(|o| {
            o == 0 || lx.tokens[o - 1].kind != Tok::Ident || !matches!(lx.text(src, o - 1), "if" | "while" | "for" | "catch")
        }),
        _ => false,
    }
}

/// Whether the token `i` can start a statement that follows a line break.
fn starts_statement(src: &str, lx: &Lexed, i: usize) -> bool {
    match lx.tokens[i].kind {
        Tok::Ident => !matches!(
            lx.text(src, i),
            "else" | "catch" | "finally" | "while" | "in" | "is" | "as" | "step" | "until" | "downTo" | "by" | "where"
        ),
        Tok::Label | Tok::StrOpen | Tok::Int | Tok::Float | Tok::Char => true,
        _ => false,
    }
}

/// The last token of the line that holds `i`.
fn line_end(lx: &Lexed, i: usize) -> usize {
    let tokens = &lx.tokens;
    let mut end = i;
    while end + 1 < tokens.len() && !tokens[end + 1].nl {
        end += 1;
    }
    end
}

/// Whether the token `i` starts a statement of its own: it follows a `{` or `;`, or a line break
/// after the end of an expression. A `return` after `if (c)` or `else` is a branch, not a statement.
fn starts_own_statement(src: &str, lx: &Lexed, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    match lx.tokens[i - 1].kind {
        Tok::LBrace | Tok::Semi => true,
        _ => lx.tokens[i].nl && ends_expression(src, lx, i - 1),
    }
}

/// Whether every bracket of the tokens `from..=to` pairs with one inside that range, so the range
/// closes no bracket it does not open.
fn brackets_within(lx: &Lexed, from: usize, to: usize) -> bool {
    (from..=to).all(|j| match lx.tokens[j].kind {
        Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::TemplateOpen => lx.matching[j].is_some_and(|c| c <= to),
        Tok::RParen | Tok::RBracket | Tok::RBrace => lx.matching[j].is_some_and(|o| o >= from),
        _ => true,
    })
}

/// Code after a `return`, `throw`, `break`, `continue` or `TODO()` that is in the same block: the
/// jump is a whole statement on its line, and the next line starts a statement. Lines that go on
/// (an operator or a dot at the start of the next line) and `if` branches without braces are left
/// alone. Only the first statement after a jump is reported.
fn unreachable_code(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let tokens = &lx.tokens;
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        if tokens[i].kind != Tok::Ident || !starts_own_statement(src, lx, i) {
            continue;
        }
        let end = match lx.text(src, i) {
            "return" | "throw" | "break" | "continue" => line_end(lx, i),
            // `TODO()` alone on its line, which returns `Nothing`.
            "TODO" if tokens.get(i + 1).is_some_and(|t| t.kind == Tok::LParen && !t.nl) => match lx.matching[i + 1] {
                Some(close) if line_end(lx, i) == close => close,
                _ => continue,
            },
            _ => continue,
        };
        let jump_alone = end == i || ends_expression(src, lx, end);
        if !jump_alone || !brackets_within(lx, i, end) {
            continue;
        }
        let next = end + 1;
        if next >= tokens.len() || !tokens[next].nl || !starts_statement(src, lx, next) {
            continue;
        }
        let last = line_end(lx, next);
        out.push(Problem::new(tokens[next].start..tokens[last].end, Severity::Warning, "unreachable code").tagged(Tag::Unnecessary));
    }
    out
}

/// The `DeprecationLevel` of a deprecated declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warning,
    Error,
    Hidden,
}

/// What a deprecated declaration says: its level, its message, and its `ReplaceWith` expression,
/// with whether that names imports too (`ReplaceWith("expr", "a.b.C")`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    pub level: Level,
    pub message: String,
    pub replace: Option<String>,
    pub replace_imports: bool,
}

/// References to declarations marked `@Deprecated`, in the project or a library: a hint with the
/// deprecated tag for `WARNING` (the default), an error for `ERROR`. `HIDDEN` declarations don't
/// resolve, so `kargo check` reports those. A name is marked only when every declaration it may
/// resolve to is deprecated. Declarations of the names themselves and imports are not usages.
pub fn deprecated_usages(an: &Analyzer, u: UnitId) -> Vec<Problem> {
    let file = an.file(u);
    let lx = &file.lexed;
    let src = &an.unit(u).text;
    let declared: HashSet<usize> = file.decls.iter().map(|d| d.name_span.start).collect();
    let mut out = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind != Tok::Ident || declared.contains(&t.start) || file.package_tokens.contains(&i) {
            continue;
        }
        if file.imports.iter().any(|im| im.span.contains(&t.start)) {
            continue;
        }
        let Some((_, first)) = deprecated_target(an, u, i) else { continue };
        let severity = if first.level == Level::Error { Severity::Error } else { Severity::Hint };
        out.push(Problem::new(t.span(), severity, deprecated_message(lx.text(src, i), &first)).tagged(Tag::Deprecated));
    }
    out
}

/// The declaration the identifier at token `i` resolves to, with what it says, when every
/// declaration the name may resolve to is deprecated and none is hidden. The first one's
/// deprecation stands for the name.
pub fn deprecated_target(an: &Analyzer, u: UnitId, i: usize) -> Option<(SymId, Deprecation)> {
    let all = features::targets_at(an, u, i)
        .iter()
        .map(|target| match target {
            Target::Sym(m) => deprecation(an, m.sym).map(|d| (m.sym, d)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if all.iter().any(|(_, d)| d.level == Level::Hidden) {
        return None;
    }
    all.into_iter().next()
}

/// What a declaration's `@Deprecated` says, or `None` when it isn't deprecated. Its annotations
/// come before its name, so the text between the start of the declaration and the name holds
/// the arguments; declarations without readable text are taken as deprecated with no details.
fn deprecation(an: &Analyzer, s: SymId) -> Option<Deprecation> {
    let d = an.decl(s);
    if !d.has("@Deprecated") {
        return None;
    }
    let text = &an.unit(s.unit_id()).text;
    let head = text.get(d.span.start..d.name_span.start).unwrap_or_default();
    let args = head.find("@Deprecated").map_or("", |at| argument_list(&head[at + "@Deprecated".len()..]));
    // Only the code outside string literals names the level.
    let code: String = args.split('"').step_by(2).collect();
    let level = if code.contains("ERROR") {
        Level::Error
    } else if code.contains("HIDDEN") {
        Level::Hidden
    } else {
        Level::Warning
    };
    let replace_at = args.find("ReplaceWith");
    let message = string_literals(replace_at.map_or(args, |at| &args[..at])).first().map(|m| m.to_string()).unwrap_or_default();
    let literals = replace_at.map(|at| string_literals(&args[at..])).unwrap_or_default();
    let replace = literals.first().map(|r| r.to_string());
    let replace_imports = literals.len() > 1;
    Some(Deprecation { level, message, replace, replace_imports })
}

fn deprecated_message(name: &str, d: &Deprecation) -> String {
    let mut message = format!("`{name}` is deprecated");
    let note = d.message.trim().trim_end_matches('.');
    if !note.is_empty() {
        message = format!("{message}: {note}");
    }
    if let Some(replace) = &d.replace {
        message = format!("{message}. Use `{replace}` instead");
    }
    message
}

/// The text inside the parentheses that `s` starts with, or "" when it doesn't start with one.
fn argument_list(s: &str) -> &str {
    if !s.starts_with('(') {
        return "";
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for (i, c) in s.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &s[1..i];
                }
            }
            _ => {}
        }
    }
    &s[1..]
}

/// The contents of the string literals in `s`, without their quotes.
fn string_literals(s: &str) -> Vec<&str> {
    s.split('"').skip(1).step_by(2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin::syntax;

    fn messages(src: &str) -> Vec<String> {
        let file = syntax::parse(src);
        problems(src, &file, Options::default()).into_iter().map(|p| format!("{}: {}", &src[p.span.clone()], p.message)).collect()
    }

    #[test]
    fn clean_code_has_no_problems() {
        let src = "package a\n\nimport kotlin.math.max\nimport java.util.*\n\n/** See [max]. */\nfun f(a: Int): String {\n    val b = max(a, 1)\n    return \"x ${b} $a\"\n}\n";
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn reports_unbalanced_brackets() {
        assert_eq!(messages("fun f() {\n    g(1, 2\n}\n"), ["(: unclosed `(`"]);
        assert_eq!(messages("fun f() {\n    g(1))\n}\n"), ["): unmatched `)`"]);
        assert_eq!(messages("fun f() {\n    g(1)\n"), ["{: unclosed `{`"]);
    }

    #[test]
    fn reports_unterminated_strings_and_comments() {
        assert_eq!(messages("val s = \"abc\nval t = 1\n"), ["\": unterminated string literal"]);
        assert_eq!(messages("val s = \"\"\"abc\n"), ["\": unterminated string literal"]);
        assert_eq!(messages("val a = 1 /* x\nval b = 2"), ["/*: unterminated comment"]);
    }

    #[test]
    fn reports_declarations_without_a_name() {
        assert_eq!(messages("class {\n}\n"), ["class: expecting a name after `class`"]);
        assert_eq!(messages("fun f() = String::class.java\nclass A\nfun interface B { fun x() }\n"), Vec::<String>::new());
    }

    #[test]
    fn reports_unused_imports_but_not_star_aliased_or_documented_ones() {
        let src = "import a.b.Used\nimport a.b.Unused\nimport a.b.Alias as Other\nimport a.b.*\nimport a.b.Doc\nimport a.b.getValue\n\n/** [Doc] */\nfun f(x: Used) = Other()\n";
        assert_eq!(messages(src), ["import a.b.Unused: unused import `a.b.Unused`"]);
    }

    #[test]
    fn reports_unused_local_variables() {
        let src = "fun f(p: Int) {\n    val a = 1\n    val b = 2\n    val _c = 3\n    for (i in 1..2) {}\n    println(b)\n}\n";
        assert_eq!(messages(src), ["a: variable `a` is never used"]);
    }

    #[test]
    fn reports_private_declarations_nothing_in_the_file_names() {
        let src = "private fun helper() = 1\nprivate val used = 2\nprivate class Gone\nfun f() = used\nprivate fun main() {}\n";
        assert_eq!(messages(src), ["helper: private function `helper` is never used", "Gone: private class `Gone` is never used"]);
        // Annotated, operator and override declarations are used by name the file doesn't show.
        let src = "@Test\nprivate fun t() {}\nprivate operator fun plus(o: Int) = 0\nprivate fun component1() = 0\n";
        assert_eq!(messages(src), Vec::<String>::new());
        // A name in a string template is a use.
        let src = "private val a = 1\nprivate val b = 2\nfun f() = \"$a ${b}\"\n";
        assert_eq!(messages(src), Vec::<String>::new());
    }

    #[test]
    fn reports_parameters_of_private_functions_only() {
        let src = "private fun g(a: Int, b: Int) = a\nfun h(c: Int) = g(c, 1)\nfun k(d: Int) = run { val _e = d }\n";
        assert_eq!(messages(src), ["b: parameter `b` is never used"]);
    }

    #[test]
    fn reports_code_after_a_jump_in_the_same_block() {
        assert_eq!(messages("fun f(): Int {\n    return 1\n    println(\"x\")\n}\n"), ["println(\"x\"): unreachable code"]);
        assert_eq!(messages("fun f() {\n    TODO()\n    g()\n}\n"), ["g(): unreachable code"]);
        assert_eq!(messages("fun f() {\n    while (true) {\n        break\n        g()\n    }\n}\n"), ["g(): unreachable code"]);
        // Branches without braces, closing braces and continued lines are reachable.
        assert_eq!(messages("fun f(c: Boolean) {\n    if (c) return\n    println(1)\n}\n"), Vec::<String>::new());
        assert_eq!(messages("fun f(c: Boolean) {\n    if (c) { return }\n    println(1)\n}\n"), Vec::<String>::new());
        assert_eq!(messages("fun f(c: Boolean) {\n    if (c) {\n        return\n    }\n    println(1)\n}\n"), Vec::<String>::new());
        assert_eq!(messages("fun f(a: Int) = run {\n    return@run a\n}\n"), Vec::<String>::new());
        assert_eq!(messages("fun f(a: Int): Int {\n    return a\n        .plus(1)\n}\n"), Vec::<String>::new());
    }

    #[test]
    fn the_options_turn_the_unused_code_reports_off() {
        let src = "private fun helper() = 1\nfun f(): Int {\n    return 1\n    g()\n}\n";
        let file = syntax::parse(src);
        let options = Options::from_json(&serde_json::json!({ "unusedCode": false }));
        assert!(!options.unused && options.deprecated);
        assert_eq!(problems(src, &file, options), Vec::new());
        assert_eq!(problems(src, &file, Options::default()).len(), 2);
    }
}
