//! Document links (`textDocument/documentLink`): the URLs in comments, KDoc and strings, and the
//! strings that name a file that exists, such as `"resources/app.properties"`.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::lexer::Tok;
use super::syntax::File;
use crate::text;

/// The links of a Kotlin file. A string names a file relative to one of `bases`: the
/// file's directory, its package's directory and that package's `resources/`.
pub fn document_links(file: &File, src: &str, bases: &[PathBuf]) -> Value {
    let lx = &file.lexed;
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    for comment in lx.comments.iter().chain(lx.docs.values()).chain(&lx.unterminated_comments) {
        urls(src, comment.clone(), &mut links);
    }
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.kind != Tok::Str {
            continue;
        }
        let before = links.len();
        urls(src, t.span(), &mut links);
        // A string that is nothing but a path: `"…"` with no template in it.
        let whole = i > 0
            && lx.tokens[i - 1].kind == Tok::StrOpen
            && lx.tokens.get(i + 1).is_some_and(|n| n.kind == Tok::StrClose)
            && src[lx.tokens[i - 1].span()].len() == 1;
        if whole
            && links.len() == before
            && let Some(target) = existing_file(&src[t.span()], bases)
        {
            links.push((t.span(), text::path_to_uri(&target)));
        }
    }
    links.sort_by_key(|(r, _)| r.start);
    links.dedup_by(|a, b| a.0 == b.0);
    json!(links.into_iter().map(|(range, target)| json!({ "range": text::range(src, range), "target": target })).collect::<Vec<_>>())
}

/// The `http://` and `https://` URLs in `span` of `src`.
fn urls(src: &str, span: Range<usize>, out: &mut Vec<(Range<usize>, String)>) {
    let text = &src[span.clone()];
    let mut from = 0;
    while let Some(found) = ["https://", "http://"].iter().filter_map(|scheme| text[from..].find(scheme)).min() {
        let start = from + found;
        let len = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | ')' | ']' | '{' | '}' | '|' | '\\'))
            .unwrap_or(text.len() - start);
        let url = text[start..start + len].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        if url.len() > "https://".len() {
            out.push((span.start + start..span.start + start + url.len(), url.to_owned()));
        }
        from = start + len.max(1);
    }
}

/// The file a string names, if it looks like a path and exists under one of `bases`.
fn existing_file(value: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let looks_like_path = value.len() < 260
        && (value.contains('/') || value.contains('.'))
        && !value.contains(|c: char| c.is_whitespace() || c == '\\' || c == '$' || c == ':' && !Path::new(value).is_absolute());
    if !looks_like_path || value.starts_with('.') && !value.starts_with("./") && !value.starts_with("../") {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() && path.is_file() {
        return Some(path.to_path_buf());
    }
    // `getResource("/app.properties")` names a resource from the root of the classpath.
    let relative = value.trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }
    bases.iter().map(|base| base.join(relative)).find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin::syntax::parse;

    fn links(src: &str, bases: &[PathBuf]) -> Vec<(String, String)> {
        let file = parse(src);
        document_links(&file, src, bases)
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let start = text::offset(src, &l["range"]["start"]);
                let end = text::offset(src, &l["range"]["end"]);
                (src[start..end].to_owned(), l["target"].as_str().unwrap().to_owned())
            })
            .collect()
    }

    #[test]
    fn links_urls_in_comments_kdoc_and_strings() {
        let src = "// See https://kotlinlang.org/docs/home.html.\n/** Per [RFC](https://www.rfc-editor.org/rfc/rfc9110). */\nfun main() {\n    val u = \"http://localhost:8080/x\"\n    val t = \"go to https://example.com/$name now\"\n}\n";
        let found = links(src, &[]);
        let urls: Vec<&str> = found.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://kotlinlang.org/docs/home.html",
                "https://www.rfc-editor.org/rfc/rfc9110",
                "http://localhost:8080/x",
                "https://example.com/"
            ]
        );
        assert!(found.iter().all(|(text, target)| text == target));
    }

    #[test]
    fn links_strings_that_name_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("resources")).unwrap();
        std::fs::write(dir.path().join("resources/app.properties"), "a=1\n").unwrap();
        let bases = vec![dir.path().to_path_buf(), dir.path().join("resources")];
        let src = "val a = \"resources/app.properties\"\nval b = getResource(\"/app.properties\")\nval c = \"missing.properties\"\nval d = \"resources/$x.properties\"\nval e = \"hello\"\n";
        let found = links(src, &bases);
        let target = text::path_to_uri(&dir.path().join("resources/app.properties"));
        assert_eq!(found, [("resources/app.properties".to_owned(), target.clone()), ("/app.properties".to_owned(), target)]);
    }
}
