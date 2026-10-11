//! Finding a program's entry point by scanning Kotlin sources for a
//! top-level `fun main`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// A top-level `main` function found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPoint {
    pub file: PathBuf,
    /// Package of the file (empty for the default package).
    pub package: String,
    /// JVM facade class holding `main`, e.g. `com.example.MainKt`.
    pub jvm_class: String,
}

impl EntryPoint {
    /// Kotlin/Native `-entry` value, e.g. `com.example.main`.
    pub fn native_entry(&self) -> String {
        if self.package.is_empty() { "main".to_owned() } else { format!("{}.main", self.package) }
    }
}

/// Strips `//` line comments and `/* */` block comments (good enough for
/// locating declarations; string literals containing comment markers are rare
/// at the top level).
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut depth = 0usize;
    while let Some(c) = chars.next() {
        if depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                depth -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                depth += 1;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            depth = 1;
        } else {
            out.push(c);
        }
    }
    out
}

/// The JVM facade class name Kotlin generates for a file.
pub fn facade_class_name(file: &Path, src: &str) -> String {
    for line in src.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@file:JvmName(") {
            if let Some(name) = rest.trim().strip_prefix('"').and_then(|r| r.split('"').next()) {
                return name.to_owned();
            }
        }
    }
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut name: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if let Some(first) = name.get(0..1) {
        let upper = first.to_uppercase();
        name.replace_range(0..1, &upper);
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    format!("{name}Kt")
}

fn package_of(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("package "))
        .map(|p| p.trim().trim_end_matches(';').replace('`', ""))
        .unwrap_or_default()
}

/// Whether the source declares a top-level `main` function.
fn has_top_level_main(src: &str) -> bool {
    let mut depth: i32 = 0;
    for line in src.lines() {
        if depth == 0 {
            let trimmed = line.trim_start();
            // Allow modifiers like `suspend` or annotations on the same line.
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            if let Some(pos) = words.iter().position(|w| *w == "fun") {
                let before_ok = words[..pos].iter().all(|w| w.starts_with('@') || matches!(*w, "suspend" | "public" | "internal"));
                let after = words[pos + 1..].join(" ");
                if before_ok && (after.starts_with("main(") || after.starts_with("main (")) {
                    return true;
                }
            }
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
    false
}

/// Scans `files` for entry points.
pub fn find_entry_points(files: &[PathBuf]) -> Vec<EntryPoint> {
    files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "kt"))
        .filter_map(|f| {
            let raw = std::fs::read_to_string(f).ok()?;
            let src = strip_comments(&raw);
            has_top_level_main(&src).then(|| {
                let package = package_of(&src);
                let class = facade_class_name(f, &src);
                let jvm_class = if package.is_empty() { class } else { format!("{package}.{class}") };
                EntryPoint { file: f.clone(), package, jvm_class }
            })
        })
        .collect()
}

/// Finds exactly one entry point or explains how to choose.
pub fn find_single_entry_point(files: &[PathBuf], setting: &str) -> Result<EntryPoint> {
    let mut found = find_entry_points(files);
    match found.len() {
        0 => bail!("no `fun main` found in the sources; add one or set `{setting}` in kargo.toml"),
        1 => Ok(found.remove(0)),
        _ => {
            let list: Vec<String> = found.iter().map(|e| format!("  {} ({})", e.jvm_class, e.file.display())).collect();
            bail!("multiple `fun main` found; set `{setting}` in kargo.toml to pick one:\n{}", list.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        p
    }

    #[test]
    fn facade_names() {
        assert_eq!(facade_class_name(Path::new("main.kt"), ""), "MainKt");
        assert_eq!(facade_class_name(Path::new("my-app.kt"), ""), "My_appKt");
        assert_eq!(facade_class_name(Path::new("App.kt"), "@file:JvmName(\"Launcher\")\npackage x"), "Launcher");
        assert_eq!(facade_class_name(Path::new("1st.kt"), ""), "_1stKt");
    }

    #[test]
    fn finds_main_with_package() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(dir.path(), "Main.kt", "package com.example.app\n\nfun main(args: Array<String>) {\n  println(1)\n}\n");
        let e = find_single_entry_point(&[f], "main-class").unwrap();
        assert_eq!(e.jvm_class, "com.example.app.MainKt");
        assert_eq!(e.native_entry(), "com.example.app.main");
    }

    #[test]
    fn ignores_non_top_level_and_commented_mains() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.kt", "class A {\n    fun main() {}\n}\n// fun main() {}\n/* fun main() {} */\nfun mainly() {}\n");
        let b = write(dir.path(), "b.kt", "object B { @JvmStatic fun main(args: Array<String>) {} }\n");
        assert!(find_entry_points(&[a, b]).is_empty());
    }

    #[test]
    fn suspend_and_default_package() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(dir.path(), "app.kt", "suspend fun main() = coroutineScope { }\n");
        let e = find_single_entry_point(&[f], "x").unwrap();
        assert_eq!(e.jvm_class, "AppKt");
        assert_eq!(e.native_entry(), "main");
    }

    #[test]
    fn ambiguity_and_absence_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.kt", "fun main() {}\n");
        let b = write(dir.path(), "b.kt", "fun main() {}\n");
        let err = find_single_entry_point(&[a, b], "main-class").unwrap_err().to_string();
        assert!(err.contains("multiple"), "{err}");
        let err = find_single_entry_point(&[], "main-class").unwrap_err().to_string();
        assert!(err.contains("no `fun main`"), "{err}");
    }

    #[test]
    fn nested_block_comments() {
        assert_eq!(strip_comments("a /* x /* y */ z */ b"), "a  b");
        assert_eq!(strip_comments("a // c\nb"), "a \nb");
    }
}
