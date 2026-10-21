//! Open documents and LSP positions (zero-based lines, columns in the negotiated encoding, UTF-16
//! by default) over byte offsets.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use serde_json::{Value, json};

/// How a `Position`'s `character` counts a line's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    /// The `positionEncoding` name of the LSP.
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16 => "utf-16",
            Encoding::Utf32 => "utf-32",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "utf-8" => Some(Encoding::Utf8),
            "utf-16" => Some(Encoding::Utf16),
            "utf-32" => Some(Encoding::Utf32),
            _ => None,
        }
    }

    /// How many units `c` takes in this encoding.
    pub fn len(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
            Encoding::Utf32 => 1,
        }
    }

    /// The encoding to use among the `general.positionEncodings` an editor offers: UTF-8, which
    /// needs no conversion, when it is there, else UTF-16, which every editor takes.
    pub fn negotiate(offered: &Value) -> Self {
        let offered: Vec<Encoding> = offered.as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(Self::parse).collect();
        if offered.contains(&Encoding::Utf8) { Encoding::Utf8 } else { Encoding::Utf16 }
    }

    /// The encoding a server's `positionEncoding` capability names (UTF-16 when it names none).
    pub fn of_capability(name: &Value) -> Self {
        name.as_str().and_then(Self::parse).unwrap_or(Encoding::Utf16)
    }
}

static ENCODING: AtomicU8 = AtomicU8::new(1);

/// The encoding the editor's positions use; UTF-16 until [`set_encoding`] says otherwise.
pub fn encoding() -> Encoding {
    match ENCODING.load(Ordering::Relaxed) {
        0 => Encoding::Utf8,
        2 => Encoding::Utf32,
        _ => Encoding::Utf16,
    }
}

/// Sets the encoding of the positions this process reads and writes.
pub fn set_encoding(encoding: Encoding) {
    ENCODING.store(
        match encoding {
            Encoding::Utf8 => 0,
            Encoding::Utf16 => 1,
            Encoding::Utf32 => 2,
        },
        Ordering::Relaxed,
    );
}

/// The byte offset of an LSP `Position` in `text`, clamped to the text.
pub fn offset(text: &str, position: &Value) -> usize {
    offset_in(text, position, encoding())
}

/// [`offset`] with the columns counted in `encoding`.
pub fn offset_in(text: &str, position: &Value, encoding: Encoding) -> usize {
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
        units += encoding.len(c);
    }
    end
}

/// The LSP `Position` of a byte offset in `text`.
pub fn position(text: &str, offset: usize) -> Value {
    position_in(text, offset, encoding())
}

/// [`position`] with the column counted in `encoding`.
pub fn position_in(text: &str, offset: usize, encoding: Encoding) -> Value {
    let offset = floor_char_boundary(text, offset.min(text.len()));
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character: usize = before[line_start..].chars().map(|c| encoding.len(c)).sum();
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
    fn counts_columns_in_the_negotiated_encoding() {
        let text = "b = \"é𝄞x\"
";
        let x = text.find('x').unwrap();
        // é is 2 UTF-8 bytes, 1 UTF-16 unit; 𝄞 is 4 bytes, 2 units, one scalar.
        for (encoding, character) in [(Encoding::Utf8, 11), (Encoding::Utf16, 8), (Encoding::Utf32, 7)] {
            let at = json!({ "line": 0, "character": character });
            assert_eq!(offset_in(text, &at, encoding), x, "{encoding:?}");
            assert_eq!(position_in(text, x, encoding), at, "{encoding:?}");
        }
        assert_eq!(Encoding::negotiate(&json!(["utf-16", "utf-8"])), Encoding::Utf8);
        assert_eq!(Encoding::negotiate(&json!(["utf-32", "utf-16"])), Encoding::Utf16);
        assert_eq!(Encoding::negotiate(&Value::Null), Encoding::Utf16);
        assert_eq!(Encoding::of_capability(&json!("utf-8")), Encoding::Utf8);
    }

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
