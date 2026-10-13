//! Open documents and LSP positions (zero-based lines, UTF-16 columns) over byte offsets.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The byte offset of an LSP `Position` in `text`, clamped to the text.
pub fn offset(text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let character = position["character"].as_u64().unwrap_or(0) as usize;
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (i, c) in text[start..end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += c.len_utf16();
    }
    end
}

/// The LSP `Position` of a byte offset in `text`.
pub fn position(text: &str, offset: usize) -> Value {
    let offset = floor_char_boundary(text, offset.min(text.len()));
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character: usize = before[line_start..].chars().map(char::len_utf16).sum();
    json!({ "line": line, "character": character })
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The LSP `Range` of a byte range in `text`.
pub fn range(text: &str, span: std::ops::Range<usize>) -> Value {
    json!({ "start": position(text, span.start), "end": position(text, span.end) })
}

/// The one `TextEdit` that turns `old` into `new`: the text between their common prefix and
/// suffix. `None` when they are equal.
pub fn edit(old: &str, new: &str) -> Option<Value> {
    if old == new {
        return None;
    }
    let mut prefix = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = old.bytes().rev().zip(new.bytes().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    Some(json!({ "range": range(old, prefix..old.len() - suffix), "newText": &new[prefix..new.len() - suffix] }))
}

/// Applies the `contentChanges` of a `textDocument/didChange` to `text`.
pub fn apply_changes(text: &mut String, changes: &[Value]) {
    for change in changes {
        let new = change["text"].as_str().unwrap_or_default();
        match change.get("range") {
            Some(range) if !range.is_null() => {
                let start = offset(text, &range["start"]);
                let end = offset(text, &range["end"]).max(start);
                text.replace_range(start..end, new);
            }
            _ => *text = new.to_owned(),
        }
    }
}

/// The file path of a `file:` URI.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    // `file:///C:/x` on Windows.
    let trimmed = match decoded.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => &decoded[1..],
        _ => &decoded[..],
    };
    Some(PathBuf::from(trimmed))
}

/// The `file:` URI of a path.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file name a URI ends with.
pub fn file_name(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_positions() {
        let text = "a = 1\nb = \"é𝄞x\"\n";
        assert_eq!(offset(text, &json!({ "line": 1, "character": 0 })), 6);
        // é is one UTF-16 unit, 𝄞 is two.
        let x = text.find('x').unwrap();
        assert_eq!(offset(text, &json!({ "line": 1, "character": 8 })), x);
        assert_eq!(position(text, x), json!({ "line": 1, "character": 8 }));
        assert_eq!(offset(text, &json!({ "line": 9, "character": 0 })), text.len());
        assert_eq!(offset(text, &json!({ "line": 0, "character": 99 })), 5);
    }

    #[test]
    fn applies_incremental_and_full_changes() {
        let mut text = "[package]\nname = \"a\"\n".to_owned();
        let change = json!({ "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 9 } }, "text": "app" });
        apply_changes(&mut text, &[change]);
        assert_eq!(text, "[package]\nname = \"app\"\n");
        apply_changes(&mut text, &[json!({ "text": "x" })]);
        assert_eq!(text, "x");
    }

    #[test]
    fn makes_minimal_edits() {
        let old = "[package]\nname = \"a\"\n";
        let new = "[package]\nname = \"a\"\nversion = \"1\"\n";
        let edit = edit(old, new).unwrap();
        assert_eq!(edit["range"], range(old, old.len()..old.len()));
        assert_eq!(edit["newText"], "version = \"1\"\n");
        let mut applied = old.to_owned();
        apply_changes(&mut applied, &[json!({ "range": edit["range"], "text": edit["newText"] })]);
        assert_eq!(applied, new);
        // Repeated text and multi-byte characters stay whole.
        let edit = super::edit("aé", "aè").unwrap();
        assert_eq!(edit["newText"], "è");
        assert_eq!(super::edit("aaa", "aa").unwrap()["newText"], "");
        assert!(super::edit("x", "x").is_none());
    }

    #[test]
    fn converts_uris() {
        let path =
            if cfg!(windows) { PathBuf::from("C:\\My Projects\\app\\kargo.toml") } else { PathBuf::from("/My Projects/app/kargo.toml") };
        let uri = path_to_uri(&path);
        assert!(uri.starts_with("file:///") && uri.contains("My%20Projects"), "{uri}");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
        assert_eq!(file_name(&uri), "kargo.toml");
        assert_eq!(uri_to_path("file:///c%3A/x").unwrap(), PathBuf::from("c:/x"));
        assert!(uri_to_path("untitled:1").is_none());
    }
}
