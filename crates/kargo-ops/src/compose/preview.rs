//! Compose previews: the top-level `@Preview @Composable` functions without parameters that
//! `kargo preview` renders to PNG and `kargo lsp` offers as runnables.
//!
//! They are found by reading the sources, not by compiling them, so an editor can list them while
//! the code does not build. Comments and strings are skipped; a function counts when an annotation
//! named `Preview` (any of the Compose `Preview` annotations, qualified or not) is on it at the top
//! level of its file.

use std::path::{Path, PathBuf};

/// The renderer `kargo preview` compiles and runs.
pub const RENDERER: &str = include_str!("KargoPreview.kt");
/// The main class of [`RENDERER`].
pub const RENDERER_MAIN: &str = "kargo.preview.KargoPreviewKt";

/// A preview function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The function's name, such as `AppPreview`.
    pub function: String,
    /// The Kotlin package, empty for the default one.
    pub package: String,
    /// The JVM class that holds the function: the file's facade, such as `com.example.MainKt`.
    pub class: String,
    pub file: PathBuf,
    /// 1-based line of the `fun` keyword.
    pub line: usize,
}

impl Preview {
    /// `com.example.AppPreview`: what `kargo preview` takes and the name of its PNG.
    pub fn qualified_name(&self) -> String {
        if self.package.is_empty() { self.function.clone() } else { format!("{}.{}", self.package, self.function) }
    }

    /// Whether `query` names this preview: its function name or its qualified name.
    pub fn matches(&self, query: &str) -> bool {
        query == self.function || query == self.qualified_name()
    }
}

/// The previews in `files`, in file order.
pub fn find(files: &[PathBuf]) -> Vec<Preview> {
    let mut out = Vec::new();
    for file in files.iter().filter(|f| f.extension().is_some_and(|e| e == "kt")) {
        if let Ok(text) = std::fs::read_to_string(file) {
            out.extend(find_in(file, &text));
        }
    }
    out
}

/// The previews in one Kotlin file.
pub fn find_in(file: &Path, text: &str) -> Vec<Preview> {
    let code = blank_comments_and_strings(text);
    let package = package_of(&code);
    let facade = jvm_name(text).unwrap_or_else(|| facade_name(file));
    let class = if package.is_empty() { facade } else { format!("{package}.{facade}") };
    let tokens = tokens(&code);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut annotated = false;
    let mut i = 0;
    while i < tokens.len() {
        let (token, offset) = tokens[i];
        match token {
            "{" => depth += 1,
            "}" => depth = depth.saturating_sub(1),
            "@" if depth == 0 => {
                // `@Preview`, `@androidx.compose.ui.tooling.preview.Preview(...)`: the last name of the path.
                let mut j = i + 1;
                let mut last = "";
                while j < tokens.len() && is_identifier(tokens[j].0) {
                    last = tokens[j].0;
                    if tokens.get(j + 1).is_some_and(|t| t.0 == ".") {
                        j += 2;
                    } else {
                        j += 1;
                        break;
                    }
                }
                if last == "Preview" {
                    annotated = true;
                }
                // Skip the annotation's arguments.
                if tokens.get(j).is_some_and(|t| t.0 == "(") {
                    j = skip_parens(&tokens, j);
                }
                i = j;
                continue;
            }
            "fun" if depth == 0 => {
                let name = tokens.get(i + 1).map(|t| t.0).filter(|n| is_identifier(n));
                let no_parameters = tokens.get(i + 2).is_some_and(|t| t.0 == "(") && tokens.get(i + 3).is_some_and(|t| t.0 == ")");
                if annotated && let Some(name) = name.filter(|_| no_parameters) {
                    let line = text[..offset].matches('\n').count() + 1;
                    out.push(Preview {
                        function: name.to_owned(),
                        package: package.clone(),
                        class: class.clone(),
                        file: file.to_path_buf(),
                        line,
                    });
                }
                annotated = false;
            }
            "class" | "object" | "interface" | "val" | "var" | "typealias" if depth == 0 => annotated = false,
            _ => {}
        }
        i += 1;
    }
    out
}

