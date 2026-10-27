//! Help while typing: on-type formatting (`textDocument/onTypeFormatting`: the indentation of a
//! new line and of a closing brace, the ` * ` of a block comment, a closing `"""`, `*/` or
//! bracket the editor did not add) and linked editing ranges (`textDocument/linkedEditingRange`:
//! a type parameter, its uses in the same signature and its KDoc tags change together).

use std::ops::Range;

use serde_json::{Value, json};

use super::lexer::Tok;
use super::syntax::{Decl, File};
use crate::text;

/// The characters after which the editor asks for on-type formatting; the first is `\n`.
pub const TRIGGERS: [&str; 6] = ["\n", "}", "\"", "{", "(", "["];

/// What one indentation level is: `tabSize` spaces, or a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indent(pub String);

impl Indent {
    /// From the `FormattingOptions` of the request.
    pub fn from_options(options: &Value) -> Self {
        if options["insertSpaces"] == false {
            return Indent("\t".to_owned());
        }
        let size = options["tabSize"].as_u64().unwrap_or(4).clamp(1, 16) as usize;
        Indent(" ".repeat(size))
    }
}

/// The edits for typing `ch`, which ends at byte `offset` of `src`.
pub fn on_type(file: &File, src: &str, offset: usize, ch: &str, indent: &Indent) -> Vec<Value> {
    let offset = offset.min(src.len());
    let edits = match ch {
        "\n" => new_line(file, src, offset, indent),
        "}" => closing_brace(file, src, offset),
        "\"" => raw_string(file, src, offset),
        "{" | "(" | "[" => bracket(file, offset, ch),
        _ => Vec::new(),
    };
    edits.into_iter().map(|(range, new_text)| json!({ "range": text::range(src, range), "newText": new_text })).collect()
}

