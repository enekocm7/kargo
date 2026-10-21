//! Diagnostics for a Kotlin file that need no resolution, so they can be reported as you type:
//! brackets and strings that never close, a declaration without a name, imports nothing uses
//! and local variables nothing reads. Everything here is decided from the tokens of the file
//! alone and leans towards saying nothing: the compiler's errors still come from `kargo check`
//! on save.

use std::collections::HashMap;
use std::ops::Range;

use serde_json::{Value, json};

use super::lexer::{Lexed, Tok, is_ident_char};
use super::syntax::{File, InitKind, Kind};
use crate::text;

/// How serious a finding is: the LSP `DiagnosticSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error = 1,
    Warning = 2,
}

/// One finding: the bytes it covers, how serious it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub span: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// Shown as unnecessary (greyed out) by editors that support it.
    pub unnecessary: bool,
}

impl Problem {
    pub fn new(span: Range<usize>, severity: Severity, message: impl Into<String>) -> Self {
        Self { span, severity, message: message.into(), unnecessary: false }
    }

    /// The LSP diagnostic for the problem in `src`.
    pub fn to_json(&self, src: &str) -> Value {
        let mut d = json!({
            "range": text::range(src, self.span.clone()),
            "severity": self.severity as u8,
            "source": "kargo",
            "message": self.message,
        });
        if self.unnecessary {
            d["tags"] = json!([1]);
        }
        d
    }
}

/// The problems of a Kotlin file, in source order.
pub fn problems(src: &str, file: &File) -> Vec<Problem> {
    let mut out = syntax_errors(src, file);
    out.extend(unused_imports(src, file));
    out.extend(unused_locals(src, file));
    out.sort_by_key(|p| (p.span.start, p.span.end));
    out
}

/// The LSP diagnostics of a Kotlin file.
pub fn diagnostics(src: &str, file: &File) -> Vec<Value> {
    problems(src, file).iter().map(|p| p.to_json(src)).collect()
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
        let mut p = Problem::new(import.span.clone(), Severity::Warning, format!("unused import `{}`", import.fq()));
        p.unnecessary = true;
        out.push(p);
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

/// Local `val` and `var` declarations whose name appears nowhere else in the file. Matching by
/// name rather than by resolution means a variable that shares its name with anything else is
/// never reported, which is the safe side.
fn unused_locals(src: &str, file: &File) -> Vec<Problem> {
    let lx = &file.lexed;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind == Tok::Ident {
            *counts.entry(lx.text(src, i)).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    for d in &file.decls {
        if d.kind != Kind::Variable || !d.local || d.name.is_empty() || d.name.starts_with('_') {
            continue;
        }
        if !matches!(d.init_kind, InitKind::Value | InitKind::Delegate) || counts.get(d.name.as_str()) != Some(&1) {
            continue;
        }
        if lx.comments.iter().any(|c| mentions(&src[c.clone()], &d.name)) {
            continue;
        }
        let mut p = Problem::new(d.name_span.clone(), Severity::Warning, format!("variable `{}` is never used", d.name));
        p.unnecessary = true;
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin::syntax;

    fn messages(src: &str) -> Vec<String> {
        let file = syntax::parse(src);
        problems(src, &file).into_iter().map(|p| format!("{}: {}", &src[p.span.clone()], p.message)).collect()
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
}