/// Skips from the `(` at `start` past its matching `)`.
fn skip_parens(tokens: &[(&str, usize)], start: usize) -> usize {
    let mut depth = 0;
    for (k, (t, _)) in tokens.iter().enumerate().skip(start) {
        match *t {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

fn is_identifier(token: &str) -> bool {
    token.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '`')
}

/// Identifiers and single punctuation characters, with their byte offsets.
fn tokens(code: &str) -> Vec<(&str, usize)> {
    let mut out = Vec::new();
    let bytes: Vec<(usize, char)> = code.char_indices().collect();
    let mut k = 0;
    while k < bytes.len() {
        let (start, c) = bytes[k];
        if c.is_alphanumeric() || c == '_' {
            let mut end = k;
            while end < bytes.len() && (bytes[end].1.is_alphanumeric() || bytes[end].1 == '_') {
                end += 1;
            }
            let stop = bytes.get(end).map(|b| b.0).unwrap_or(code.len());
            out.push((&code[start..stop], start));
            k = end;
        } else if c == '`' {
            let mut end = k + 1;
            while end < bytes.len() && bytes[end].1 != '`' {
                end += 1;
            }
            let stop = bytes.get(end + 1).map(|b| b.0).unwrap_or(code.len());
            out.push((&code[start..stop], start));
            k = end + 1;
        } else {
            if !c.is_whitespace() {
                out.push((&code[start..start + c.len_utf8()], start));
            }
            k += 1;
        }
    }
    out
}

/// `text` with comments and the contents of strings and characters replaced by spaces (newlines kept),
/// so offsets and line numbers stay the same.
fn blank_comments_and_strings(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut k = 0;
    let blank = |out: &mut String, c: char| out.push(if c == '\n' { '\n' } else { ' ' });
    while k < chars.len() {
        let c = chars[k];
        let next = chars.get(k + 1).copied();
        if c == '/' && next == Some('/') {
            while k < chars.len() && chars[k] != '\n' {
                blank(&mut out, chars[k]);
                k += 1;
            }
        } else if c == '/' && next == Some('*') {
            // Kotlin block comments nest.
            let mut depth = 0;
            while k < chars.len() {
                if chars[k] == '/' && chars.get(k + 1) == Some(&'*') {
                    depth += 1;
                    blank(&mut out, chars[k]);
                    blank(&mut out, chars[k + 1]);
                    k += 2;
                } else if chars[k] == '*' && chars.get(k + 1) == Some(&'/') {
                    depth -= 1;
                    blank(&mut out, chars[k]);
                    blank(&mut out, chars[k + 1]);
                    k += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, chars[k]);
                    k += 1;
                }
            }
        } else if c == '"' && next == Some('"') && chars.get(k + 2) == Some(&'"') {
            out.push_str("\"\"\"");
            k += 3;
            while k < chars.len() && !(chars[k] == '"' && chars.get(k + 1) == Some(&'"') && chars.get(k + 2) == Some(&'"')) {
                blank(&mut out, chars[k]);
                k += 1;
            }
            out.push_str(&"\"".repeat((chars.len() - k).min(3)));
            k += 3;
        } else if c == '"' || c == '\'' {
            out.push(c);
            k += 1;
            while k < chars.len() && chars[k] != c && chars[k] != '\n' {
                if chars[k] == '\\' {
                    blank(&mut out, chars[k]);
                    k += 1;
                    if k < chars.len() {
                        blank(&mut out, chars[k]);
                        k += 1;
                    }
                    continue;
                }
                blank(&mut out, chars[k]);
                k += 1;
            }
            if k < chars.len() {
                out.push(chars[k]);
                k += 1;
            }
        } else {
            out.push(c);
            k += 1;
        }
    }
    out
}

/// The `package` of a file, from its code without comments.
fn package_of(code: &str) -> String {
    for line in code.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("package ") {
            return rest.trim().trim_end_matches(';').replace('`', "").split_whitespace().collect();
        }
        if line.starts_with("import ") || line.starts_with("fun ") || line.starts_with("class ") {
            break;
        }
    }
    String::new()
}

/// The name `@file:JvmName("...")` gives the file's facade class.
fn jvm_name(text: &str) -> Option<String> {
    let at = text.find("@file:JvmName(")?;
    let rest = &text[at + "@file:JvmName(".len()..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(rest[start..end].to_owned())
}

/// The facade class Kotlin compiles a file's top-level functions into: `main.kt` holds `MainKt`.
fn facade_name(file: &Path) -> String {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut name: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if let Some(first) = name.chars().next() {
        name.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    }
    format!("{name}Kt")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"package com.example.app

import androidx.compose.runtime.Composable
import androidx.compose.ui.tooling.preview.Preview

// @Preview fun Commented() {}
/* @Preview /* nested */ fun AlsoCommented() {} */

@Preview
@Composable
fun AppPreview() {
    val text = "@Preview fun InString() {}"
    App()
}

@Composable
@androidx.compose.desktop.ui.tooling.preview.Preview(showBackground = true)
private fun DarkPreview() = App()

@Preview
@Composable
fun WithParameter(name: String) {}

@Composable
fun NotAPreview() {}

class Screens {
    @Preview
    @Composable
    fun Member() {}
}
"#;

    #[test]
    fn finds_top_level_previews_without_parameters() {
        let previews = find_in(Path::new("src/main.kt"), SOURCE);
        let names: Vec<&str> = previews.iter().map(|p| p.function.as_str()).collect();
        assert_eq!(names, ["AppPreview", "DarkPreview"]);
        let app = &previews[0];
        assert_eq!(app.class, "com.example.app.MainKt");
        assert_eq!(app.qualified_name(), "com.example.app.AppPreview");
        assert_eq!(app.line, 11);
        assert!(app.matches("AppPreview") && app.matches("com.example.app.AppPreview") && !app.matches("app.AppPreview"));
    }

    #[test]
    fn the_facade_class_follows_the_file_and_jvm_name() {
        assert_eq!(find_in(Path::new("Screens.kt"), "@Preview\n@Composable\nfun P() {}\n")[0].class, "ScreensKt");
        let named = "@file:JvmName(\"Previews\")\npackage a.b\n\n@Preview @Composable fun P() {}\n";
        assert_eq!(find_in(Path::new("x.kt"), named)[0].class, "a.b.Previews");
        assert_eq!(facade_name(Path::new("my-screen.kt")), "My_screenKt");
    }
}
