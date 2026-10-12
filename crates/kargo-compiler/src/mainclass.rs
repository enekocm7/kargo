//! Finding a program's entry point by scanning Kotlin sources for a
//! top-level `fun main`, or (on the JVM) a `@JvmStatic fun main` in an
//! `object` or `companion object`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// How a `main` function is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A top-level `fun main`; usable on every target.
    TopLevel,
    /// A `@JvmStatic fun main` in an `object` or `companion object`; JVM only.
    JvmStatic,
}

/// A `main` function found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPoint {
    pub file: PathBuf,
    /// Package of the file (empty for the default package).
    pub package: String,
    /// JVM class holding `main`, e.g. `com.example.MainKt` or `com.example.Cli`.
    pub jvm_class: String,
    pub kind: EntryKind,
    /// `main` declares a parameter (`args: Array<String>`).
    pub takes_args: bool,
    pub suspend: bool,
}

impl EntryPoint {
    /// Kotlin/Native `-entry` value, e.g. `com.example.main`.
    pub fn native_entry(&self) -> String {
        if self.package.is_empty() { "main".to_owned() } else { format!("{}.main", self.package) }
    }
}

/// Length in chars of the string or char literal starting at `i`, or 0 if
/// none starts there. String templates are not parsed.
fn literal_len(s: &[char], i: usize) -> usize {
    const RAW: [char; 3] = ['"', '"', '"'];
    let end = match s[i] {
        '"' if s[i..].starts_with(&RAW) => {
            let mut j = i + 3;
            while j < s.len() && !s[j..].starts_with(&RAW) {
                j += 1;
            }
            j = (j + 3).min(s.len());
            while j < s.len() && s[j] == '"' {
                j += 1;
            }
            j
        }
        q @ ('"' | '\'') => {
            let mut j = i + 1;
            while j < s.len() && s[j] != q && s[j] != '\n' {
                j += if s[j] == '\\' { 2 } else { 1 };
            }
            if j < s.len() && s[j] == q { j + 1 } else { j.min(s.len()) }
        }
        _ => return 0,
    };
    end - i
}