fn line_start(src: &str, offset: usize) -> usize {
    src[..offset].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(src: &str, offset: usize) -> usize {
    src[offset..].find('\n').map_or(src.len(), |i| offset + i)
}

/// The leading whitespace of the line that starts at `start`.
fn leading(src: &str, start: usize) -> &str {
    let line = &src[start..line_end(src, start)];
    &line[..line.len() - line.trim_start().len()]
}

/// Replaces the leading whitespace of the line starting at `start` with `want`, if it differs.
fn reindent(src: &str, start: usize, want: String) -> Option<(Range<usize>, String)> {
    let have = leading(src, start);
    (have != want).then(|| (start..start + have.len(), want))
}

fn new_line(file: &File, src: &str, offset: usize, indent: &Indent) -> Vec<(Range<usize>, String)> {
    let start = line_start(src, offset);
    if start == 0 {
        return Vec::new();
    }
    if let Some(edits) = comment_line(file, src, start) {
        return edits;
    }
    // The previous line that has code, and what it ends with.
    let mut prev = line_start(src, start - 1);
    while prev > 0 && src[prev..start].trim().is_empty() {
        prev = line_start(src, prev - 1);
    }
    let base = leading(src, prev).to_owned();
    let prev_text = src[prev..line_end(src, prev)].trim_end();
    let opens = prev_text.ends_with(['{', '(', '[']) || prev_text.ends_with("->");
    let rest = src[start..line_end(src, start)].trim_start();
    let want = if opens && !rest.starts_with(['}', ')', ']']) { format!("{base}{}", indent.0) } else { base };
    reindent(src, start, want).into_iter().collect()
}

/// A new line inside a block comment: ` * ` under the comment's `*`, and its `*/` when the
/// comment was just opened and is not closed.
fn comment_line(file: &File, src: &str, start: usize) -> Option<Vec<(Range<usize>, String)>> {
    let lx = &file.lexed;
    let newline = start - 1;
    let comment = lx
        .comments
        .iter()
        .chain(lx.docs.values())
        .chain(&lx.unterminated_comments)
        .find(|c| c.start < newline && newline < c.end && src[c.start..].starts_with("/*"))?
        .clone();
    let base = leading(src, line_start(src, comment.start)).to_owned();
    let rest = src[start..line_end(src, start)].trim_start();
    let mut edits = Vec::new();
    if !rest.starts_with('*') {
        let have = leading(src, start).len();
        edits.push((start..start + have, format!("{base} * ")));
    }
    // Closed by the `*/` of a later comment, or not at all.
    let body = &src[comment.start + 2..comment.end];
    let unclosed = lx.unterminated_comments.contains(&comment) || body.contains("/*");
    let opened = src[line_start(src, newline)..newline].trim() == src[comment.start..newline].trim();
    if unclosed && opened {
        let end = line_end(src, start);
        edits.push((end..end, format!("\n{base} */")));
    }
    Some(edits)
}

/// A `}` typed first on its line goes under the line of its `{`.
fn closing_brace(file: &File, src: &str, offset: usize) -> Vec<(Range<usize>, String)> {
    let lx = &file.lexed;
    let Some(i) = lx.before(offset).filter(|&i| lx.tokens[i].kind == Tok::RBrace && lx.tokens[i].end == offset) else {
        return Vec::new();
    };
    let start = line_start(src, offset);
    if !src[start..offset - 1].trim().is_empty() {
        return Vec::new();
    }
    let Some(open) = lx.matching[i].filter(|&o| o < i) else { return Vec::new() };
    let want = leading(src, line_start(src, lx.tokens[open].start)).to_owned();
    reindent(src, start, want).into_iter().collect()
}

/// The closing `"""` of a raw string just opened, when nothing closes it.
fn raw_string(file: &File, src: &str, offset: usize) -> Vec<(Range<usize>, String)> {
    if !src[..offset].ends_with("\"\"\"") || src[..offset].ends_with("\"\"\"\"") {
        return Vec::new();
    }
    let lx = &file.lexed;
    let raw = |kind: Tok| lx.tokens.iter().filter(|t| t.kind == kind && src[t.span()].starts_with("\"\"\"")).count();
    if raw(Tok::StrOpen) > raw(Tok::StrClose) { vec![(offset..offset, "\"\"\"".to_owned())] } else { Vec::new() }
}

/// The closing bracket of one just typed that nothing closes.
fn bracket(file: &File, offset: usize, ch: &str) -> Vec<(Range<usize>, String)> {
    let lx = &file.lexed;
    let kind = match ch {
        "{" => Tok::LBrace,
        "(" => Tok::LParen,
        _ => Tok::LBracket,
    };
    let Some(i) = lx.before(offset).filter(|&i| lx.tokens[i].kind == kind && lx.tokens[i].end == offset) else {
        return Vec::new();
    };
    if lx.matching[i].is_some() {
        return Vec::new();
    }
    let close = match ch {
        "{" => "}",
        "(" => ")",
        _ => "]",
    };
    vec![(offset..offset, close.to_owned())]
}

/// Where a declaration's signature ends: at the `{` of its body or the `=` of its value, outside
/// its parentheses.
fn signature_end(file: &File, decl: &Decl) -> usize {
    let lx = &file.lexed;
    let mut i = lx.at_or_after(decl.name_span.end);
    while let Some(t) = lx.tokens.get(i).filter(|t| t.start < decl.span.end) {
        match t.kind {
            Tok::LParen | Tok::LBracket => match lx.matching[i].filter(|&c| c > i) {
                Some(close) => i = close,
                None => return decl.span.end,
            },
            Tok::LBrace | Tok::Eq => return t.start,
            _ => {}
        }
        i += 1;
    }
    decl.span.end
}

/// The ranges that change together with the identifier at `offset`: a type parameter of a
/// declaration, its uses in that declaration's signature, and `@param` and `[name]` in its KDoc.
pub fn linked_editing(file: &File, src: &str, offset: usize) -> Value {
    let lx = &file.lexed;
    let Some(tok) = lx.ident_at(offset).filter(|&i| lx.tokens[i].kind == Tok::Ident) else { return Value::Null };
    let name = lx.text(src, tok);
    let at = lx.tokens[tok].start;
    let in_doc = |d: &Decl| d.doc.as_ref().is_some_and(|doc| doc.contains(&at));
    let decl = file
        .decls
        .iter()
        .filter(|d| d.type_params.iter().any(|p| p.name == name))
        .filter(|d| (d.span.start <= at && at < signature_end(file, d)) || in_doc(d))
        .min_by_key(|d| d.span.len());
    let Some(decl) = decl else { return Value::Null };
    let end = signature_end(file, decl);
    let mut ranges: Vec<Range<usize>> = Vec::new();
    if let Some(doc) = &decl.doc {
        ranges.extend(doc_mentions(src, doc.clone(), name));
    }
    let mut i = lx.at_or_after(decl.span.start);
    while let Some(t) = lx.tokens.get(i).filter(|t| t.start < end) {
        if t.kind == Tok::Ident && t.span() != decl.name_span && lx.text(src, i) == name && &src[t.span()] == name {
            ranges.push(t.span());
        }
        i += 1;
    }
    if !ranges.iter().any(|r| r.contains(&at)) {
        return Value::Null;
    }
    let ranges: Vec<Value> = ranges.into_iter().map(|r| text::range(src, r)).collect();
    json!({ "ranges": ranges, "wordPattern": "[A-Za-z_][A-Za-z_0-9]*" })
}

/// `@param name` and `[name]` in the KDoc at `doc`.
fn doc_mentions(src: &str, doc: Range<usize>, name: &str) -> Vec<Range<usize>> {
    let text = &src[doc.clone()];
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for (i, _) in text.match_indices(name) {
        let before = &text[..i];
        let after = &text[i + name.len()..];
        if before.ends_with(word) || after.starts_with(word) {
            continue;
        }
        let tagged = before.trim_end_matches([' ', '\t']).ends_with("@param") && before.ends_with([' ', '\t']);
        if tagged || (before.ends_with('[') && after.starts_with(']')) {
            out.push(doc.start + i..doc.start + i + name.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin::syntax::parse;

    /// `src` with the edits of typing the character before `|`.
    fn typed(src: &str, ch: &str) -> String {
        let offset = src.find('|').unwrap();
        let src = src.replacen('|', "", 1);
        let file = parse(&src);
        let edits = on_type(&file, &src, offset, ch, &Indent("    ".to_owned()));
        let mut out = src.clone();
        let mut spans: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|e| {
                let start = text::offset(&src, &e["range"]["start"]);
                let end = text::offset(&src, &e["range"]["end"]);
                (start, end, e["newText"].as_str().unwrap().to_owned())
            })
            .collect();
        // From the end; of two inserts at one place, the first one's text comes first.
        spans.reverse();
        spans.sort_by_key(|s| std::cmp::Reverse(s.0));
        for (start, end, new_text) in spans {
            out.replace_range(start..end, &new_text);
        }
        out
    }

    #[test]
    fn indents_a_new_line_in_a_block() {
        assert_eq!(typed("fun main() {\n|\n}\n", "\n"), "fun main() {\n    \n}\n");
        assert_eq!(typed("fun main() {\n    val a = 1\n|\n}\n", "\n"), "fun main() {\n    val a = 1\n    \n}\n");
        assert_eq!(typed("val f = { x ->\n|\n}\n", "\n"), "val f = { x ->\n    \n}\n");
        // Already right: nothing to do.
        assert_eq!(typed("fun main() {\n    |\n}\n", "\n"), "fun main() {\n    \n}\n");
        // A closing brace moved down by the editor stays under its line.
        assert_eq!(typed("fun main() {\n    |}\n", "\n"), "fun main() {\n}\n");
    }

    #[test]
    fn continues_and_closes_block_comments() {
        assert_eq!(typed("/**\n|\nfun main() {}\n", "\n"), "/**\n * \n */\nfun main() {}\n");
        assert_eq!(typed("    /**\n|", "\n"), "    /**\n     * \n     */");
        // In a closed comment, only the ` * ` of the new line.
        assert_eq!(typed("/**\n * Doc.\n|\n */\nfun main() {}\n", "\n"), "/**\n * Doc.\n * \n */\nfun main() {}\n");
        // The editor already continued it.
        assert_eq!(typed("/**\n * Doc.\n * |\n */\n", "\n"), "/**\n * Doc.\n * \n */\n");
    }

    #[test]
    fn dedents_a_closing_brace() {
        assert_eq!(typed("fun main() {\n    val a = 1\n    }|\n", "}"), "fun main() {\n    val a = 1\n}\n");
        assert_eq!(typed("fun main() {\n    if (a) {\n    b()\n        }|\n}\n", "}"), "fun main() {\n    if (a) {\n    b()\n    }\n}\n");
        // Not first on its line.
        assert_eq!(typed("val f = { 1 }|\n", "}"), "val f = { 1 }\n");
    }

    #[test]
    fn closes_raw_strings_and_brackets_nothing_closes() {
        assert_eq!(typed("val s = \"\"\"|\n", "\""), "val s = \"\"\"\"\"\"\n");
        assert_eq!(typed("val s = \"\"\"a\"\"\"|\n", "\""), "val s = \"\"\"a\"\"\"\n");
        assert_eq!(typed("fun main() {|\n", "{"), "fun main() {}\n");
        assert_eq!(typed("val a = listOf(|\n", "("), "val a = listOf()\n");
        // The editor closed it already.
        assert_eq!(typed("val a = listOf(|)\n", "("), "val a = listOf()\n");
        // Not in a string.
        assert_eq!(typed("val a = \"(|\"\n", "("), "val a = \"(\"\n");
    }

    #[test]
    fn links_a_type_parameter_with_its_uses_in_the_signature() {
        let src = "/**\n * Maps [T].\n * @param T the input\n */\nfun <T, R> T.map(f: (T) -> R): List<T> {\n    val t: T = this\n    return listOf(t)\n}\n";
        let file = parse(src);
        let at = src.find("<T").unwrap() + 1;
        let found = linked_editing(&file, src, at);
        let starts: Vec<usize> = found["ranges"].as_array().unwrap().iter().map(|r| text::offset(src, &r["start"])).collect();
        let expected: Vec<usize> = vec![
            src.find("[T]").unwrap() + 1,
            src.find("@param T").unwrap() + 7,
            at,
            src.find("T.map").unwrap(),
            src.find("(T)").unwrap() + 1,
            src.find("List<T>").unwrap() + 5,
        ];
        assert_eq!(starts, expected, "{found}");
        // From a use, the same ranges; not in the body, where rename takes over.
        assert_eq!(linked_editing(&file, src, src.find("(T)").unwrap() + 1), found);
        assert!(linked_editing(&file, src, src.find("t: T").unwrap() + 3).is_null());
        // Not a type parameter.
        assert!(linked_editing(&file, src, src.find("map").unwrap()).is_null());
    }
}