/// Strips `//` line comments and `/* */` block comments, leaving string and
/// char literals intact.
fn strip_comments(src: &str) -> String {
    let s: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(&['/', '/']) {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if s[i..].starts_with(&['/', '*']) {
            let mut depth = 0usize;
            while i < s.len() {
                if s[i..].starts_with(&['/', '*']) {
                    depth += 1;
                    i += 2;
                } else if s[i..].starts_with(&['*', '/']) {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if s[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
        } else {
            let n = literal_len(&s, i).max(1);
            out.extend(&s[i..i + n]);
            i += n;
        }
    }
    out
}

/// The JVM facade class name Kotlin generates for a file.
pub fn facade_class_name(file: &Path, src: &str) -> String {
    for line in src.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("@file:JvmName(")
            && let Some(name) = rest.trim().strip_prefix('"').and_then(|r| r.split('"').next())
        {
            return name.to_owned();
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

/// What a `{ ... }` block is, as far as finding `main` is concerned.
enum Scope {
    /// The top level of the file.
    File,
    /// A class, interface or object body; `binary` is its JVM binary name
    /// relative to the package, e.g. `Outer$Inner`.
    Type { binary: String, object: bool },
    /// A companion object body; its static members go to the enclosing class.
    Companion { owner: String },
    /// Anything else: function bodies, lambdas, local or anonymous classes.
    Other,
}

/// A block being scanned.
struct Body {
    scope: Scope,
    /// The block's own text without nested blocks; block boundaries and `;` become newlines.
    direct: String,
    /// The text since the last block boundary or `;`, i.e. the header of the next nested block.
    header: String,
}

impl Body {
    fn new(scope: Scope) -> Self {
        Body { scope, direct: String::new(), header: String::new() }
    }

    fn push(&mut self, text: &str) {
        self.direct.push_str(text);
        self.header.push_str(text);
    }

    fn boundary(&mut self) {
        self.direct.push('\n');
        self.header.clear();
    }
}

/// Decides what the block opened after `header` is, given its parent.
fn classify(header: &str, parent: &Scope) -> Scope {
    let parent_binary = match parent {
        Scope::File => "",
        Scope::Type { binary, .. } => binary.as_str(),
        _ => return Scope::Other,
    };
    let words: Vec<&str> = header.split_whitespace().collect();
    let Some(i) = words.iter().rposition(|w| matches!(*w, "object" | "class" | "interface")) else {
        return Scope::Other;
    };
    // A body-less declaration such as `data object Empty`, followed by a function.
    if words[i + 1..].iter().any(|w| matches!(*w, "fun" | "init")) {
        return Scope::Other;
    }
    if words[i] == "object" && i > 0 && words[i - 1] == "companion" {
        return match parent {
            Scope::Type { binary, object: false } => Scope::Companion { owner: binary.clone() },
            _ => Scope::Other,
        };
    }
    let name: String = words
        .get(i + 1)
        .map(|w| w.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '`')).filter(|c| *c != '`').collect())
        .unwrap_or_default();
    if name.is_empty() {
        return Scope::Other; // `object : Runnable { ... }`
    }
    let binary = if parent_binary.is_empty() { name } else { format!("{parent_binary}${name}") };
    Scope::Type { binary, object: words[i] == "object" }
}

/// How a `main` function is declared: whether it takes `args` and is `suspend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Signature {
    takes_args: bool,
    suspend: bool,
}

/// Whether the parameter list of the `fun main` declared in `text` is non-empty.
fn has_parameters(text: &str) -> bool {
    let Some(fun) = text.find("fun") else { return false };
    let Some(main) = text[fun..].find("main").map(|i| fun + i) else { return false };
    let Some(open) = text[main..].find('(').map(|i| main + i) else { return false };
    !text[open + 1..].trim_start().starts_with(')')
}

/// The `fun main` a block's own text declares; with `jvm_static`, only an
/// `@JvmStatic` one counts.
fn declares_main(direct: &str, jvm_static: bool) -> Option<Signature> {
    let mut annotations: Vec<&str> = Vec::new();
    let mut offset = 0;
    for line in direct.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        // Annotations on lines of their own apply to the next declaration.
        if words.iter().all(|w| w.starts_with('@')) {
            annotations.extend(&words);
            continue;
        }
        let modifiers = std::mem::take(&mut annotations);
        let Some(pos) = words.iter().position(|w| *w == "fun") else { continue };
        let after = words[pos + 1..].join(" ");
        if !(after.starts_with("main(") || after.starts_with("main (")) {
            continue;
        }
        let mut before = modifiers.iter().chain(&words[..pos]);
        if !before.clone().all(|w| w.starts_with('@') || matches!(*w, "suspend" | "public" | "internal")) {
            continue;
        }
        let suspend = before.clone().any(|w| *w == "suspend");
        if !jvm_static || before.any(|w| matches!(w.split('(').next(), Some("@JvmStatic" | "@kotlin.jvm.JvmStatic"))) {
            // The parameters may continue on the following lines.
            return Some(Signature { takes_args: has_parameters(&direct[start..]), suspend });
        }
    }
    None
}

/// Finds the `main` functions in comment-stripped source. The class is `None`
/// for a top-level `main` (it lives in the file facade) and otherwise the JVM
/// binary name relative to the package.
fn scan_mains(src: &str) -> Vec<(EntryKind, Option<String>, Signature)> {
    fn collect(body: &Body, found: &mut Vec<(EntryKind, Option<String>, Signature)>) {
        match &body.scope {
            Scope::File => {
                if let Some(sig) = declares_main(&body.direct, false) {
                    found.push((EntryKind::TopLevel, None, sig));
                }
            }
            Scope::Type { binary: class, object: true } | Scope::Companion { owner: class } => {
                if let Some(sig) = declares_main(&body.direct, true) {
                    found.push((EntryKind::JvmStatic, Some(class.clone()), sig));
                }
            }
            _ => {}
        }
    }

    let s: Vec<char> = src.chars().collect();
    let mut stack = vec![Body::new(Scope::File)];
    let mut found = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let top = stack.last_mut().expect("the file body is never popped");
        let n = literal_len(&s, i);
        if n > 0 {
            top.push("\"\"");
            i += n;
            continue;
        }
        match s[i] {
            '{' => {
                let scope = classify(&top.header, &top.scope);
                top.boundary();
                stack.push(Body::new(scope));
            }
            '}' => {
                if stack.len() > 1 {
                    let body = stack.pop().expect("checked above");
                    collect(&body, &mut found);
                }
                stack.last_mut().expect("the file body is never popped").boundary();
            }
            ';' => top.boundary(),
            c => top.push(c.encode_utf8(&mut [0; 4])),
        }
        i += 1;
    }
    // Unbalanced braces leave blocks open; still look at them.
    for body in stack.iter().rev() {
        collect(body, &mut found);
    }
    found
}

/// Reads a source file as text, dropping the UTF-8 byte-order mark that
/// Windows editors put at the start of a file. The Kotlin compiler ignores it,
/// but `str::trim` doesn't, so it would hide a leading `package` or `@file:`.
fn read_source(file: &Path) -> Option<String> {
    let mut text = std::fs::read_to_string(file).ok()?;
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    Some(text)
}

/// Scans `files` for entry points.
pub fn find_entry_points(files: &[PathBuf]) -> Vec<EntryPoint> {
    files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "kt"))
        .flat_map(|f| {
            let Some(raw) = read_source(f) else { return Vec::new() };
            let src = strip_comments(&raw);
            let package = package_of(&src);
            scan_mains(&src)
                .into_iter()
                .map(|(kind, class, sig)| {
                    let class = class.unwrap_or_else(|| facade_class_name(f, &src));
                    let jvm_class = if package.is_empty() { class } else { format!("{package}.{class}") };
                    EntryPoint {
                        file: f.clone(),
                        package: package.clone(),
                        jvm_class,
                        kind,
                        takes_args: sig.takes_args,
                        suspend: sig.suspend,
                    }
                })
                .collect()
        })
        .collect()
}

/// Picks the only entry point or explains how to choose one: `flag` is the
/// `kargo run` option that names one, `name` how it is shown in that option.
fn single(mut found: Vec<EntryPoint>, setting: &str, flag: &str, name: fn(&EntryPoint) -> String, none: &str) -> Result<EntryPoint> {
    match found.len() {
        0 => bail!("{none}, or set `{setting}` in kargo.toml"),
        1 => Ok(found.remove(0)),
        _ => {
            let list: Vec<String> = found.iter().map(|e| format!("  {} ({})", name(e), e.file.display())).collect();
            bail!(
                "found multiple `main` functions; pick one with `kargo run {flag}`, declare each as a [[bin]], \
                 or set `{setting}` in kargo.toml:\n{}",
                list.join("\n")
            )
        }
    }
}

/// Finds exactly one top-level `fun main` (the only kind Kotlin/Native can
/// use) or explains how to choose.
pub fn find_single_entry_point(files: &[PathBuf], setting: &str) -> Result<EntryPoint> {
    let found = find_entry_points(files).into_iter().filter(|e| e.kind == EntryKind::TopLevel).collect();
    single(found, setting, "--entry-point <function>", EntryPoint::native_entry, "no top-level `fun main` found in the sources; add one")
}

/// Finds the JVM main class: the class of exactly one top-level `fun main` or
/// `@JvmStatic fun main` in an object or companion object.
pub fn find_jvm_main_class(files: &[PathBuf], setting: &str) -> Result<String> {
    let none = "no `main` function found in the sources; add a top-level `fun main` or a `@JvmStatic fun main` in an object";
    Ok(single(find_entry_points(files), setting, "--main <class>", |e| e.jvm_class.clone(), none)?.jvm_class)
}

/// Finds the one `main` function declared in `file` (a `[[bin]]` `path` or an
/// example); `native` only accepts a top-level `fun main`.
pub fn find_entry_point_in(file: &Path, native: bool) -> Result<EntryPoint> {
    if !file.is_file() {
        bail!("{} does not exist", file.display());
    }
    let mut found: Vec<EntryPoint> =
        find_entry_points(&[file.to_path_buf()]).into_iter().filter(|e| !native || e.kind == EntryKind::TopLevel).collect();
    match found.len() {
        0 if native => bail!("no top-level `fun main` found in {}; add one", file.display()),
        0 => bail!("no `main` function found in {}; add a top-level `fun main` or a `@JvmStatic fun main` in an object", file.display()),
        1 => Ok(found.remove(0)),
        _ => bail!("{} declares more than one `main` function; keep one per file", file.display()),
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

    fn classes(src: &str) -> Vec<(EntryKind, Option<String>)> {
        scan_mains(&strip_comments(src)).into_iter().map(|(kind, class, _)| (kind, class)).collect()
    }

    fn signatures(src: &str) -> Vec<(bool, bool)> {
        scan_mains(&strip_comments(src)).into_iter().map(|(_, _, s)| (s.takes_args, s.suspend)).collect()
    }

    #[test]
    fn main_signatures() {
        assert_eq!(signatures("fun main() {}"), [(false, false)]);
        assert_eq!(signatures("fun main( ) = Unit"), [(false, false)]);
        assert_eq!(signatures("fun main(args: Array<String>) {}"), [(true, false)]);
        assert_eq!(signatures("suspend fun main(vararg args: String) {}"), [(true, true)]);
        assert_eq!(signatures("@Suppress(\"x\")\nsuspend fun main() {}"), [(false, true)]);
        assert_eq!(signatures("public fun main(\n    args: Array<String>,\n) {}"), [(true, false)]);
        assert_eq!(signatures("object O { @JvmStatic fun main(a: Array<String>) {} }"), [(true, false)]);
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
        let e = find_single_entry_point(std::slice::from_ref(&f), "main-class").unwrap();
        assert_eq!(e.jvm_class, "com.example.app.MainKt");
        assert_eq!(e.native_entry(), "com.example.app.main");
        assert_eq!(find_jvm_main_class(&[f], "main-class").unwrap(), "com.example.app.MainKt");
    }

    #[test]
    fn ignores_non_top_level_and_commented_mains() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.kt", "class A {\n    fun main() {}\n}\n// fun main() {}\n/* fun main() {} */\nfun mainly() {}\n");
        let b = write(
            dir.path(),
            "b.kt",
            "object B { fun main(args: Array<String>) {} }\nfun f() { val o = object { @JvmStatic fun main() {} } }\n",
        );
        assert!(find_entry_points(&[a, b]).is_empty());
    }

    #[test]
    fn jvm_static_main_in_object() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(
            dir.path(),
            "Cli.kt",
            "package m\nobject Cli { @JvmStatic fun main(args: Array<String>) { println(\"object main\") } }\n",
        );
        assert_eq!(find_jvm_main_class(std::slice::from_ref(&f), "main-class").unwrap(), "m.Cli");
        // Kotlin/Native can't use it.
        let err = find_single_entry_point(&[f], "entry-point").unwrap_err().to_string();
        assert!(err.contains("no top-level `fun main`"), "{err}");
    }

    #[test]
    fn jvm_static_main_in_companion_and_nested_object() {
        let companion =
            "class App(val x: Int) {\n  companion object {\n    @JvmStatic\n    fun main(args: Array<String>) = println(\"}\")\n  }\n}\n";
        assert_eq!(classes(companion), vec![(EntryKind::JvmStatic, Some("App".to_owned()))]);
        let named = "class App { companion object Factory { @kotlin.jvm.JvmStatic fun main(args: Array<String>) {} } }";
        assert_eq!(classes(named), vec![(EntryKind::JvmStatic, Some("App".to_owned()))]);
        let nested = "class Outer {\n  object Inner : Runnable {\n    override fun run() {}\n    @JvmStatic fun main(args: Array<String>) {}\n  }\n}";
        assert_eq!(classes(nested), vec![(EntryKind::JvmStatic, Some("Outer$Inner".to_owned()))]);
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
    fn braces_and_comment_markers_in_strings() {
        let src = "val url = \"http://x/{\"\nval c = '{'\nval raw = \"\"\"\n}\n\"\"\"\nfun main() {}\n";
        assert_eq!(classes(src), vec![(EntryKind::TopLevel, None)]);
    }

    #[test]
    fn ambiguity_and_absence_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        let a = write(dir.path(), "Main.kt", "package m\nfun main() {}\n");
        let b = write(dir.path(), "other/Other.kt", "package m.other\nfun main() {}\n");
        let err = find_jvm_main_class(&[a.clone(), b.clone()], "[target.jvm] main-class").unwrap_err().to_string();
        assert!(err.starts_with("found multiple `main` functions; pick one with `kargo run --main <class>`"), "{err}");
        assert!(err.contains("set `[target.jvm] main-class`") && err.contains("[[bin]]"), "{err}");
        assert!(err.contains("m.MainKt") && err.contains("m.other.OtherKt"), "{err}");
        let err = find_single_entry_point(&[a, b], "[target.native] entry-point").unwrap_err().to_string();
        assert!(err.contains("`kargo run --entry-point <function>`"), "{err}");
        assert!(err.contains("  m.main (") && err.contains("  m.other.main ("), "{err}");
        let err = find_jvm_main_class(&[], "main-class").unwrap_err().to_string();
        assert!(err.contains("no `main` function found"), "{err}");
        let err = find_single_entry_point(&[], "main-class").unwrap_err().to_string();
        assert!(err.contains("no top-level `fun main`"), "{err}");
    }

    #[test]
    fn entry_point_in_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(
            dir.path(),
            "Tool.kt",
            "package t
fun main() {}
",
        );
        assert_eq!(find_entry_point_in(&a, true).unwrap().native_entry(), "t.main");
        assert_eq!(find_entry_point_in(&a, false).unwrap().jvm_class, "t.ToolKt");
        let cli = write(
            dir.path(),
            "Cli.kt",
            "package t
object Cli { @JvmStatic fun main(args: Array<String>) {} }
",
        );
        assert_eq!(find_entry_point_in(&cli, false).unwrap().jvm_class, "t.Cli");
        let err = find_entry_point_in(&cli, true).unwrap_err().to_string();
        assert!(err.contains("no top-level `fun main`"), "{err}");
        let two = write(
            dir.path(),
            "Two.kt",
            "package t
fun main() {}
object O { @JvmStatic fun main(args: Array<String>) {} }
",
        );
        assert!(find_entry_point_in(&two, false).unwrap_err().to_string().contains("more than one"));
        assert!(find_entry_point_in(&dir.path().join("Missing.kt"), false).unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn utf8_bom_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let bom = "\u{feff}";
        let plain = write(dir.path(), "Main.kt", &format!("{bom}package enc\nfun main() {{ println(\"hi\") }}\n"));
        let e = find_single_entry_point(std::slice::from_ref(&plain), "main-class").unwrap();
        assert_eq!((e.package.as_str(), e.jvm_class.as_str()), ("enc", "enc.MainKt"));
        assert_eq!(e.native_entry(), "enc.main");

        let crlf = write(dir.path(), "Crlf.kt", &format!("{bom}package enc\r\n\r\nfun main() {{}}\r\n"));
        assert_eq!(find_entry_point_in(&crlf, false).unwrap().jvm_class, "enc.CrlfKt");

        let named = write(dir.path(), "App.kt", &format!("{bom}@file:JvmName(\"Launcher\")\npackage enc\nfun main() {{}}\n"));
        assert_eq!(find_entry_point_in(&named, false).unwrap().jvm_class, "enc.Launcher");

        let no_package = write(dir.path(), "Bare.kt", &format!("{bom}fun main() {{}}\n"));
        assert_eq!(find_entry_point_in(&no_package, false).unwrap().jvm_class, "BareKt");

        let object = write(dir.path(), "Cli.kt", &format!("{bom}package enc\nobject Cli {{ @JvmStatic fun main() {{}} }}\n"));
        assert_eq!(find_entry_point_in(&object, false).unwrap().jvm_class, "enc.Cli");
    }

    #[test]
    fn nested_block_comments() {
        assert_eq!(strip_comments("a /* x /* y */ z */ b"), "a  b");
        assert_eq!(strip_comments("a // c\nb"), "a \nb");
        assert_eq!(strip_comments("\"a // b\" // c"), "\"a // b\" ");
    }
}
